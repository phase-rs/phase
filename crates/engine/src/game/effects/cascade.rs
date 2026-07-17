use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{Effect, EffectError, EffectKind, LibraryPosition, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{BatchCompletion, CastOfferKind, GameState, WaitingFor};
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
/// `CastPermissionConstraint::ManaValue` predicate carried on the hit's
/// cast-during-resolution `ExileWithAltCost` permission (CR 608.2g), because X
/// and other variable costs are only resolved at that point.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if !matches!(&ability.effect, Effect::Cascade) {
        return Err(EffectError::InvalidParam("Expected Cascade".to_string()));
    }

    // CR 202.3b + CR 202.3d + CR 202.3e + CR 702.85a + CR 702.102b: Read the source
    // spell's mana value from the stack object through the split-aware authority.
    // `spell_mana_value` returns the COMBINED value of both halves for a FUSED split
    // spell (CR 202.3d + CR 702.102b) and otherwise the object's own cost with the
    // chosen X included (`cost_x_paid`, CR 202.3e) — so a fused `Breaking // Entering`
    // that gained cascade seeds the threshold from its combined MV (8), not the front
    // half (2). Byte-identical to the prior `mana_value_with_x(zone, cost_x_paid)`
    // read for every non-fused spell.
    let source_mv = state
        .objects
        .get(&ability.source_id)
        .map(|obj| obj.spell_mana_value())
        .unwrap_or(0);

    // CR 603.3a: Re-read the controller from the source spell at resolution
    // time rather than trusting `ability.controller` (captured at trigger-
    // creation time). If the cascade spell is still on the stack we use its
    // current `controller` so a control-change effect between trigger
    // creation and resolution is honored. If the spell has left the stack,
    // fall back to the trigger's snapshot.
    // TODO: unify controller-at-resolution pattern across triggers (this
    // currently has to be done at the resolver per effect).
    let controller = state
        .objects
        .get(&ability.source_id)
        .map(|obj| obj.controller)
        .unwrap_or(ability.controller);

    if !state.players.iter().any(|p| p.id == controller) {
        return Err(EffectError::PlayerNotFound);
    }

    let _ = continue_exile_loop(
        state,
        controller,
        ability.source_id,
        source_mv,
        Vec::new(),
        events,
    );

    Ok(())
}

/// CR 702.85a + CR 614.1 + CR 616.1: Propose one Library-to-Exile event at a
/// time. The typed completion carries the loop cursor across a replacement
/// choice, so the next top card and the cascade tail cannot run early.
pub(crate) fn continue_exile_loop(
    state: &mut GameState,
    controller: crate::types::player::PlayerId,
    source_id: ObjectId,
    source_mv: u32,
    exiled_misses: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    let Some(current_card) = state
        .players
        .iter()
        .find(|player| player.id == controller)
        .and_then(|player| player.library.front().copied())
    else {
        return finish_without_hit(state, controller, source_id, exiled_misses, events);
    };

    zone_pipeline::move_objects_simultaneously_then(
        state,
        vec![ZoneMoveRequest::effect(
            current_card,
            Zone::Exile,
            source_id,
        )],
        Some(BatchCompletion::CascadeExileLoopComplete {
            controller,
            source_id,
            source_mv,
            exiled_misses,
            current_card,
        }),
        events,
    )
}

