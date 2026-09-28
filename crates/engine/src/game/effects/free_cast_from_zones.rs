use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::{CastOfferKind, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Complete, already-frozen input for opening a resolution-scoped free-cast
/// window.  Keeping the policy and its window budget in one value prevents a
/// caller from accidentally replacing policy provenance while forwarding the
/// other pause-state fields.
pub(crate) struct FreeCastWindowRequest {
    pub(crate) count: Option<u8>,
    pub(crate) max_total_mv: Option<u32>,
    pub(crate) zones: Vec<Zone>,
    pub(crate) graveyard_replacement:
        Option<crate::types::ability::SpellStackToGraveyardReplacement>,
    pub(crate) face_policy: crate::types::ability::ResolutionCastFacePolicy,
}

/// Build the exact request for one free cast from a current window.
///
/// CR 608.2g + CR 118.9b: an effect may let a player cast a spell during its
/// resolution without paying its mana cost as an alternative cost.
///
/// The cleanup retains the window's re-offer authority, while the request
/// itself deliberately carries no graveyard rider: `FreeCastOfferRemaining`
/// installs that one rider after the cast finalizes.
pub(in crate::game) fn free_cast_window_resolution_request(
    controller: PlayerId,
    remaining_casts: Option<u8>,
    remaining_mv_budget: Option<u32>,
    face_policy: crate::types::ability::ResolutionCastFacePolicy,
    zones: Vec<Zone>,
    graveyard_replacement: Option<crate::types::ability::SpellStackToGraveyardReplacement>,
    member_pool: Vec<ObjectId>,
) -> crate::game::casting::ResolutionCastRequest {
    let cleanup = crate::types::ability::ResolutionCastCleanup {
        source_id: face_policy.source_id,
        offer_id: None,
        face_policy: face_policy.clone(),
        exiled_misses: Vec::new(),
        reject_action: crate::types::ability::ResolutionMvRejectAction::RemainExiled,
        success_action:
            crate::types::ability::ResolutionCastSuccessAction::FreeCastOfferRemaining {
                controller,
                remaining_casts,
                remaining_mv_budget,
                face_policy: Box::new(face_policy.clone()),
                zones,
                graveyard_replacement,
                member_pool,
            },
        delayed_trigger_receipts: Vec::new(),
    };
    crate::game::casting::ResolutionCastRequest {
        face_policy,
        cast_transformed: false,
        cleanup,
        graveyard_replacement: None,
        cost: crate::types::ability::ResolutionCastCost::Free,
    }
}

/// CR 608.2g + CR 601.2 + CR 118.9: Open an interactive free-cast window.
///
/// The controller may cast up to `count` spells matching `filter` from their
/// own graveyard and/or hand (`zones`) — or ANY NUMBER of them when `count` is
/// `None`, the unbounded "any number of spells" form whose only bound is
/// candidate exhaustion — each without paying its mana cost,
/// casting them one at a time during this resolution (CR 608.2g). When
/// `max_total_mv` is `Some(n)`, the *running total* mana value of the spells
/// cast this way must not exceed `n` (CR 202.3); the engine handler shrinks the
/// budget after each cast and re-filters the candidate list.
///
/// The resolver only computes the initial candidate set and sets the
/// `WaitingFor::CastOffer { FreeCastWindow }` pause. The accept/decline loop —
/// casting each chosen spell via `initiate_cast_during_resolution`, decrementing
/// the count and budget, and re-offering — lives in `engine_resolution_choices`,
/// matching the Cascade/Discover/Ripple pattern. The "Exile ~" sub-ability is
/// stashed as a `pending_continuation` and runs after the window finishes.
///
/// Invoke Calamity is the type specimen. The optional CR 614.1a destination
/// rider is carried on the offer so each cast spell is stamped with it.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, max_total_mv, filter, zones, graveyard_replacement) = match &ability.effect {
        Effect::FreeCastFromZones {
            count,
            max_total_mv,
            filter,
            zones,
            graveyard_replacement,
        } => (
            *count,
            *max_total_mv,
            filter.clone(),
            zones.clone(),
            graveyard_replacement.clone(),
        ),
        _ => return Err(EffectError::MissingParam("FreeCastFromZones".to_string())),
    };
    let face_policy = crate::types::ability::ResolutionCastFacePolicy::new(
        crate::game::effects::cast_from_zone::freeze_resolution_cast_filter(
            state, ability, filter, None,
        ),
        ability.source_id,
        ability.controller,
        None,
    );
    resolve_with_face_policy(
        state,
        ability,
        FreeCastWindowRequest {
            count,
            max_total_mv,
            zones,
            graveyard_replacement,
            face_policy,
        },
        events,
    )
}

