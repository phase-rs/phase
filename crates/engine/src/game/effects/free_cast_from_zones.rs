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
/// Invoke Calamity is the type specimen. The `exile_instead_of_graveyard` rider
/// (CR 614.1a — "if those spells would be put into your graveyard, exile them
/// instead") is carried on the offer so each cast spell is stamped with it.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, max_total_mv, filter, zones, exile_instead_of_graveyard) = match &ability.effect {
        Effect::FreeCastFromZones {
            count,
            max_total_mv,
            filter,
            zones,
            exile_instead_of_graveyard,
        } => (
            *count,
            *max_total_mv,
            filter.clone(),
            zones.clone(),
            *exile_instead_of_graveyard,
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

    let candidates = eligible_candidates(state, controller, &filter, &zones, max_total_mv);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::FreeCastFromZones,
        source_id: ability.source_id,
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
            exile_instead_of_graveyard,
        },
    };

    Ok(())
}

/// CR 601.2a + CR 202.3: Gather the controller's own cards in `zones` that match
/// `filter` and (when `max_total_mv` is `Some`) whose mana value fits the
/// remaining budget. Shared by the resolver and the handler's re-offer loop so
/// the candidate set stays consistent across both entry points.
pub(crate) fn eligible_candidates(
    state: &GameState,
    controller: PlayerId,
    filter: &TargetFilter,
    zones: &[Zone],
    max_total_mv: Option<u32>,
) -> Vec<ObjectId> {
    let Some(player) = state.players.iter().find(|p| p.id == controller) else {
        return Vec::new();
    };

    let ctx = FilterContext::from_source_with_controller(ObjectId(0), controller);
    let mut candidates = Vec::new();
    for &zone in zones {
        let zone_ids = match zone {
            Zone::Graveyard => &player.graveyard,
            Zone::Hand => &player.hand,
            // CR 601.2a: The class today only draws from the controller's
            // graveyard and hand. A new zone would need a parser/effect change,
            // so an unexpected zone contributes no candidates rather than
            // silently scanning the wrong pile.
            _ => continue,
        };
        for &id in zone_ids {
            if !matches_target_filter_in_owner_zone(state, id, filter, &ctx) {
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
                    .map(|obj| obj.mana_cost.mana_value())
                    .unwrap_or(0);
                if mv > budget {
                    continue;
                }
            }
            candidates.push(id);
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{TypeFilter, TypedFilter};
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
            &instant_sorcery_filter(),
            &[Zone::Graveyard, Zone::Hand],
            None,
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
            &instant_sorcery_filter(),
            &[Zone::Graveyard, Zone::Hand],
            Some(6),
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
            &instant_sorcery_filter(),
            &[Zone::Graveyard, Zone::Hand],
            None,
        );
        assert_eq!(candidates, vec![mine]);
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
                exile_instead_of_graveyard: true,
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
                exile_instead_of_graveyard: true,
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
                        exile_instead_of_graveyard,
                        ..
                    },
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(candidates, &vec![instant]);
                assert_eq!(*remaining_casts, 2);
                assert_eq!(*remaining_mv_budget, Some(6));
                assert!(*exile_instead_of_graveyard);
            }
            other => panic!("expected FreeCastWindow, got {other:?}"),
        }
    }
}