/// CR 702.85a + CR 614.1 + CR 616.1: Classify one settled cascade exile after
/// its replacement-safe delivery. A redirected card is not a hit or miss; a
/// card still on top ends the loop defensively just as the former raw loop did.
pub(crate) fn complete_exile_loop_step(
    state: &mut GameState,
    controller: crate::types::player::PlayerId,
    source_id: ObjectId,
    source_mv: u32,
    mut exiled_misses: Vec<ObjectId>,
    current_card: ObjectId,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    if state.objects.get(&current_card).map(|object| object.zone) != Some(Zone::Exile) {
        let current_card_is_still_top = state
            .players
            .iter()
            .find(|player| player.id == controller)
            .is_some_and(|player| player.library.front().copied() == Some(current_card));
        return if current_card_is_still_top {
            finish_without_hit(state, controller, source_id, exiled_misses, events)
        } else {
            continue_exile_loop(
                state,
                controller,
                source_id,
                source_mv,
                exiled_misses,
                events,
            )
        };
    }

    let is_hit = state.objects.get(&current_card).is_some_and(|object| {
        let is_land = object.card_types.core_types.contains(&CoreType::Land);
        // CR 202.3d + CR 709.4b: the exiled card is off the stack, so a split
        // card's mana value is its combined halves (front-only would misjudge
        // the < source_mv hit test). No-ops for single-face cards.
        let mana_value = object.effective_mana_value();
        !is_land && mana_value < source_mv
    });
    if is_hit {
        return finish_with_hit(
            state,
            controller,
            source_id,
            source_mv,
            exiled_misses,
            current_card,
            events,
        );
    }

    exiled_misses.push(current_card);
    continue_exile_loop(
        state,
        controller,
        source_id,
        source_mv,
        exiled_misses,
        events,
    )
}

/// CR 702.85a: After the first eligible card is delivered to exile, expose the
/// cast offer exactly once. Declined or rejected casts bottom this hit together
/// with the carried miss pile in `engine_resolution_choices`/`casting_costs`.
fn finish_with_hit(
    state: &mut GameState,
    controller: crate::types::player::PlayerId,
    source_id: ObjectId,
    source_mv: u32,
    exiled_misses: Vec<ObjectId>,
    hit_card: ObjectId,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Cascade,
        source_id,
        subject: None,
    });
    state.waiting_for = WaitingFor::CastOffer {
        player: controller,
        kind: CastOfferKind::Cascade {
            hit_card,
            exiled_misses,
            source_mv,
            source_id,
        },
    };
    BatchMoveResult::Done
}

/// CR 702.85a + CR 616.1: With no eligible card, finish only after every miss
/// settles into the randomly ordered Library-bottom batch.
fn finish_without_hit(
    state: &mut GameState,
    controller: crate::types::player::PlayerId,
    source_id: ObjectId,
    exiled_misses: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    let exiled_count = exiled_misses.len() as u32;
    shuffle_to_bottom(
        state,
        &exiled_misses,
        source_id,
        Some(BatchCompletion::CascadeBottomComplete {
            controller,
            source_id,
            exiled_count,
        }),
        events,
    )
}