/// Open a free-cast window using a policy fixed by the caller while its
/// resolution context is still live.  `CastFromZone` uses this for a frozen
/// constraint; ordinary `FreeCastFromZones` calls [`resolve`] above.
pub(crate) fn resolve_with_face_policy(
    state: &mut GameState,
    ability: &ResolvedAbility,
    request: FreeCastWindowRequest,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let FreeCastWindowRequest {
        count,
        max_total_mv,
        zones,
        graveyard_replacement,
        face_policy,
    } = request;

    // CR 603.3a: Resolve the acting player from the ability's controller (the
    // resolving spell's controller). Invoke Calamity grants the window to its
    // own controller — "you may cast ... from your graveyard and/or hand".
    let controller = ability.controller;
    if !state.players.iter().any(|p| p.id == controller) {
        return Err(EffectError::PlayerNotFound);
    }

    // CR 607.2a + CR 608.2g: When this window is the continuation of a
    // `ChooseFromZone` over "cards exiled this way" (Plargg and Nassari), the
    // answer handler forwards the choose's FULL candidate pool — THIS
    // resolution's typed exile batch — as this ability's object targets. That
    // concrete member pool confines the offer to the current resolution's
    // batch: `ExiledBySource` alone reads the source's complete live
    // linked-exile ledger, which would wrongly re-offer a linked nonland card
    // left in exile by a PREVIOUS resolution. Empty (Invoke Calamity's
    // graveyard/hand window — no choose head) means no batch restriction.
    let member_pool: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            crate::types::ability::TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .collect();

    // A member pool has already consumed any player-target leg from its parent
    // resolution.  Freeze that residual form into the one policy carried by
    // this window so initial enumeration and every re-offer evaluate exactly
    // the same authority.
    let face_policy = if member_pool.is_empty() {
        face_policy
    } else {
        crate::types::ability::ResolutionCastFacePolicy::new(
            member_pool_filter(&face_policy.filter),
            face_policy.source_id,
            face_policy.controller,
            face_policy.constraint,
        )
    };

    let cast_request = free_cast_window_resolution_request(
        controller,
        count,
        max_total_mv,
        face_policy.clone(),
        zones.clone(),
        graveyard_replacement.clone(),
        member_pool.clone(),
    );
    let candidates = eligible_candidates(state, &zones, max_total_mv, &member_pool, &cast_request);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::FreeCastFromZones,
        source_id: ability.source_id,
        subject: None,
    });

    // CR 601.2: "Up to N" — with no eligible candidate the window opens to zero
    // legal casts, so skip the pause entirely and let the continuation (Exile ~)
    // run. The handler's decline path produces the same outcome, but short-
    // circuiting here avoids a no-op prompt.
    //
    // `count == Some(0)` is the printed-zero case only. `None` is the unbounded
    // "any number of spells" form and must NOT short-circuit: its
    // bound is the candidate list, which the emptiness check above already
    // covers.
    if candidates.is_empty() || count == Some(0) {
        return Ok(());
    }

    state.waiting_for = WaitingFor::CastOffer {
        player: controller,
        kind: CastOfferKind::FreeCastWindow {
            candidates,
            remaining_casts: count,
            remaining_mv_budget: max_total_mv,
            face_policy,
            zones,
            graveyard_replacement,
            member_pool,
        },
    };

    Ok(())
}

