use rand::seq::SliceRandom;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{
    Effect, EffectError, EffectKind, LibraryPosition, QuantityExpr, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// Place target card at a specific position in its owner's library. Unlike
/// ChangeZone { destination: Library } which shuffles the destination library,
/// this places at a specific position without shuffling.
///
/// Also handles LTB self-return triggers (CR 603.10a) such as Avenging Angel:
/// "When this creature dies, you may put it on top of its owner's library."
/// When the trigger resolves, the source is already in the graveyard. The parser
/// emits `target: ParentTarget` (or `SelfRef`) with empty `ability.targets`; the
/// resolver treats that as a self-reference to `ability.source_id`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (position, target_filter, count_expr) = match &ability.effect {
        Effect::PutAtLibraryPosition {
            position,
            target,
            count,
        } => (position.clone(), target.clone(), count.clone()),
        _ => (
            LibraryPosition::Top,
            TargetFilter::None,
            QuantityExpr::Fixed { value: 1 },
        ),
    };

    // CR 401.5 + CR 608.2c (issue #1365): This Dig-tail seam is the one place
    // "put up to one of them on top … the rest on the bottom" can land with
    // `target: ParentTarget` and no selection — when the Dig it follows looked
    // at an empty library. There is nothing to place, and that must NOT fall
    // back to `resolved_targets`'s generic self-fallback (below), which would
    // otherwise move the Dig's own source (e.g. a reanimated Thassa's Oracle)
    // into the library it just found empty, corrupting devotion and
    // library-count reads for any trailing win condition.
    //
    // `ability.dig_found_nothing_for_parent_target` is a typed, per-ability
    // signal stamped ONLY by `effects::apply_parent_chain_context` at the
    // exact moment THIS ability is handed off as a Dig's immediate
    // sub_ability — never copied to grandchildren and never read from raw
    // global state here. That means every OTHER `ParentTarget` consumer
    // (Avenging Angel's LTB self-return, etc.) keeps its ordinary
    // self-fallback regardless of an unrelated Dig anywhere else in the same
    // resolution, including a second, later `PutAtLibraryPosition` call.
    let dig_found_nothing_for_parent_target = matches!(target_filter, TargetFilter::ParentTarget)
        && ability.targets.is_empty()
        && ability.dig_found_nothing_for_parent_target;

    // CR 608.2c + 603.10a: Delegate to the unified 3-tier dispatch
    // (`resolved_targets`). `SelfRef` always resolves to the source object;
    // `None` / `ParentTarget` fall back to the source only when
    // `ability.targets` is empty (the LTB self-return shape — Avenging Angel).
    // This is the post-#323 SelfRef short-circuit applied uniformly.
    let effective_targets = if dig_found_nothing_for_parent_target {
        Vec::new()
    } else {
        crate::game::targeting::resolved_targets(ability, &target_filter, state)
    };
    // CR 608.2c: `effect_object_targets` forwards `ability.targets` verbatim
    // for non-slot filters. A dig hand-keep binds `ParentTarget` on the exile
    // tail but must not pre-fill a `TrackedSet` bottom pick with the kept card.
    let mut collected_targets = match &target_filter {
        TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. } => Vec::new(),
        _ => crate::game::effects::effect_object_targets(&target_filter, &effective_targets),
    };
    if collected_targets.is_empty() && matches!(target_filter, TargetFilter::ExiledBySource) {
        let ctx = crate::game::filter::FilterContext::from_ability(ability);
        collected_targets = state
            .objects
            .iter()
            .filter(|(id, obj)| {
                obj.zone == Zone::Exile
                    && crate::game::filter::matches_target_filter(state, **id, &target_filter, &ctx)
            })
            .map(|(id, _)| *id)
            .collect();
        // CR 701.20e: Look-then-cast tails put uncast looked-at cards on the
        // bottom via `ExiledBySource`, but those cards remain in the library.
        if collected_targets.is_empty() && !state.last_revealed_ids.is_empty() {
            collected_targets = crate::game::filter::last_revealed_library_ids_matching(
                state,
                &target_filter,
                &ctx,
            );
        }
    }
    if collected_targets.is_empty()
        && matches!(
            target_filter,
            TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. }
        )
    {
        // CR 608.2c + CR 401.2: Tracked-set continuations after a dig keep may
        // still reference cards left in the library (Expressive Iteration's
        // "put one on the bottom" step). Only those library members are legal
        // picks — cards already routed to hand must not re-enter this choice.
        // Resolve sentinel/explicit tracked-set identity first, then apply the
        // filter predicate instead of blindly reading `chain_tracked_set_id`.
        let effective_filter =
            crate::game::targeting::resolve_tracked_set_sentinel(state, target_filter.clone());
        let ctx = crate::game::filter::FilterContext::from_ability(ability);
        let candidate_ids: Vec<ObjectId> = match &effective_filter {
            TargetFilter::TrackedSet { id } | TargetFilter::TrackedSetFiltered { id, .. } => state
                .tracked_object_sets
                .get(id)
                .cloned()
                .unwrap_or_default(),
            _ => state.objects.keys().copied().collect(),
        };
        collected_targets = candidate_ids
            .into_iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.zone == Zone::Library)
                    && crate::game::filter::matches_target_filter(
                        state,
                        *id,
                        &effective_filter,
                        &ctx,
                    )
            })
            .collect();
    }

    // CR 115.1 + CR 400.2: When the filter specifies a private zone (hand/library)
    // and no targets were pre-selected during casting (because the Oracle text does
    // not say "target"), present an EffectZoneChoice for resolution-time selection.
    // This covers Brainstorm ("put two cards from your hand on top of your library")
    // and similar cards where the player chooses during resolution.
    let expected = resolve_quantity_with_targets(state, &count_expr, ability).max(0) as usize;
    let expected = if expected == 0
        && matches!(
            position,
            LibraryPosition::Bottom | LibraryPosition::NthFromTop { .. }
        )
        && matches!(
            target_filter,
            TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. }
        )
        && !collected_targets.is_empty()
    {
        // Parser placeholder `count: 0` on dig-tail bottom steps means "one of
        // the tracked looked-at cards", not "all of them".
        1
    } else {
        expected
    };

    if collected_targets.is_empty() {
        if expected == 0 {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::PutAtLibraryPosition,
                source_id: ability.source_id,
            });
            return Ok(());
        }
        if let Some(source_zone) = target_filter.extract_in_zone() {
            if matches!(source_zone, Zone::Hand | Zone::Library) {
                let eligible: Vec<_> = match source_zone {
                    Zone::Hand => state.players[ability.controller.0 as usize]
                        .hand
                        .iter()
                        .copied()
                        .collect(),
                    Zone::Library => state.players[ability.controller.0 as usize]
                        .library
                        .iter()
                        .copied()
                        .collect(),
                    _ => unreachable!(),
                };
                let eligible_count = eligible.len();
                if eligible.is_empty() {
                    events.push(GameEvent::EffectResolved {
                        kind: EffectKind::PutAtLibraryPosition,
                        source_id: ability.source_id,
                    });
                    return Ok(());
                }
                state.waiting_for = WaitingFor::EffectZoneChoice {
                    player: ability.controller,
                    cards: eligible,
                    count: expected.min(eligible_count),
                    min_count: 0,
                    up_to: false,
                    source_id: ability.source_id,
                    effect_kind: EffectKind::PutAtLibraryPosition,
                    zone: source_zone,
                    destination: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enter_transformed: false,
                    enters_under_player: None,
                    enters_attacking: false,
                    owner_library: false,
                    track_exiled_by_source: false,
                    // CR 708.2a: library-position selection is not a face-down entry.
                    face_down_profile: None,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    count_param: 0,
                    library_position: Some(position.clone()),
                    is_cost_payment: false,
                };
                return Ok(());
            }
        }
        // CR 701.23b: A search/forward continuation that found nothing — fail to
        // find, or no instant/sorcery left in the library for a top-of-library
        // tutor (Mystical/Vampiric/Worldly/Personal/Enlightened Tutor) — leaves
        // `target: Any` with no forwarded card. There is nothing to position, so
        // resolve as a legal no-op: the spell still shuffled, it just placed no
        // card on top. Only `Any` (the "the card you found" forward target) is
        // treated this way; a concrete filter resolving to nothing is a real bug.
        if matches!(target_filter, TargetFilter::Any) {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::PutAtLibraryPosition,
                source_id: ability.source_id,
            });
            return Ok(());
        }
        return Err(EffectError::InvalidParam(
            "PutAtLibraryPosition requires a target".to_string(),
        ));
    }

    // CR 115.1 + CR 608.2c: When more tracked-set library candidates exist
    // than the placement count allows, prompt before auto-picking the first.
    if collected_targets.len() > expected && expected > 0 {
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: ability.controller,
            cards: collected_targets,
            count: expected,
            min_count: 0,
            up_to: false,
            source_id: ability.source_id,
            effect_kind: EffectKind::PutAtLibraryPosition,
            zone: Zone::Library,
            destination: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            face_down_profile: None,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            count_param: 0,
            library_position: Some(position.clone()),
            is_cost_payment: false,
        };
        return Ok(());
    }

    // `count` carries the cardinality of the placement. For multi-card
    // placement, CR 401.4 lets the owner arrange cards put into the same
    // library position. The runtime uses target/selection order for "in any
    // order" effects; linked-exile bottom cleanup randomizes separately below.
    let mut randomized_targets;
    let to_place = if expected == 0 {
        if matches!(position, LibraryPosition::Bottom)
            && matches!(target_filter, TargetFilter::ExiledBySource)
        {
            randomized_targets = collected_targets.clone();
            randomized_targets.shuffle(&mut state.rng);
            randomized_targets.as_slice()
        } else {
            collected_targets.as_slice()
        }
    } else {
        &collected_targets[..collected_targets.len().min(expected)]
    };

    let index = match &position {
        // Top = index 0, Bottom = None (push to end), NthFromTop = index n-1
        // ("second from the top" = index 1).
        LibraryPosition::Top => Some(0),
        LibraryPosition::Bottom => None,
        LibraryPosition::NthFromTop { n } => Some(n.saturating_sub(1) as usize),
        // CR 401.7 (Unexpectedly Absent class): "just beneath the top N cards"
        // leaves exactly `depth` cards above the placed object, i.e. the 0-based
        // insertion index IS the resolved depth (no `-1`, unlike `NthFromTop`).
        // Per CR 401.7 `move_to_library_at_index` clamps an index past the
        // library size to the bottom.
        LibraryPosition::BeneathTop { depth } => {
            Some(resolve_quantity_with_targets(state, depth, ability).max(0) as usize)
        }
    };
    match position {
        LibraryPosition::Top => {
            for object_id in to_place.iter().rev() {
                zones::move_to_library_at_index(state, *object_id, index, events);
            }
        }
        LibraryPosition::Bottom
        | LibraryPosition::NthFromTop { .. }
        | LibraryPosition::BeneathTop { .. } => {
            for object_id in to_place {
                zones::move_to_library_at_index(state, *object_id, index, events);
            }
        }
    }
    if matches!(target_filter, TargetFilter::ExiledBySource) {
        state.exile_links.retain(|link| {
            link.source_id != ability.source_id || !to_place.contains(&link.exiled_id)
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PutAtLibraryPosition,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn test_resolve_puts_card_on_top_of_library() {
        let mut state = GameState::new_two_player(42);
        // Create two cards in the library
        let _id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );

        // id2 is at the end of the library; put it on top
        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
            },
            vec![TargetRef::Object(id2)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // id2 should now be at library[0] (top)
        assert_eq!(state.players[0].library[0], id2);
        assert_eq!(state.objects[&id2].zone, Zone::Library);
    }

    /// CR 608.2c (issue #323 class): a chained
    /// `PutAtLibraryPosition { target: SelfRef }` sub-ability must put the
    /// source object on top of its owner's library even when chain target
    /// propagation in `effects::mod.rs::resolve_ability_chain` injected the
    /// parent's targets into `ability.targets`. Pre-fix this resolver
    /// collapsed `SelfRef | None | ParentTarget` into the
    /// `ability.targets.is_empty()` gate, so a propagated parent target would
    /// route through the chosen-targets branch and put the wrong object on
    /// top.
    #[test]
    fn put_on_top_selfref_overrides_propagated_parent_targets() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Graveyard,
        );
        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::SelfRef,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
            },
            // Simulate chain target propagation from a parent that targeted
            // `other`. SelfRef must override and put the source on top.
            vec![TargetRef::Object(other)],
            source,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].library[0], source,
            "SelfRef sub-ability must put the SOURCE on top, \
             not the propagated parent target"
        );
        assert_eq!(state.objects[&source].zone, Zone::Library);
    }

    #[test]
    fn test_resolve_does_not_shuffle_library() {
        let mut state = GameState::new_two_player(42);
        // Create three cards to verify order is preserved
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );
        let id3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Card C".to_string(),
            Zone::Library,
        );

        // Record order before: [id1, id2, id3]
        let lib = &state.players[0].library;
        let before_order: Vec<_> = lib.iter().copied().collect();
        assert_eq!(before_order, vec![id1, id2, id3]);

        // Put id2 on top
        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
            },
            vec![TargetRef::Object(id2)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Expected: [id2, id1, id3] — id2 on top, rest preserved in order
        let lib = &state.players[0].library;
        let after_order: Vec<_> = lib.iter().copied().collect();
        assert_eq!(after_order, vec![id2, id1, id3]);
    }

    /// Issue #2019 — look-then-cast cleanup binds `ExiledBySource` but cards
    /// remain in the library; the bottom step must read `last_revealed_ids`.
    #[test]
    fn exiled_by_source_bottom_cleanup_uses_last_revealed_library_cards() {
        let mut state = GameState::new_two_player(42);
        let bottom_marker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bottom Marker".to_string(),
            Zone::Library,
        );
        let looked_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Looked A".to_string(),
            Zone::Library,
        );
        let looked_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Looked B".to_string(),
            Zone::Library,
        );
        state.last_revealed_ids = vec![looked_a, looked_b];

        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::ExiledBySource,
                count: QuantityExpr::Fixed { value: 0 },
                position: LibraryPosition::Bottom,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        let lib = &state.players[0].library;
        assert_eq!(lib[0], bottom_marker, "untouched card stays on top");
        assert_eq!(lib[lib.len() - 2], looked_a, "first looked card on bottom");
        assert_eq!(lib[lib.len() - 1], looked_b, "second looked card on bottom");
        assert_eq!(state.objects[&looked_a].zone, Zone::Library);
        assert_eq!(state.objects[&looked_b].zone, Zone::Library);
    }

    #[test]
    fn count_zero_places_all_selected_cards_on_top_in_target_order() {
        let mut state = GameState::new_two_player(42);
        let existing = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Existing".to_string(),
            Zone::Library,
        );
        let first = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );
        let third = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Third".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 0 },
                position: LibraryPosition::Top,
            },
            vec![
                TargetRef::Object(third),
                TargetRef::Object(first),
                TargetRef::Object(second),
            ],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = vec![];

        resolve(&mut state, &ability, &mut events).unwrap();

        let after_order: Vec<_> = state.players[0].library.iter().copied().collect();
        assert_eq!(after_order, vec![third, first, second, existing]);
    }

    #[test]
    fn count_zero_with_no_selected_cards_is_noop() {
        let mut state = GameState::new_two_player(42);
        let existing = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Existing".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 0 },
                position: LibraryPosition::Top,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = vec![];

        resolve(&mut state, &ability, &mut events).unwrap();

        let after_order: Vec<_> = state.players[0].library.iter().copied().collect();
        assert_eq!(after_order, vec![existing]);
        assert!(matches!(
            events.as_slice(),
            [GameEvent::EffectResolved {
                kind: EffectKind::PutAtLibraryPosition,
                source_id: ObjectId(100)
            }]
        ));
    }

    #[test]
    fn test_resolve_puts_card_on_bottom() {
        let mut state = GameState::new_two_player(42);
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );
        // Put id1 on bottom
        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Bottom,
            },
            vec![TargetRef::Object(id1)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // id1 should be at the bottom, id2 on top
        let lib = &state.players[0].library;
        assert_eq!(*lib.last().unwrap(), id1);
        assert_eq!(lib[0], id2);
    }

    /// CR 603.10a / Avenging Angel class: LTB self-return triggers fire after
    /// the source has moved to the graveyard. The parsed effect is
    /// `PutAtLibraryPosition { target: ParentTarget }` with empty
    /// `ability.targets`; the resolver must treat that as "put the source object
    /// from the graveyard on top of its owner's library."
    #[test]
    fn test_put_on_top_ltb_self_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Avenging Angel".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::ParentTarget,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].graveyard.contains(&obj_id));
        assert_eq!(state.players[0].library[0], obj_id);
        assert_eq!(state.objects[&obj_id].zone, Zone::Library);
    }

    /// Issue #1365 follow-up: this resolver must consult
    /// `ability.dig_found_nothing_for_parent_target` (a typed field stamped
    /// ONLY by `effects::apply_parent_chain_context` at a real Dig->child
    /// hand-off), never `state.last_dig_found_nothing` directly. A freshly
    /// built `ResolvedAbility` (as in any ordinary LTB self-return trigger)
    /// never goes through that hand-off, so its field stays `false` no matter
    /// what stray global state an unrelated, earlier empty-library Dig left
    /// behind in the same resolution.
    #[test]
    fn stale_dig_found_nothing_does_not_suppress_unrelated_ltb_self_return() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Avenging Angel".to_string(),
            Zone::Graveyard,
        );
        // Simulate a stale flag left behind by an unrelated empty-library Dig
        // earlier in the same top-level resolution.
        state.last_dig_found_nothing = true;

        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::ParentTarget,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.players[0].graveyard.contains(&obj_id),
            "a stale Dig flag must not suppress this unrelated LTB self-return"
        );
        assert_eq!(state.players[0].library[0], obj_id);
        assert_eq!(state.objects[&obj_id].zone, Zone::Library);
    }

    #[test]
    fn test_put_on_top_ltb_self_ref_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Selfsame".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::SelfRef,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
            },
            vec![],
            obj_id,
            PlayerId(1),
        );
        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[1].graveyard.contains(&obj_id));
        assert_eq!(state.players[1].library[0], obj_id);
    }

    /// End-to-end Avenging Angel-class pipeline test.
    #[test]
    fn test_put_on_top_ltb_pipeline_returns_to_top_of_library() {
        use crate::game::stack::resolve_top;
        use crate::game::triggers::process_triggers;
        use crate::types::ability::{AbilityDefinition, AbilityKind, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let angel_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Avenging Angel".to_string(),
            Zone::Battlefield,
        );

        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.trigger_zones = vec![Zone::Graveyard];
        trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutAtLibraryPosition {
                target: TargetFilter::ParentTarget,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
            },
        )));
        state
            .objects
            .get_mut(&angel_id)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, angel_id, Zone::Graveyard, &mut events);
        assert!(state.players[0].graveyard.contains(&angel_id));

        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 1, "LTB trigger did not reach the stack");

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        assert_eq!(
            state.players[0].library[0], angel_id,
            "Avenging Angel should be on top of its owner's library"
        );
        assert!(!state.players[0].graveyard.contains(&angel_id));
    }

    #[test]
    fn test_resolve_puts_card_nth_from_top() {
        let mut state = GameState::new_two_player(42);
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );
        let id3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Card C".to_string(),
            Zone::Library,
        );
        let id4 = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Card D".to_string(),
            Zone::Hand,
        );

        // Library is [id1, id2, id3]. Put id4 (from hand) third from top.
        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::NthFromTop { n: 3 },
            },
            vec![TargetRef::Object(id4)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // "third from the top" = index 2: [id1, id2, id4, id3]
        let lib: Vec<_> = state.players[0].library.iter().copied().collect::<Vec<_>>();
        assert_eq!(lib, vec![id1, id2, id4, id3]);
        assert_eq!(state.objects[&id4].zone, Zone::Library);
    }

    /// CR 115.1 + CR 400.2: Brainstorm-class — "put two cards from your hand on
    /// top of your library" is a resolution-time selection (no "target" keyword),
    /// so the resolver must emit EffectZoneChoice instead of requiring pre-selected
    /// targets.
    #[test]
    fn test_resolution_time_hand_selection_emits_effect_zone_choice() {
        use crate::types::ability::FilterProp;
        use crate::types::ability::TypeFilter;

        let mut state = GameState::new_two_player(42);
        // Put 3 cards in hand (simulating post-draw)
        let h1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hand A".to_string(),
            Zone::Hand,
        );
        let h2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hand B".to_string(),
            Zone::Hand,
        );
        let h3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Hand C".to_string(),
            Zone::Hand,
        );

        // PutAtLibraryPosition with InZone(Hand) filter and no pre-selected targets
        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Typed(crate::types::ability::TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    controller: Some(crate::types::ability::ControllerRef::You),
                    properties: vec![FilterProp::InZone { zone: Zone::Hand }],
                }),
                count: QuantityExpr::Fixed { value: 2 },
                position: LibraryPosition::Top,
            },
            vec![], // No targets — resolution-time selection
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should emit EffectZoneChoice for the player to select 2 cards from hand
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                effect_kind,
                zone,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 2);
                assert_eq!(*effect_kind, EffectKind::PutAtLibraryPosition);
                assert_eq!(*zone, Zone::Hand);
                assert!(cards.contains(&h1));
                assert!(cards.contains(&h2));
                assert!(cards.contains(&h3));
            }
            other => panic!("Expected EffectZoneChoice, got {:?}", other),
        }
    }

    /// CR 608.2c (issue #1162): filtered tracked-set library-position
    /// continuations must honor `TrackedSetFiltered`, not every library member
    /// in the chain set.
    #[test]
    fn tracked_set_filtered_library_bottom_honors_inner_filter() {
        use crate::types::ability::TypeFilter;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::TrackedSetId;

        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Library,
        );
        let instant_a = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Bolt A".to_string(),
            Zone::Library,
        );
        let instant_b = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Bolt B".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        state
            .objects
            .get_mut(&instant_a)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Instant];
        state
            .objects
            .get_mut(&instant_b)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Instant];
        state.players[0].library = vec![creature, instant_a, instant_b].into();

        let set_id = TrackedSetId(7);
        state
            .tracked_object_sets
            .insert(set_id, vec![creature, instant_a, instant_b]);
        state.chain_tracked_set_id = Some(set_id);

        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::TrackedSetFiltered {
                    id: set_id,
                    filter: Box::new(TargetFilter::Typed(
                        crate::types::ability::TypedFilter::new(TypeFilter::Instant),
                    )),
                    caused_by: None,
                },
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Bottom,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert_eq!(
                    cards,
                    &vec![instant_a, instant_b],
                    "only instant members of the tracked set may be offered"
                );
                assert!(
                    !cards.contains(&creature),
                    "creature must not be offered for instant-only filter"
                );
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }
}