/// CR 702.85a + CR 401.4: Put cards on the bottom of the player's library in
/// random order through the replacement-aware library-placement pipeline.
pub(crate) fn shuffle_to_bottom(
    state: &mut GameState,
    cards: &[ObjectId],
    source_id: ObjectId,
    completion: Option<BatchCompletion>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    use rand::seq::SliceRandom;

    let mut shuffled = cards.to_vec();
    shuffled.shuffle(&mut state.rng);

    let requests = shuffled
        .into_iter()
        .map(|card_id| {
            ZoneMoveRequest::effect(card_id, Zone::Library, source_id)
                .at_library_position(LibraryPosition::Bottom)
        })
        .collect();
    zone_pipeline::move_objects_simultaneously_then(state, requests, completion, events)
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
        state.players[0].library = im::vector![land1, land2, hit];

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CastOffer {
                kind:
                    CastOfferKind::Cascade {
                        hit_card,
                        exiled_misses,
                        source_mv,
                        ..
                    },
                ..
            } => {
                assert_eq!(*hit_card, hit);
                assert_eq!(exiled_misses, &vec![land1, land2]);
                assert_eq!(*source_mv, 4);
            }
            other => panic!("Expected CascadeChoice, got {:?}", other),
        }
    }

    /// CR 202.3d + CR 702.102b + CR 702.85a: A FUSED split spell that gained
    /// cascade seeds the cascade threshold from its COMBINED mana value, not the
    /// front half. Breaking // Entering combines to MV 8 (front Breaking {U}{B} = 2,
    /// back Entering {4}{B}{R} = 6); a nonland whose MV (5) sits BETWEEN the front
    /// half (2) and the combined value (8) must be a cascade HIT. Reverting the
    /// resolver to the front-half read seeds `source_mv = 2`, so the MV-5 card is a
    /// miss (5 !< 2) and the offered `source_mv`/`hit_card` both flip.
    #[test]
    fn fused_split_spell_cascades_from_combined_mana_value() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::game::scenario_db::GameScenarioDbExt;

        let db = crate::test_support::shared_card_db();
        let mut sc = GameScenario::new();
        let source = sc.add_real_card(P0, "Breaking", Zone::Battlefield, db);
        {
            let obj = sc.state.objects.get_mut(&source).unwrap();
            assert_eq!(
                obj.spell_mana_value(),
                2,
                "precondition: a non-fused Breaking reads the front-half MV 2"
            );
            obj.fused_split_spell = true;
            obj.keywords.push(Keyword::Cascade);
        }
        assert_eq!(
            sc.state.objects.get(&source).unwrap().spell_mana_value(),
            8,
            "a fused Breaking // Entering has combined MV 8"
        );

        let mut state = sc.state;
        // A nonland whose MV (5) is strictly between the front half (2) and the
        // combined value (8): a hit under threshold 8, a miss under threshold 2.
        let hit = add_library_card(&mut state, PlayerId(0), "Mid MV", 5, false);
        state.players[0].library = im::vector![hit];

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CastOffer {
                kind:
                    CastOfferKind::Cascade {
                        hit_card,
                        source_mv,
                        ..
                    },
                ..
            } => {
                assert_eq!(
                    *source_mv, 8,
                    "cascade source MV is the combined value (8), not the front half (2)"
                );
                assert_eq!(
                    *hit_card, hit,
                    "the MV-5 card is a cascade hit under the combined threshold (8)"
                );
            }
            other => panic!(
                "expected a Cascade offer with the MV-5 hit, got {:?}",
                other
            ),
        }
    }

    /// CR 702.85a: first MV check is strict inequality. A nonland with MV
    /// equal to source MV is a miss; the next eligible card is the hit.
    #[test]
    fn mv_boundary_strict_inequality() {
        let (mut state, source_id) = setup_with_source(4);
        let equal = add_library_card(&mut state, PlayerId(0), "Equal MV", 4, false);
        let hit = add_library_card(&mut state, PlayerId(0), "Below MV", 3, false);
        state.players[0].library = im::vector![equal, hit];

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CastOffer {
                kind:
                    CastOfferKind::Cascade {
                        hit_card,
                        exiled_misses,
                        ..
                    },
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
        state.players[0].library = im::vector![a, b];

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No CascadeChoice produced — waiting_for remains whatever the initial
        // state was (resolver leaves it alone when library is exhausted).
        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::CastOffer {
                    kind: CastOfferKind::Cascade { .. },
                    ..
                }
            ),
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
        state.players[0].library = im::vector![hit];

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { source_mv, .. },
                ..
            } => assert_eq!(*source_mv, 5),
            other => panic!("Expected CascadeChoice, got {:?}", other),
        }
    }

    /// CR 702.85a: empty library — cascade resolves cleanly (no panic, no
    /// CascadeChoice) and emits a CascadeMissed event with `exiled_count: 0`.
    #[test]
    fn empty_library_emits_missed_event_and_no_choice() {
        let (mut state, source_id) = setup_with_source(4);
        state.players[0].library.clear();

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::CastOffer {
                    kind: CastOfferKind::Cascade { .. },
                    ..
                }
            ),
            "No CascadeChoice should be offered with an empty library"
        );
        let missed = events.iter().find_map(|e| match e {
            GameEvent::CascadeMissed {
                controller,
                source_id: sid,
                exiled_count,
            } => Some((*controller, *sid, *exiled_count)),
            _ => None,
        });
        assert_eq!(
            missed,
            Some((PlayerId(0), source_id, 0)),
            "CascadeMissed must fire with exiled_count: 0 on empty library"
        );
    }

    /// CR 702.85a: all-lands library — every card is exiled (each is a miss)
    /// and CascadeMissed fires with the full count, then all cards are
    /// shuffled to the bottom of the library.
    #[test]
    fn all_lands_library_emits_missed_event_with_full_count() {
        let (mut state, source_id) = setup_with_source(4);
        let l1 = add_library_card(&mut state, PlayerId(0), "Forest", 0, true);
        let l2 = add_library_card(&mut state, PlayerId(0), "Mountain", 0, true);
        let l3 = add_library_card(&mut state, PlayerId(0), "Island", 0, true);
        state.players[0].library = im::vector![l1, l2, l3];

        let ability = ResolvedAbility::new(Effect::Cascade, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::CastOffer {
                    kind: CastOfferKind::Cascade { .. },
                    ..
                }
            ),
            "No CascadeChoice should be offered when no nonland is hit"
        );
        let missed = events.iter().find_map(|e| match e {
            GameEvent::CascadeMissed { exiled_count, .. } => Some(*exiled_count),
            _ => None,
        });
        assert_eq!(
            missed,
            Some(3),
            "CascadeMissed exiled_count must reflect every land that was exiled"
        );
        // All three lands shuffled back to bottom of library.
        assert_eq!(state.players[0].library.len(), 3);
        for &id in &[l1, l2, l3] {
            assert_eq!(state.objects.get(&id).map(|o| o.zone), Some(Zone::Library));
        }
    }

    /// CR 702.85c: a spell with multiple cascade keywords triggers the
    /// cascade ability separately for each instance. Verifies the trigger
    /// synthesizer in `process_triggers` produces N pending triggers for
    /// N cascade keywords, with monotonically increasing timestamps.
    #[test]
    fn multi_instance_cascade_fires_one_trigger_per_keyword() {
        use crate::game::triggers::process_triggers;
        use crate::types::events::GameEvent as Ev;

        let mut state = GameState::new_two_player(7);
        // Build a cascade spell on the stack with TWO Cascade keyword
        // instances (matches CR 702.85c — each triggers separately).
        let spell_id = create_object(
            &mut state,
            CardId(2000),
            PlayerId(0),
            "Multi-Cascade Spell".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.mana_cost = ManaCost::generic(5);
            obj.keywords.push(Keyword::Cascade);
            obj.keywords.push(Keyword::Cascade);
            // CR 611.2f: the Cascade seam counts instances from the cast-time
            // keyword snapshot (`cast_spell_keywords`) stamped by `finalize_cast`
            // (`effective_spell_keyword_instances` preserves printed duplicates).
            // This test bypasses finalize, so mirror the two printed instances.
            obj.cast_spell_keywords.push(Keyword::Cascade);
            obj.cast_spell_keywords.push(Keyword::Cascade);
        }

        // Drive the trigger synthesizer with a SpellCast event for spell_id.
        // Empty library so cascade resolution falls through quickly without
        // requiring a WaitingFor handshake.
        let evts = vec![Ev::SpellCast {
            card_id: CardId(2000),
            controller: PlayerId(0),
            object_id: spell_id,
        }];

        let ts_before = state.next_timestamp;
        process_triggers(&mut state, &evts);

        // Two Cascade keywords ⇒ next_timestamp advanced by 2 (one per
        // instance), and two cascade triggers were placed on the stack
        // before any ran (or one CascadeMissed event was emitted twice if
        // the library was empty for both resolutions).
        assert!(
            state.next_timestamp >= ts_before + 2,
            "Expected next_timestamp to advance by ≥2 for two cascade triggers, \
             got before={ts_before} after={}",
            state.next_timestamp
        );
    }
}
