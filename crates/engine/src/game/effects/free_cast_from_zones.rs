use crate::game::filter::{matches_target_filter_in_owner_zone, FilterContext};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::{CastOfferKind, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 608.2g + CR 601.2 + CR 118.9: Open an interactive free-cast window.
///
/// The controller may cast up to `count` spells matching `filter` from their
/// own graveyard and/or hand (`zones`), each without paying its mana cost,
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

    let candidates = eligible_candidates(
        state,
        controller,
        ability.source_id,
        &filter,
        &zones,
        max_total_mv,
        &member_pool,
    );

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::FreeCastFromZones,
        source_id: ability.source_id,
        subject: None,
    });

    // CR 601.2: "Up to N" — with no eligible candidate the window opens to zero
    // legal casts, so skip the pause entirely and let the continuation (Exile ~)
    // run. The handler's decline path produces the same outcome, but short-
    // circuiting here avoids a no-op prompt.
    if candidates.is_empty() || count == 0 {
        return Ok(());
    }

    state.waiting_for = WaitingFor::CastOffer {
        player: controller,
        kind: CastOfferKind::FreeCastWindow {
            candidates,
            remaining_casts: count,
            remaining_mv_budget: max_total_mv,
            filter,
            zones,
            graveyard_replacement,
            source: ability.source_id,
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
/// `source` is the resolving ability's source object: `TargetFilter`s such as
/// `ExiledBySource` (Plargg and Nassari's "the other cards exiled this way")
/// resolve their exile links against it, so the real id must be threaded
/// rather than a sentinel.
///
/// `member_pool`, when non-empty, is THIS resolution's concrete approved pool:
/// candidates are enumerated from those exact ids, rather than from a zone scan.
/// It originally served "exiled this way" batches, and also carries paired
/// opponent-graveyard targets into the same window. In the latter case the
/// player target has intentionally been consumed before this resolver runs, so
/// `TargetPlayer`/`Owned(TargetPlayer)` are not evaluated again; zone, type,
/// cast legality, and every unrelated filter property remain authoritative.
/// Empty retains Invoke Calamity's existing controller-zone enumeration.
pub(crate) fn eligible_candidates(
    state: &GameState,
    controller: PlayerId,
    source: ObjectId,
    filter: &TargetFilter,
    zones: &[Zone],
    max_total_mv: Option<u32>,
    member_pool: &[ObjectId],
) -> Vec<ObjectId> {
    let Some(player) = state.players.iter().find(|p| p.id == controller) else {
        return Vec::new();
    };

    let ctx = FilterContext::from_source_with_controller(source, controller);
    let member_pool_filter = member_pool_filter(filter);
    let mut candidates = Vec::new();
    let candidate_ids: Vec<ObjectId> = if member_pool.is_empty() {
        let mut ids = Vec::new();
        for &zone in zones {
            let zone_ids = match zone {
                Zone::Graveyard => &player.graveyard,
                Zone::Hand => &player.hand,
                // CR 400.10a + CR 608.2g: Exile is a shared zone — the whole pile
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
        // CR 305.1: a land card can never be cast — this window is a cast
        // grant, so lands are excluded from the offer outright (mirrors
        // `cast_from_zone`'s cast-mode land guard).
        if state.objects.get(&id).is_some_and(|obj| {
            obj.card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
        }) {
            continue;
        }
        let candidate_filter = if member_pool.is_empty() {
            filter
        } else {
            &member_pool_filter
        };
        if !matches_target_filter_in_owner_zone(state, id, candidate_filter, &ctx) {
            continue;
        }
        // CR 601.2c + CR 608.2g: A spell cast during resolution still
        // needs every required target to be legal before it can be offered.
        let Some(obj) = state.objects.get(&id) else {
            continue;
        };
        if !crate::game::casting::spell_has_legal_targets(state, obj, controller) {
            continue;
        }
        // CR 202.3 + CR 107.3b + CR 601.2b: Respect the running MV budget.
        // Because this window casts without paying a mana cost, X can only
        // be announced as 0, so the card's printed mana_value() is the same
        // value used when the choice is submitted.
        if let Some(budget) = max_total_mv {
            let mv = state
                .objects
                .get(&id)
                // CR 202.3d + CR 709.4b: candidate cards are in a non-stack
                // zone, so a split card's MV budget is its combined halves.
                .map(|obj| obj.effective_mana_value())
                .unwrap_or(0);
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
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{SpellStackToGraveyardReplacement, TypeFilter, TypedFilter};
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

        let candidates = eligible_candidates(
            &state,
            PlayerId(0),
            ObjectId(0),
            &instant_sorcery_filter(),
            &[Zone::Graveyard, Zone::Hand],
            None,
            &[],
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

        let candidates = eligible_candidates(
            &state,
            PlayerId(0),
            ObjectId(0),
            &instant_sorcery_filter(),
            &[Zone::Graveyard, Zone::Hand],
            Some(6),
            &[],
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

        let candidates = eligible_candidates(
            &state,
            PlayerId(0),
            ObjectId(0),
            &instant_sorcery_filter(),
            &[Zone::Graveyard, Zone::Hand],
            None,
            &[],
        );
        assert_eq!(candidates, vec![mine]);
    }

    /// CR 608.2g + CR 115.1a: A nonempty member pool is the enumeration
    /// authority. It preserves exactly the paired opponent-graveyard targets,
    /// skips their consumed TargetPlayer/Owned binding, and never substitutes
    /// another matching card from either graveyard on the initial offer or a
    /// re-offer after one pool member becomes illegal.
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
        let filter = TargetFilter::Typed(
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
        );

        let pool = [selected_p1, selected_p2];
        let candidates = eligible_candidates(
            &state,
            PlayerId(0),
            ObjectId(900),
            &filter,
            &[Zone::Graveyard],
            None,
            &pool,
        );
        assert_eq!(
            candidates,
            vec![selected_p1, selected_p2],
            "the type filter still applies, but only the approved paired targets may be offered"
        );

        state.objects.get_mut(&selected_p1).unwrap().zone = Zone::Exile;
        let reoffered = eligible_candidates(
            &state,
            PlayerId(0),
            ObjectId(900),
            &filter,
            &[Zone::Graveyard],
            None,
            &pool,
        );
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
            eligible_candidates(
                &state,
                PlayerId(0),
                ObjectId(900),
                &instant_sorcery_filter(),
                &[Zone::Graveyard],
                None,
                &[],
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
                count: 2,
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
                count: 2,
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
                assert_eq!(*remaining_casts, 2);
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