/// CR 601.2a + CR 202.3: Gather the cards in `zones` that match `filter` and
/// (when `max_total_mv` is `Some`) whose mana value fits the remaining budget.
/// Shared by the resolver and the handler's re-offer loop so the candidate set
/// stays consistent across both entry points.
///
/// `member_pool`, when non-empty, is THIS resolution's concrete approved pool:
/// candidates are enumerated from those exact ids, rather than from a zone scan.
/// It originally served "exiled this way" batches, and also carries paired
/// opponent-graveyard targets into the same window. In the latter case the
/// player target has intentionally been consumed before this resolver runs, so
/// `TargetPlayer`/`Owned(TargetPlayer)` are not evaluated again; zone, type,
/// cast legality, and every unrelated filter property remain authoritative.
/// Empty retains Invoke Calamity's existing controller-zone enumeration.
/// `request` is the exact free resolution-cast transaction that will be used
/// for a chosen candidate. Its policy, cleanup, payment shape, and rider must
/// all survive the clone projection unchanged.
pub(in crate::game) fn eligible_candidates(
    state: &GameState,
    zones: &[Zone],
    max_total_mv: Option<u32>,
    member_pool: &[ObjectId],
    request: &crate::game::casting::ResolutionCastRequest,
) -> Vec<ObjectId> {
    let face_policy = &request.face_policy;
    let controller = face_policy.controller;
    let Some(player) = state.players.iter().find(|p| p.id == controller) else {
        return Vec::new();
    };
    // One immutable, layer-flushed baseline serves this entire enumeration.
    // Each card/face probe below derives its own projection, so neither a
    // rejected candidate nor a swapped face can contaminate a later candidate.
    let projection = crate::game::casting::ResolutionCastProjection::new(state);

    let mut candidates = Vec::new();
    let candidate_ids: Vec<ObjectId> = if member_pool.is_empty() {
        let mut ids = Vec::new();
        for &zone in zones {
            let zone_ids = match zone {
                Zone::Graveyard => &player.graveyard,
                Zone::Hand => &player.hand,
                // CR 400.1 + CR 608.2g: Exile is a shared zone — the whole pile
                // is scanned and the `filter` (e.g. `ExiledBySource` +
                // `Not(InTrackedSet)`) narrows it to this resolution's linked set
                // regardless of who owns the exiled cards (Plargg and Nassari
                // exiles from EVERY player's library).
                Zone::Exile => &state.exile,
                // CR 601.2a: Other zones would need a parser/effect change, so an
                // unexpected zone contributes no candidates rather than silently
                // scanning the wrong pile.
                _ => continue,
            };
            ids.extend(zone_ids.iter().copied());
        }
        ids
    } else {
        member_pool.to_vec()
    };

    for id in candidate_ids {
        if !state
            .objects
            .get(&id)
            .is_some_and(|object| zones.contains(&object.zone))
        {
            continue;
        }
        // CR 601.2b-c + CR 608.2g: discover candidates by projecting each
        // spell face through the exact request that will be announced. A
        // policy-only front check would admit a spell with no legal targets or
        // erase a legal back-only spell before it could be elected.
        if projection
            .spell_face_legality(face_policy.controller, id, request)
            .count()
            == 0
        {
            continue;
        }
        // CR 202.3 + CR 107.3b + CR 601.2b: Respect the running MV budget.
        // Because this window casts without paying a mana cost, X can only
        // be announced as 0, so the card's printed mana_value() is the same
        // value used when the choice is submitted.
        if let Some(budget) = max_total_mv {
            // CR 202.3d + CR 709.4b: candidate cards are in a non-stack zone,
            // so a split card's budget is its combined halves. Read that MV
            // from the immutable flushed baseline, never from an earlier
            // candidate or the caller's unflushed state.
            let mv = projection.candidate_mana_value(id).unwrap_or(0);
            if mv > budget {
                continue;
            }
        }
        candidates.push(id);
    }
    candidates
}

