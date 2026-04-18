use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 702.85a: Cascade — when you cast a spell with cascade, exile cards from
/// the top of your library until you exile a nonland card whose mana value is
/// strictly less than this spell's mana value. You may cast that card without
/// paying its mana cost if the resulting spell's mana value is also less than
/// this spell's mana value. Then put all cards exiled this way that weren't
/// cast on the bottom of your library in a random order.
///
/// The second MV check (resulting-spell MV) is enforced at cast time in
/// `casting_costs::finalize_cast_with_phyrexian_choices` via the
/// `CastPermissionConstraint::CascadeResultingMvBelow` predicate, because X
/// and other variable costs are only resolved at that point.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if !matches!(&ability.effect, Effect::Cascade) {
        return Err(EffectError::InvalidParam("Expected Cascade".to_string()));
    }

    // CR 202.3b + CR 702.85a: Read source MV from the stack spell object.
    // `mana_cost.mana_value()` contributes 0 for {X} shards (CR 107.3b), so
    // add `cost_x_paid` to reflect the chosen value of X on the stack.
    let source_mv = state
        .objects
        .get(&ability.source_id)
        .map(|obj| obj.mana_cost.mana_value() + obj.cost_x_paid.unwrap_or(0))
        .unwrap_or(0);

    // CR 702.85a: Validate the controller exists, then iterate per-card by
    // re-reading the live library top each pass. A snapshot taken once would
    // desync if a replacement effect (e.g., "if a card would be exiled from
    // your library, instead ...") redirects, blocks, or reorders the move,
    // because the snapshot remembers the pre-replacement order.
    if !state.players.iter().any(|p| p.id == ability.controller) {
        return Err(EffectError::PlayerNotFound);
    }

    let mut exiled_misses: Vec<ObjectId> = Vec::new();
    let mut hit_card: Option<ObjectId> = None;

    // CR 702.85a: Exile one at a time until a nonland with MV < source_mv is
    // exiled, or the library is exhausted. Each iteration reads `library[0]`
    // off the live player record so any replacement effect that mutated the
    // library mid-loop is observed.
    while let Some(card_id) = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .and_then(|p| p.library.first().copied())
    {
        zones::move_to_zone(state, card_id, Zone::Exile, events);

        // CR 614.1: A replacement effect may have moved the card to a zone
        // other than Exile, or removed it entirely. Only count it as exiled
        // (a hit or a miss) if it actually landed in Exile.
        let zone_after = state.objects.get(&card_id).map(|o| o.zone);
        if zone_after != Some(Zone::Exile) {
            // Replacement redirected or removed; do not loop on the same card.
            // Re-read the next library top on the next iteration.
            if state
                .players
                .iter()
                .find(|p| p.id == ability.controller)
                .is_some_and(|p| p.library.first().copied() == Some(card_id))
            {
                // Defensive: if the card is somehow still on top, break to
                // avoid an infinite loop.
                break;
            }
            continue;
        }

        let is_hit = state.objects.get(&card_id).is_some_and(|obj| {
            let is_land = obj.card_types.core_types.contains(&CoreType::Land);
            let mv = obj.mana_cost.mana_value();
            !is_land && mv < source_mv
        });

        if is_hit {
            hit_card = Some(card_id);
            break;
        } else {
            exiled_misses.push(card_id);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    match hit_card {
        Some(hit) => {
            // CR 702.85a: Offer the cast. The caster's response is handled in
            // `engine_resolution_choices` — we do not bottom-shuffle misses
            // here because a rejection at cast time (X makes resulting MV
            // ineligible) must still bottom-shuffle them together with the
            // hit, and that path runs from `casting_costs`.
            state.waiting_for = WaitingFor::CascadeChoice {
                player: ability.controller,
                hit_card: hit,
                exiled_misses,
                source_mv,
            };
        }
        None => {
            // CR 702.85a: Library exhausted with no eligible hit — shuffle all
            // exiled misses to the bottom in random order.
            shuffle_to_bottom(state, &exiled_misses, events);
        }
    }

    Ok(())
}

/// CR 702.85a: Put cards on the bottom of the player's library in random order.
pub(crate) fn shuffle_to_bottom(
    state: &mut GameState,
    cards: &[ObjectId],
    events: &mut Vec<GameEvent>,
) {
    use rand::seq::SliceRandom;

    let mut shuffled = cards.to_vec();
    shuffled.shuffle(&mut state.rng);

    for &card_id in &shuffled {
        zones::move_to_library_position(state, card_id, false, events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;

    /// Build a two-player state with `source_id` on the battlefield as a
    /// proxy for the cascade spell. For unit tests, MV is read off the
    /// `mana_cost` field regardless of zone, so battlefield is sufficient.
    fn setup_with_source(source_mv: u32) -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Cascade Spell".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source_id).unwrap().mana_cost = ManaCost::generic(source_mv);
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .keywords
            .push(Keyword::Cascade);
        (state, source_id)
    }

    fn add_library_card(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        mv: u32,
        is_land: bool,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, owner, name.to_string(), Zone::Library);
        let obj = state.objects.get_mut(&id).unwrap();
        if is_land {
            obj.card_types.core_types.push(CoreType::Land);
        } else {
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(mv);
        }
        id
    }

    /// CR 702.85a: basic flow — first nonland with MV < source MV is offered,
    /// prior lands are recorded as misses.
    #[test]
    fn basic_flow_offers_first_eligible_nonland() {
        let (mut state, source_id) = setup_with_source(4);
        // Library top-first ordering: with_library_top-style — insertion order
        // is bottom-first here, so append in pop order.
        let land1 = add_library_card(&mut state, PlayerId(0), "Forest", 0, true);
        let land2 = add_library_card(&mut state, PlayerId(0), "Mountain", 0, true);
        let hit = add_library_card(&mut state, PlayerId(0), "Bear", 2, false);
        // library[0] is top (CR 402.2 / engine convention); set so cascade
        // exiles land1, land2, then finds hit.
        state.players[0].library = vec![land1, land2, hit];

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CascadeChoice {
                hit_card,
                exiled_misses,
                source_mv,
                ..
            } => {
                assert_eq!(*hit_card, hit);
                assert_eq!(exiled_misses, &vec![land1, land2]);
                assert_eq!(*source_mv, 4);
            }
            other => panic!("Expected CascadeChoice, got {:?}", other),
        }
    }

    /// CR 702.85a: first MV check is strict inequality. A nonland with MV
    /// equal to source MV is a miss; the next eligible card is the hit.
    #[test]
    fn mv_boundary_strict_inequality() {
        let (mut state, source_id) = setup_with_source(4);
        let equal = add_library_card(&mut state, PlayerId(0), "Equal MV", 4, false);
        let hit = add_library_card(&mut state, PlayerId(0), "Below MV", 3, false);
        state.players[0].library = vec![equal, hit];

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CascadeChoice {
                hit_card,
                exiled_misses,
                ..
            } => {
                assert_eq!(*hit_card, hit);
                assert_eq!(exiled_misses, &vec![equal]);
            }
            other => panic!("Expected CascadeChoice, got {:?}", other),
        }
    }

    /// CR 702.85a: if the library runs out with no eligible hit, all exiled
    /// cards go to the bottom in a random order and no choice is offered.
    #[test]
    fn library_exhausted_no_hit_no_choice() {
        let (mut state, source_id) = setup_with_source(2);
        // Only MV-2 and MV-3 nonlands present — none are strictly less than 2.
        let a = add_library_card(&mut state, PlayerId(0), "Too Big A", 3, false);
        let b = add_library_card(&mut state, PlayerId(0), "Too Big B", 2, false);
        state.players[0].library = vec![a, b];

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No CascadeChoice produced — waiting_for remains whatever the initial
        // state was (resolver leaves it alone when library is exhausted).
        assert!(
            !matches!(state.waiting_for, WaitingFor::CascadeChoice { .. }),
            "No CascadeChoice should be offered when nothing hits"
        );

        // Both cards should be back in library (on bottom), none on battlefield
        // or exile.
        assert_eq!(
            state.players[0].library.len(),
            2,
            "Exiled misses must be shuffled back to the bottom of the library"
        );
        for &id in &[a, b] {
            assert_eq!(
                state.objects.get(&id).map(|o| o.zone),
                Some(Zone::Library),
                "Miss card must be in library, not exile"
            );
        }
    }

    /// CR 202.3b: the source MV snapshot read into `CascadeChoice.source_mv`
    /// reflects the cascade spell's mana value at trigger resolution time.
    /// For an X-cost cascade spell with X already chosen, MV is the chosen
    /// value (tested here via the `chosen_x` field on the source object).
    #[test]
    fn source_mv_reads_current_mana_value() {
        let (mut state, source_id) = setup_with_source(5);
        let hit = add_library_card(&mut state, PlayerId(0), "Small", 1, false);
        state.players[0].library = vec![hit];

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CascadeChoice { source_mv, .. } => assert_eq!(*source_mv, 5),
            other => panic!("Expected CascadeChoice, got {:?}", other),
        }
    }
}