/// CR 608.2g + CR 115.1a: A fixed member pool already proves which paired
/// player selected each object. Strip only that consumed player binding before
/// re-offering the exact ids; every type, zone, and cast-legality check remains.
fn member_pool_filter(filter: &TargetFilter) -> TargetFilter {
    use crate::types::ability::{ControllerRef, FilterProp};

    match filter {
        TargetFilter::Typed(tf) => {
            let mut tf = tf.clone();
            if tf.controller == Some(ControllerRef::TargetPlayer) {
                tf.controller = None;
            }
            tf.properties.retain(|prop| {
                !matches!(
                    prop,
                    FilterProp::Owned {
                        controller: ControllerRef::TargetPlayer
                    }
                )
            });
            TargetFilter::Typed(tf)
        }
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.iter().map(member_pool_filter).collect(),
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.iter().map(member_pool_filter).collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(member_pool_filter(filter)),
        },
        TargetFilter::TrackedSetFiltered {
            id,
            filter,
            caused_by,
        } => TargetFilter::TrackedSetFiltered {
            id: *id,
            filter: Box::new(member_pool_filter(filter)),
            caused_by: *caused_by,
        },
        TargetFilter::ChosenDamageSource { filter } => TargetFilter::ChosenDamageSource {
            filter: filter
                .as_ref()
                .map(|filter| Box::new(member_pool_filter(filter))),
        },
        unchanged @ (TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SourceController
        | TargetFilter::ControllerAndControlledPermanents { .. }
        | TargetFilter::Opponent
        | TargetFilter::SelfRef
        | TargetFilter::GrantingObject
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::PlayerMatching { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::LastZoneChanged
        | TargetFilter::CostPaidObject
        | TargetFilter::AmassedArmy
        | TargetFilter::ChosenCard
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSourceController
        | TargetFilter::EventTargetController
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::OriginalSource
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageSource
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers) => unchanged.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, FilterProp, SpellStackToGraveyardReplacement, ThisWayCause, TypeFilter,
        TypedFilter,
    };
    use crate::types::card::LayoutKind;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;

    fn instant_sorcery_filter() -> TargetFilter {
        TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
            ],
        }
    }

    fn test_face_policy(
        filter: TargetFilter,
        source_id: ObjectId,
        controller: PlayerId,
    ) -> crate::types::ability::ResolutionCastFacePolicy {
        crate::types::ability::ResolutionCastFacePolicy::new(filter, source_id, controller, None)
    }

    fn test_eligible_candidates(
        state: &GameState,
        zones: &[Zone],
        max_total_mv: Option<u32>,
        member_pool: &[ObjectId],
        face_policy: crate::types::ability::ResolutionCastFacePolicy,
    ) -> Vec<ObjectId> {
        // A pooled production window normalizes away its already-consumed
        // paired-player binding before it persists the request. Keep these
        // direct enumerator tests on that same serialized request shape.
        let face_policy = if member_pool.is_empty() {
            face_policy
        } else {
            crate::types::ability::ResolutionCastFacePolicy::new(
                member_pool_filter(&face_policy.filter),
                face_policy.source_id,
                face_policy.controller,
                face_policy.constraint,
            )
        };
        let request = free_cast_window_resolution_request(
            face_policy.controller,
            None,
            max_total_mv,
            face_policy,
            zones.to_vec(),
            None,
            member_pool.to_vec(),
        );
        eligible_candidates(state, zones, max_total_mv, member_pool, &request)
    }

    fn add_card(
        state: &mut GameState,
        owner: PlayerId,
        zone: Zone,
        core: CoreType,
        mv: u32,
    ) -> ObjectId {
        // `create_object` already files the object into the correct zone vector
        // via `add_to_zone`; only the characteristics need setting here.
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, owner, "Spell".to_string(), zone);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(core);
        obj.mana_cost = ManaCost::generic(mv);
        id
    }

    fn add_modal_spell_card(state: &mut GameState, front: CoreType, back: CoreType) -> ObjectId {
        let id = add_card(state, PlayerId(0), Zone::Graveyard, front, 1);
        let mut back_types = crate::types::card_type::CardType::default();
        back_types.core_types.push(back);
        state.objects.get_mut(&id).unwrap().back_face =
            Some(crate::game::game_object::BackFaceData {
                name: format!("Back face {id:?}"),
                card_types: back_types,
                mana_cost: ManaCost::generic(1),
                layout_kind: Some(LayoutKind::Modal),
                ..Default::default()
            });
        id
    }

    fn add_targeted_aura_card(state: &mut GameState, zone: Zone) -> ObjectId {
        let id = add_card(state, PlayerId(0), zone, CoreType::Enchantment, 1);
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.subtypes.push("Aura".to_string());
        object
            .keywords
            .push(crate::types::keywords::Keyword::Enchant(
                TargetFilter::Typed(TypedFilter::creature()),
            ));
        object.base_card_types = object.card_types.clone();
        object.base_keywords = object.keywords.clone();
        object.base_mana_cost = object.mana_cost.clone();
        id
    }

    fn add_targeted_modal_aura_card(state: &mut GameState, zone: Zone) -> ObjectId {
        let id = add_targeted_aura_card(state, zone);
        let mut back_types = crate::types::card_type::CardType::default();
        back_types.core_types.push(CoreType::Enchantment);
        back_types.subtypes.push("Aura".to_string());
        state.objects.get_mut(&id).unwrap().back_face =
            Some(crate::game::game_object::BackFaceData {
                name: format!("Targeted Aura Back {id:?}"),
                card_types: back_types,
                mana_cost: ManaCost::generic(1),
                keywords: vec![crate::types::keywords::Keyword::Enchant(
                    TargetFilter::Typed(TypedFilter::creature()),
                )],
                layout_kind: Some(LayoutKind::Modal),
                ..Default::default()
            });
        id
    }

    fn add_target_creature(state: &mut GameState) -> ObjectId {
        let id = add_card(state, PlayerId(1), Zone::Battlefield, CoreType::Creature, 1);
        let object = state.objects.get_mut(&id).unwrap();
        object.base_card_types = object.card_types.clone();
        id
    }

    #[test]
    fn candidate_projections_isolate_rejected_and_face_swapped_siblings() {
        let mut state = GameState::new_two_player(1);
        let rejected = add_card(
            &mut state,
            PlayerId(0),
            Zone::Graveyard,
            CoreType::Creature,
            1,
        );
        let back_only = add_modal_spell_card(&mut state, CoreType::Sorcery, CoreType::Instant);
        let later = add_card(
            &mut state,
            PlayerId(0),
            Zone::Graveyard,
            CoreType::Instant,
            1,
        );

        let candidates = test_eligible_candidates(
            &state,
            &[Zone::Graveyard],
            None,
            &[],
            test_face_policy(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                ObjectId(900),
                PlayerId(0),
            ),
        );

        assert_eq!(candidates, vec![back_only, later]);
        assert_eq!(
            state.objects[&back_only].name, "Spell",
            "the back-face probe must not swap the authoritative candidate"
        );
        assert!(
            !state.objects[&back_only].modal_back_face,
            "the next candidate must not inherit the prior face projection"
        );
        assert_eq!(state.objects[&rejected].name, "Spell");
        assert_eq!(state.objects[&later].name, "Spell");
    }

    /// The shared free-cast traversal flushes one baseline for the whole pool;
    /// target-bearing one- and two-face candidates still receive distinct
    /// candidate and elected-face projections. The target helper fallback must
    /// not clone and flush once again for each face.
    #[test]
    fn targeted_candidate_projection_uses_one_baseline_without_target_fallback() {
        for (face_count, make_card) in [
            (
                1_u32,
                add_targeted_aura_card as fn(&mut GameState, Zone) -> ObjectId,
            ),
            (2_u32, add_targeted_modal_aura_card),
        ] {
            for candidate_count in [1_u32, 3] {
                let mut state = GameState::new_two_player(1);
                let _target = add_target_creature(&mut state);
                let expected: Vec<_> = (0..candidate_count)
                    .map(|_| make_card(&mut state, Zone::Graveyard))
                    .collect();
                let original_state = serde_json::to_value(&state).unwrap();

                crate::game::casting::reset_resolution_cast_projection_measurements();
                let candidates = test_eligible_candidates(
                    &state,
                    &[Zone::Graveyard],
                    None,
                    &[],
                    test_face_policy(
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
                        ObjectId(900),
                        PlayerId(0),
                    ),
                );
                let measurements = crate::game::casting::resolution_cast_projection_measurements();

                assert_eq!(candidates, expected, "all targeted faces stay eligible");
                assert_eq!(measurements.baseline_clones, 1);
                assert_eq!(measurements.baseline_flushes, 1);
                assert_eq!(measurements.candidate_clones, candidate_count);
                assert_eq!(measurements.face_clones, candidate_count * face_count);
                assert_eq!(
                    measurements.projected_target_checks,
                    candidate_count * face_count
                );
                assert_eq!(measurements.target_helper_fallback_clone_flushes, 0);
                assert_eq!(
                    serde_json::to_value(&state).unwrap(),
                    original_state,
                    "projection must be read-only"
                );
            }
        }
    }

    #[test]
    fn member_pool_filter_recurses_through_its_two_direct_nested_carriers() {
        let consumed_binding = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Instant)
                .controller(ControllerRef::TargetPlayer)
                .properties(vec![
                    FilterProp::Owned {
                        controller: ControllerRef::TargetPlayer,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ]),
        );
        let retained = TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant).properties(vec![
            FilterProp::InZone {
                zone: Zone::Graveyard,
            },
        ]));
        let filtered = TargetFilter::And {
            filters: vec![
                TargetFilter::TrackedSetFiltered {
                    id: crate::types::identifiers::TrackedSetId(44),
                    filter: Box::new(consumed_binding.clone()),
                    caused_by: Some(ThisWayCause::Exiled),
                },
                TargetFilter::ChosenDamageSource {
                    filter: Some(Box::new(TargetFilter::Not {
                        filter: Box::new(consumed_binding),
                    })),
                },
                TargetFilter::ChosenDamageSource { filter: None },
            ],
        };

        assert_eq!(
            member_pool_filter(&filtered),
            TargetFilter::And {
                filters: vec![
                    TargetFilter::TrackedSetFiltered {
                        id: crate::types::identifiers::TrackedSetId(44),
                        filter: Box::new(retained.clone()),
                        caused_by: Some(ThisWayCause::Exiled),
                    },
                    TargetFilter::ChosenDamageSource {
                        filter: Some(Box::new(TargetFilter::Not {
                            filter: Box::new(retained),
                        })),
                    },
                    TargetFilter::ChosenDamageSource { filter: None },
                ],
            },
            "only the already-consumed TargetPlayer binding is removed inside the direct carriers"
        );
    }

    /// CR 601.2a: Candidates are gathered from BOTH the graveyard and the hand,
    /// restricted to the instant/sorcery filter (a creature in either zone is
    /// excluded).
    #[test]
    fn gathers_instant_sorcery_from_graveyard_and_hand() {
        let mut state = GameState::new_two_player(1);
        let gy_instant = add_card(
            &mut state,
            PlayerId(0),
            Zone::Graveyard,
            CoreType::Instant,
            2,
        );
        let hand_sorcery = add_card(&mut state, PlayerId(0), Zone::Hand, CoreType::Sorcery, 3);
        let _gy_creature = add_card(
            &mut state,
            PlayerId(0),
            Zone::Graveyard,
            CoreType::Creature,
            1,
        );

        let candidates = test_eligible_candidates(
            &state,
            &[Zone::Graveyard, Zone::Hand],
            None,
            &[],
            test_face_policy(instant_sorcery_filter(), ObjectId(0), PlayerId(0)),
        );
        assert!(candidates.contains(&gy_instant));
        assert!(candidates.contains(&hand_sorcery));
        assert_eq!(
            candidates.len(),
            2,
            "creature must be excluded by the filter"
        );
    }

    /// CR 202.3: A candidate whose mana value exceeds the remaining budget is
    /// excluded; one within budget is kept.
    #[test]
    fn mv_budget_excludes_over_budget_candidates() {
        let mut state = GameState::new_two_player(1);
        let cheap = add_card(
            &mut state,
            PlayerId(0),
            Zone::Graveyard,
            CoreType::Instant,
            4,
        );
        let _expensive = add_card(&mut state, PlayerId(0), Zone::Hand, CoreType::Sorcery, 7);

        let candidates = test_eligible_candidates(
            &state,
            &[Zone::Graveyard, Zone::Hand],
            Some(6),
            &[],
            test_face_policy(instant_sorcery_filter(), ObjectId(0), PlayerId(0)),
        );
        assert_eq!(candidates, vec![cheap]);
    }

    /// CR 601.2a: The window only sees the controller's own cards — an
    /// opponent's graveyard instant is never a candidate.
    #[test]
    fn opponent_cards_are_not_candidates() {
        let mut state = GameState::new_two_player(1);
        let _opp = add_card(
            &mut state,
            PlayerId(1),
            Zone::Graveyard,
            CoreType::Instant,
            1,
        );
        let mine = add_card(
            &mut state,
            PlayerId(0),
            Zone::Graveyard,
            CoreType::Instant,
            1,
        );

        let candidates = test_eligible_candidates(
            &state,
            &[Zone::Graveyard, Zone::Hand],
            None,
            &[],
            test_face_policy(instant_sorcery_filter(), ObjectId(0), PlayerId(0)),
        );
        assert_eq!(candidates, vec![mine]);
    }

    /// CR 608.2g + CR 115.1a: A nonempty member pool is the enumeration
    /// authority. It preserves exactly the paired opponent-graveyard targets,
    /// skips their consumed TargetPlayer/Owned binding even below the linked
    /// `TrackedSetFiltered` carrier, and never substitutes another matching
    /// card from either graveyard on the initial offer or a re-offer after one
    /// pool member becomes illegal.
    #[test]
    fn member_pool_keeps_exact_opponent_graveyard_targets_without_substitutes() {
        use crate::types::ability::{ControllerRef, FilterProp};
        use crate::types::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 1);
        let selected_p1 = add_card(
            &mut state,
            PlayerId(1),
            Zone::Graveyard,
            CoreType::Instant,
            1,
        );
        let selected_p2 = add_card(
            &mut state,
            PlayerId(2),
            Zone::Graveyard,
            CoreType::Sorcery,
            1,
        );
        let _p1_extra = add_card(
            &mut state,
            PlayerId(1),
            Zone::Graveyard,
            CoreType::Instant,
            1,
        );
        let _p2_extra = add_card(
            &mut state,
            PlayerId(2),
            Zone::Graveyard,
            CoreType::Sorcery,
            1,
        );
        let tracked_set = crate::types::identifiers::TrackedSetId(71);
        state.tracked_object_sets.insert(
            tracked_set,
            vec![selected_p1, selected_p2, _p1_extra, _p2_extra],
        );
        let filter = TargetFilter::TrackedSetFiltered {
            id: tracked_set,
            filter: Box::new(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::AnyOf(vec![
                    TypeFilter::Instant,
                    TypeFilter::Sorcery,
                ]))
                .controller(ControllerRef::TargetPlayer)
                .properties(vec![
                    FilterProp::Owned {
                        controller: ControllerRef::TargetPlayer,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ]),
            )),
            caused_by: None,
        };

        let pool = [selected_p1, selected_p2];
        let face_policy = test_face_policy(filter.clone(), ObjectId(900), PlayerId(0));
        let candidates =
            test_eligible_candidates(&state, &[Zone::Graveyard], None, &pool, face_policy.clone());
        assert_eq!(
            candidates,
            vec![selected_p1, selected_p2],
            "the type filter still applies, but only the approved paired targets may be offered"
        );

        state.objects.get_mut(&selected_p1).unwrap().zone = Zone::Exile;
        let reoffered =
            test_eligible_candidates(&state, &[Zone::Graveyard], None, &pool, face_policy);
        assert!(
            reoffered == vec![selected_p2],
            "an illegal selected target must disappear rather than be replaced by a graveyard extra"
        );
    }

    /// Invoke Calamity has no fixed pool, so its established own-zone scan must
    /// remain the authority rather than attempting paired-target semantics.
    #[test]
    fn empty_member_pool_retains_invoke_calamity_controller_zone_scan() {
        let mut state = GameState::new_two_player(1);
        let mine = add_card(
            &mut state,
            PlayerId(0),
            Zone::Graveyard,
            CoreType::Instant,
            1,
        );
        let _opponent = add_card(
            &mut state,
            PlayerId(1),
            Zone::Graveyard,
            CoreType::Instant,
            1,
        );
        assert_eq!(
            test_eligible_candidates(
                &state,
                &[Zone::Graveyard],
                None,
                &[],
                test_face_policy(instant_sorcery_filter(), ObjectId(900), PlayerId(0)),
            ),
            vec![mine],
            "empty pool preserves Invoke Calamity's controller-graveyard scan"
        );
    }

    /// CR 608.2g: An empty candidate set sets no pause and emits EffectResolved,
    /// so the continuation (Exile ~) runs immediately.
    #[test]
    fn no_candidates_skips_the_pause() {
        let mut state = GameState::new_two_player(1);
        let source = create_object(
            &mut state,
            CardId(9000),
            PlayerId(0),
            "Invoke Calamity".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::FreeCastFromZones {
                count: Some(2),
                max_total_mv: Some(6),
                filter: instant_sorcery_filter(),
                zones: vec![Zone::Graveyard, Zone::Hand],
                graveyard_replacement: Some(SpellStackToGraveyardReplacement::Exile),
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::CastOffer {
                    kind: CastOfferKind::FreeCastWindow { .. },
                    ..
                }
            ),
            "no candidates must not open a window"
        );
    }

    /// CR 608.2g + CR 601.2: With eligible candidates, the resolver opens the
    /// free-cast window carrying the count, budget, candidate set, and exile
    /// rider.
    #[test]
    fn opens_window_with_candidates() {
        let mut state = GameState::new_two_player(1);
        let source = create_object(
            &mut state,
            CardId(9000),
            PlayerId(0),
            "Invoke Calamity".to_string(),
            Zone::Stack,
        );
        let instant = add_card(
            &mut state,
            PlayerId(0),
            Zone::Graveyard,
            CoreType::Instant,
            2,
        );
        let ability = ResolvedAbility::new(
            Effect::FreeCastFromZones {
                count: Some(2),
                max_total_mv: Some(6),
                filter: instant_sorcery_filter(),
                zones: vec![Zone::Graveyard, Zone::Hand],
                graveyard_replacement: Some(SpellStackToGraveyardReplacement::Exile),
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::CastOffer {
                player,
                kind:
                    CastOfferKind::FreeCastWindow {
                        candidates,
                        remaining_casts,
                        remaining_mv_budget,
                        graveyard_replacement,
                        ..
                    },
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(candidates, &vec![instant]);
                assert_eq!(*remaining_casts, Some(2));
                assert_eq!(*remaining_mv_budget, Some(6));
                assert_eq!(
                    graveyard_replacement.as_ref(),
                    Some(&SpellStackToGraveyardReplacement::Exile)
                );
            }
            other => panic!("expected FreeCastWindow, got {other:?}"),
        }
    }
}
