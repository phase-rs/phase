use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 701.17a: Mill N — put the top N cards of a player's library into their graveyard.
/// When `destination` is set to a zone other than Graveyard (e.g., Exile or Hand),
/// cards are moved there instead -- building block for top-of-library move patterns.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_cards, destination, target_player) = match &ability.effect {
        Effect::Mill {
            count,
            destination,
            target,
        } => (
            // CR 107.1b: Resolve with full ability context so `QuantityRef::Variable { "X" }`
            // reads the caster-chosen X from the resolving ability, and clamp a
            // negative result to zero before the `as usize` cast. Mill shares the
            // Draw/Mill/Discard dynamic-count parser, so a subtractive count
            // ("mill cards equal to A minus B" with B > A) resolves negative;
            // without the clamp `-1 as usize` wraps huge and the downstream
            // library-size `min` mills the entire library instead of nothing.
            // Mirrors the guard in `draw.rs` / `discard.rs`.
            resolve_quantity_with_targets(state, count, ability).max(0) as usize,
            *destination,
            // CR 701.17a + CR 115.1: Mirror Draw/Scry/Surveil — context-ref
            // target filters (Controller, PostReplacementSourceController,
            // ParentTargetController, etc.) must consult state slots, not
            // `ability.targets`, so a Mill sub-ability chained off a Player-
            // targeted parent does not inherit the parent's chosen player.
            super::resolve_player_for_context_ref(state, ability, target),
        ),
        _ => (1, Zone::Graveyard, ability.controller),
    };

    if destination == Zone::Graveyard {
        let proposed = ProposedEvent::Mill {
            player_id: target_player,
            count: num_cards as u32,
            destination,
            applied: Default::default(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                // CR 616.1: a per-card pause leaves `state.waiting_for` set and
                // the tail parked; bail before emitting `EffectResolved` so the
                // surfaced prompt is not clobbered. The resume path
                // (`zone_pipeline::drain_pending_batch_deliveries`) finishes the
                // batch.
                if !apply_mill_after_replacement(state, event, events)? {
                    return Ok(());
                }
            }
            ReplacementResult::Prevented => {}
            ReplacementResult::NeedsChoice(player) => {
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
        }
    } else if !apply_mill_after_replacement(
        state,
        ProposedEvent::Mill {
            player_id: target_player,
            count: num_cards as u32,
            destination,
            applied: Default::default(),
        },
        events,
    )? {
        // CR 616.1: per-card pause (see above) — bail before `EffectResolved`.
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 701.17a-b: Apply an accepted mill event after replacement effects have
/// had a chance to modify the count.
///
/// Returns `true` when every milled card was delivered, `false` when a per-card
/// `Moved` replacement surfaced a CR 616.1 ordering choice that parked the batch
/// (`state.waiting_for` is left set, the undelivered tail in
/// the active `BatchDelivery` frame). Callers that reset `state.waiting_for`
/// after applying an accepted event MUST early-return on `false` so they don't
/// clobber the parked prompt (mirrors the `apply_etb_counters` early-return
/// precedent in `handle_replacement_choice`).
pub fn apply_mill_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> Result<bool, EffectError> {
    let ProposedEvent::Mill {
        player_id,
        count,
        destination,
        ..
    } = event
    else {
        return Ok(true);
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == player_id)
        .ok_or(EffectError::PlayerNotFound)?;

    // CR 701.17b: A player can't mill more cards than are in their library;
    // if instructed to, they mill as many as possible.
    let count = (count as usize).min(player.library.len());
    let cards_to_mill: Vec<_> = player.library.iter().take(count).copied().collect();
    state.last_effect_count = Some(cards_to_mill.len() as i32);

    // CR 701.17a + CR 614.6: Route each milled card through the zone-change
    // pipeline (the shared `zone_pipeline::move_objects_simultaneously` batch
    // entry) rather than a raw `zones::move_to_zone`. The raw move never
    // proposed a per-card ZoneChange, so `Moved` redirects ("if a card would be
    // put into a graveyard from anywhere, exile it instead" — Rest in Peace /
    // Leyline of the Void class) never fired for milled cards. The batch entry
    // proposes each inner ZoneChange and consults those replacements before
    // delivery, fixing the known bug.
    //
    // Attribution: the milled card itself anchors the `Effect` cause (mill to a
    // graveyard creates no exile-link, and a `Moved` replacement's `valid_card`
    // is evaluated against the moved card, so this matches the pre-pipeline raw
    // behavior while enabling the replacement consult).
    //
    // CR 616.1: a per-card ordering choice (two simultaneous graveyard→exile
    // redirects) parks `state.waiting_for` + the undelivered tail in
    // the active `BatchDelivery` frame; the replacement-choice resume path
    // (`zone_pipeline::drain_pending_batch_deliveries`) finishes the batch.
    let reqs: Vec<ZoneMoveRequest> = cards_to_mill
        .iter()
        .map(|&obj_id| ZoneMoveRequest::effect(obj_id, destination, obj_id))
        .collect();
    // CR 701.17a: milling is a move *toward the graveyard* — "that player puts
    // that many cards from the top of their library into their graveyard".
    // `Effect::Mill` with any other declared destination is the shared
    // top-of-library move building block and is not a mill, so it emits nothing.
    // `effects::this_way_cause_for_effect` is the sibling authority on the same
    // predicate and is kept in step with this conjunct.
    //
    // CR 603.2c + CR 616.1: the `Milled` events ride the batch completion rather
    // than a synchronous event window. A per-card ordering choice (two graveyard
    // redirects colliding) parks the undelivered tail, and the resume path drains
    // it with a FRESH event vector — so a window read here would omit every card
    // delivered after the pause, and those cards leave the library without ever
    // firing a milled trigger. The completion runs exactly once after the whole
    // batch settles, on the synchronous and the resumed path alike.
    let completion = (destination == Zone::Graveyard).then(|| {
        crate::types::game_state::BatchCompletion::MilledDeliveryComplete {
            player_id,
            cards: cards_to_mill.clone(),
        }
    });
    let delivered = matches!(
        zone_pipeline::move_objects_simultaneously_then(state, reqs, completion, events),
        BatchMoveResult::Done
    );

    Ok(delivered)
}

/// CR 701.17a + CR 603.2c: emit one `Milled` per card that actually left the
/// library, once the whole mill batch has settled.
///
/// CR 614.6 makes the modified event the one that occurred, so a graveyard-diverting
/// replacement that redirects a card back into its library moved no card out of it and
/// yields no `Milled`. The settled zone is read from `state` rather than from an event
/// window — the same authority the other delivery completions use — because a tail
/// parked by a CR 616.1 ordering choice resumes with a fresh event vector and no window
/// spans the pause.
pub(crate) fn complete_mill_delivery(
    state: &mut GameState,
    player_id: crate::types::player::PlayerId,
    cards: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    for object_id in cards {
        let Some(zone) = state.objects.get(&object_id).map(|object| object.zone) else {
            continue;
        };
        if zone == Zone::Library {
            continue;
        }
        events.push(GameEvent::Milled {
            player_id,
            object_id,
            to: zone,
        });
    }
    BatchMoveResult::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::effects::resolve_ability_chain;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, PlayerFilter, QuantityExpr, QuantityRef,
        ReplacementDefinition, ReplacementPlayerScope, TargetFilter, TargetRef,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn make_mill_ability(
        num_cards: u32,
        targets: Vec<TargetRef>,
        destination: Zone,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed {
                    value: num_cards as i32,
                },
                target: TargetFilter::Any,
                destination,
            },
            targets,
            ObjectId(100),
            PlayerId(0),
        )
    }

    /// Every `GameEvent::Milled` in `events`, as `(player, object, destination)`.
    fn milled_events(events: &[GameEvent]) -> Vec<(PlayerId, ObjectId, Zone)> {
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::Milled {
                    player_id,
                    object_id,
                    to,
                } => Some((*player_id, *object_id, *to)),
                _ => None,
            })
            .collect()
    }

    /// Every library-origin `ZoneChanged` in `events`, as `(object, destination)`.
    fn library_departures(events: &[GameEvent]) -> Vec<(ObjectId, Zone)> {
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Library),
                    to,
                    ..
                } => Some((*object_id, *to)),
                _ => None,
            })
            .collect()
    }

    /// CR 614.6: a graveyard→exile `Moved` redirect (Rest in Peace / Leyline of
    /// the Void class). Two of these are simultaneously applicable to each milled
    /// card, so the CR 616.1 materiality classifier prompts for ordering per card.
    fn change_zone_effect(destination: Zone, target: TargetFilter) -> Effect {
        use crate::types::zones::EtbTapState;
        Effect::ChangeZone {
            destination,
            origin: None,
            target,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        }
    }

    fn graveyard_exile_redirect(description: &str) -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .destination_zone(Zone::Graveyard)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                change_zone_effect(Zone::Exile, TargetFilter::SelfRef),
            ))
            .description(description.to_string())
    }

    /// P1 regression (round-2 review): `apply_mill_after_replacement` MUST report
    /// a per-card pause to its caller (return `false`) rather than swallow it.
    ///
    /// The nested Mill-event resume path (`handle_replacement_choice`'s Mill arm)
    /// applies an accepted Mill event and then unconditionally resets
    /// `waiting_for` to Priority. If `apply_mill_after_replacement` swallowed a
    /// per-card CR 616.1 pause (the old `let _ =`), that reset would clobber the
    /// parked prompt and strand the first paused milled card. This test drives the
    /// shared seam directly: with two simultaneously-applicable graveyard→exile
    /// redirects, the first milled card surfaces a CR 616.1 ordering prompt, so
    /// the helper must return `false`, leave `state.waiting_for` set to that
    /// prompt, and park the undelivered tail.
    #[test]
    fn apply_mill_after_replacement_reports_per_card_pause_to_caller() {
        let mut state = GameState::new_two_player(42);

        for (description, source_card) in [
            ("Rest in Peace redirect", CardId(1000)),
            ("Leyline of the Void redirect", CardId(1001)),
        ] {
            let source = create_object(
                &mut state,
                source_card,
                PlayerId(0),
                "Redirect Source".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .replacement_definitions = vec![graveyard_exile_redirect(description)].into();
        }

        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Milled {i}"),
                Zone::Library,
            );
        }

        let mut events = Vec::new();
        let delivered = apply_mill_after_replacement(
            &mut state,
            ProposedEvent::Mill {
                player_id: PlayerId(1),
                count: 3,
                destination: Zone::Graveyard,
                applied: Default::default(),
            },
            &mut events,
        )
        .expect("mill applies");

        // The pause signal must reach the caller so it can early-return before
        // resetting `waiting_for`.
        assert!(
            !delivered,
            "a per-card CR 616.1 pause must be reported as a non-delivery (false)"
        );
        assert!(
            matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::ReplacementChoice { .. }
            ),
            "the per-card ordering prompt must be parked in waiting_for"
        );
        assert!(
            state.active_batch_delivery().is_some(),
            "the undelivered tail must be stashed for the resume path"
        );

        // CR 616.1 + CR 701.17a: a parked tail has not been delivered, so no card
        // has left the library yet and the window must be empty on both channels.
        // The assertions above prove the seam ran, so these zeros are not the
        // instrument failing to fire.
        assert!(library_departures(&events).is_empty());
        assert!(milled_events(&events).is_empty());
    }

    /// V8 — CR 701.17a: milling is a move toward the graveyard. `Effect::Mill`
    /// with any other declared destination is the shared top-of-library move
    /// building block (Scroll Rack) and emits no mill action event. The two legs
    /// differ only in the `destination` argument.
    #[test]
    fn mill_emits_the_action_event_only_for_a_graveyard_destination() {
        let build = |destination| {
            let mut state = GameState::new_two_player(42);
            for i in 0..5 {
                create_object(
                    &mut state,
                    CardId(i + 1),
                    PlayerId(1),
                    format!("Card {i}"),
                    Zone::Library,
                );
            }
            let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))], destination);
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();
            events
        };

        assert!(
            milled_events(&build(Zone::Hand)).is_empty(),
            "a top-of-library move to hand is not a mill (CR 701.17a)"
        );
        // The nonzero leg is the live control for the zero above.
        assert_eq!(milled_events(&build(Zone::Graveyard)).len(), 3);
    }

    /// V4 — CR 701.17a + CR 701.17c: `Effect::Mill { target: Controller }` under
    /// `player_scope: Opponent` re-enters the effect once per opponent against
    /// ONE shared `events` vec. Each emitted `Milled` must carry the player whose
    /// library its card left and the destination that card actually reached.
    /// Only the second opponent's cards are diverted, so the undiverted opponent
    /// is the same-invocation reach-guard.
    #[test]
    fn player_scope_repeat_binds_each_mill_to_its_own_player_and_destination() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        // The redirect keys on the card name, so it catches exactly P2's cards.
        for (player, name) in [(1u8, "Plain Card"), (2u8, "Redirected Card")] {
            for i in 0u64..6 {
                create_object(
                    &mut state,
                    CardId(100 + (player as u64) * 10 + i),
                    PlayerId(player),
                    name.to_string(),
                    Zone::Library,
                );
            }
        }
        let redirect_source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Redirect Source".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&redirect_source)
            .unwrap()
            .replacement_definitions = vec![graveyard_exile_redirect("P2-only redirect")
            .valid_card(TargetFilter::Named {
                name: "Redirected Card".to_string(),
            })]
        .into();

        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let milled = milled_events(&events);
        assert_eq!(milled.len(), 6, "three cards milled from each opponent");
        // The undiverted opponent — the reach-guard for the diverted one.
        let p1: Vec<_> = milled.iter().filter(|m| m.0 == PlayerId(1)).collect();
        assert_eq!(p1.len(), 3);
        assert!(p1.iter().all(|m| m.2 == Zone::Graveyard));
        // The diverted opponent: same action, CR 701.17c destination.
        let p2: Vec<_> = milled.iter().filter(|m| m.0 == PlayerId(2)).collect();
        assert_eq!(p2.len(), 3);
        assert!(p2.iter().all(|m| m.2 == Zone::Exile));
        // Per-player attribution: no event names a card from the other library.
        for (player, object, _) in &milled {
            assert_eq!(
                state.objects[object].owner, *player,
                "each Milled must name the player whose library its card left"
            );
        }
    }

    /// V5 — CR 603.2c: one `Milled` per milled card, and only for this
    /// invocation's own cards. The window is not closed over them:
    /// `apply_zone_delivery_tail` drains a stashed post-replacement
    /// continuation inside `move_objects_simultaneously`, so a redirect
    /// carrying a mill rider lands a nested mill's library departures in the
    /// outer invocation's window. A `ChangeZone` rider does not reach this
    /// seam — `EventModifiers::is_event_modifier_effect` matches
    /// `Effect::ChangeZone` unconditionally, so the chain is absorbed into the
    /// event and no continuation stashes.
    #[test]
    fn a_mill_rider_in_the_same_window_stamps_each_card_exactly_once() {
        let mut state = GameState::new_two_player(42);
        // Strictly more cards than the outer mill takes: the rider's own
        // departures are what make the window wider than `cards_to_mill`.
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {i}"),
                Zone::Library,
            );
        }
        let redirect_source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Redirect Source".to_string(),
            Zone::Battlefield,
        );
        // The redirect link is an event modifier, so
        // `EventModifiers::first_non_modifier_ability` walks past it and stashes
        // the mill rider as the continuation the delivery tail drains.
        state
            .objects
            .get_mut(&redirect_source)
            .unwrap()
            .replacement_definitions = vec![graveyard_exile_redirect("redirect with a mill rider")
            .execute(
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    change_zone_effect(Zone::Exile, TargetFilter::SelfRef),
                )
                .sub_ability(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Mill {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                        destination: Zone::Graveyard,
                    },
                )),
            )]
        .into();

        let mut events = Vec::new();
        apply_mill_after_replacement(
            &mut state,
            ProposedEvent::Mill {
                player_id: PlayerId(1),
                count: 1,
                destination: Zone::Graveyard,
                applied: Default::default(),
            },
            &mut events,
        )
        .expect("mill applies");

        // Reach-guard: the window must carry more library departures than the
        // one card this invocation milled, or the per-card uniqueness assertion
        // below holds vacuously.
        let departures = library_departures(&events);
        assert_eq!(departures.len(), 4, "got {departures:?}");
        assert!(departures.iter().all(|(_, to)| *to == Zone::Exile));

        let milled = milled_events(&events);
        assert_eq!(milled.len(), departures.len(), "got {milled:?}");
        for (object, _) in &departures {
            let stamps = milled.iter().filter(|(_, id, _)| id == object).count();
            assert_eq!(stamps, 1, "{object:?} stamped {stamps}x: {milled:?}");
        }
    }

    /// V12 — CR 614.6 + CR 701.17a: the admitted member the departure conjunct
    /// must refuse. A graveyard-diverting replacement that puts the card back in
    /// the library leaves it where it started, so CR 701.17a's action never
    /// happened and CR 701.17c's "the zone it moved to from the library" has no
    /// referent — even though the pipeline still emits a `Library -> Library`
    /// `ZoneChanged`.
    #[test]
    fn a_replacement_that_returns_the_card_to_the_library_is_not_a_mill() {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {i}"),
                Zone::Library,
            );
        }
        let redirect_source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Redirect Source".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&redirect_source)
            .unwrap()
            .replacement_definitions = vec![graveyard_exile_redirect("back to the library")
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                change_zone_effect(Zone::Library, TargetFilter::SelfRef),
            ))]
        .into();

        let mut events = Vec::new();
        apply_mill_after_replacement(
            &mut state,
            ProposedEvent::Mill {
                player_id: PlayerId(1),
                count: 3,
                destination: Zone::Graveyard,
                applied: Default::default(),
            },
            &mut events,
        )
        .expect("mill applies");

        // The seam really ran: the pipeline emitted a library-origin event per
        // card. This is the live control for the zero below.
        let departures = library_departures(&events);
        assert_eq!(departures.len(), 3, "got {departures:?}");
        assert!(departures.iter().all(|(_, to)| *to == Zone::Library));

        assert!(
            milled_events(&events).is_empty(),
            "a card that never left the library was not milled"
        );
        assert_eq!(state.players[1].library.len(), 3);
        assert!(state.players[1].graveyard.is_empty());
    }

    #[test]
    fn mill_3_moves_top_3_from_library_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let top_3: Vec<_> = state.players[1]
            .library
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>();

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))], Zone::Graveyard);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].library.len(), 2);
        assert_eq!(state.players[1].graveyard.len(), 3);
        for id in &top_3 {
            assert!(state.players[1].graveyard.contains(id));
        }
    }

    #[test]
    fn mill_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[1].library.is_empty());

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))], Zone::Graveyard);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.players[1].graveyard.is_empty());
    }

    #[test]
    fn mill_with_fewer_cards_than_requested_mills_available() {
        let mut state = GameState::new_two_player(42);
        for i in 0..2 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let ability = make_mill_ability(5, vec![TargetRef::Player(PlayerId(1))], Zone::Graveyard);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].library.is_empty());
        assert_eq!(state.players[1].graveyard.len(), 2);
    }

    #[test]
    fn opponent_mill_replacement_doubles_resolved_mill_count() {
        let mut state = GameState::new_two_player(42);
        let replacement_source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Mill Doubler".to_string(),
            Zone::Battlefield,
        );
        let mut replacement =
            ReplacementDefinition::new(ReplacementEvent::Mill).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mill {
                    count: QuantityExpr::Multiply {
                        factor: 2,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount,
                        }),
                    },
                    target: TargetFilter::Controller,
                    destination: Zone::Graveyard,
                },
            ));
        replacement.valid_player = Some(ReplacementPlayerScope::Opponent);
        state
            .objects
            .get_mut(&replacement_source)
            .unwrap()
            .replacement_definitions = vec![replacement].into();
        for i in 0..8 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))], Zone::Graveyard);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].library.len(), 2);
        assert_eq!(state.players[1].graveyard.len(), 6);
    }

    #[test]
    fn opponent_mill_replacement_does_not_apply_to_controller_mill() {
        let mut state = GameState::new_two_player(42);
        let replacement_source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Mill Doubler".to_string(),
            Zone::Battlefield,
        );
        let mut replacement =
            ReplacementDefinition::new(ReplacementEvent::Mill).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mill {
                    count: QuantityExpr::Multiply {
                        factor: 2,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount,
                        }),
                    },
                    target: TargetFilter::Controller,
                    destination: Zone::Graveyard,
                },
            ));
        replacement.valid_player = Some(ReplacementPlayerScope::Opponent);
        state
            .objects
            .get_mut(&replacement_source)
            .unwrap()
            .replacement_definitions = vec![replacement].into();
        for i in 0..8 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(0))], Zone::Graveyard);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].library.len(), 5);
        assert_eq!(state.players[0].graveyard.len(), 3);
    }

    /// Issue #310 (Maddening Cacophony / Fractured Sanity): "Each opponent
    /// mills N cards." parses as `Effect::Mill { target: Controller }` with
    /// `player_scope: Opponent` on the surrounding ability. The
    /// player_scope iteration loop must rebind `controller` to each opponent
    /// per CR 608.2 + CR 109.5 so the inner Mill effect mills the iterated
    /// opponent — not the printed controller.
    ///
    /// Three-player coverage: opponents must be expanded in APNAP order so the
    /// "each opponent" semantics is universal, not just "the next opponent."
    #[test]
    fn player_scope_opponent_mill_targets_each_opponent_three_player_apnap() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        for p in 0u8..3 {
            for i in 0u64..6 {
                create_object(
                    &mut state,
                    CardId(100 + (p as u64) * 10 + i),
                    PlayerId(p),
                    format!("P{p} Library {i}"),
                    Zone::Library,
                );
            }
        }

        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].graveyard.len(), 0, "caster not milled");
        assert_eq!(state.players[1].graveyard.len(), 3, "opponent 1 milled");
        assert_eq!(state.players[2].graveyard.len(), 3, "opponent 2 milled");
    }

    #[test]
    fn player_scope_opponent_mill_targets_each_opponent_not_controller() {
        let mut state = GameState::new_two_player(42);
        for i in 0..8 {
            create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("P0 Library {i}"),
                Zone::Library,
            );
            create_object(
                &mut state,
                CardId(200 + i),
                PlayerId(1),
                format!("P1 Library {i}"),
                Zone::Library,
            );
        }

        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Controller (PlayerId(0)) MUST NOT be milled — only opponents.
        assert_eq!(
            state.players[0].graveyard.len(),
            0,
            "controller must not be milled by Each opponent mills"
        );
        assert_eq!(
            state.players[1].graveyard.len(),
            3,
            "each opponent must be milled"
        );
    }

    /// Issue #310 (Maddening Cacophony kicker mode): "Each opponent mills
    /// half their library, rounded up." Parses as
    /// `Effect::Mill { count: ZoneCardCount{scope: ScopedPlayer, ...}/2 ceil,
    /// target: Controller }` with `player_scope: Opponent` after the parser
    /// rewrite at `parser/oracle_effect/mod.rs` promotes the
    /// `TargetZoneCardCount{Library}` form.
    ///
    /// CR 608.2 + CR 109.5: `CountScope::ScopedPlayer` MUST bind to the
    /// iterated player's library — not the caster's. A three-player game
    /// with libraries of differing sizes (caster: 4, opponent 1: 6,
    /// opponent 2: 10) exposes the bug clearly: opponent 1 must mill
    /// `ceil(6/2)=3`, opponent 2 must mill `ceil(10/2)=5`, and the caster
    /// must NOT be milled at all. Pre-fix the rewrite emitted
    /// `CountScope::Controller`, which counted the caster's 4-card library
    /// for both, milling each opponent `ceil(4/2)=2`.
    #[test]
    fn player_scope_opponent_mill_half_their_library_uses_iterated_library() {
        use crate::types::ability::{CountScope, RoundingMode, ZoneRef};
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        // Library sizes: caster (P0) = 4, opponent 1 (P1) = 6, opponent 2 (P2) = 10.
        // Differing sizes prove the count is computed per-iterated-player,
        // not from the caster's library.
        let library_sizes = [4u64, 6u64, 10u64];
        for (p, &size) in library_sizes.iter().enumerate() {
            for i in 0..size {
                create_object(
                    &mut state,
                    CardId(100 + (p as u64) * 100 + i),
                    PlayerId(p as u8),
                    format!("P{p} Library {i}"),
                    Zone::Library,
                );
            }
        }

        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::DivideRounded {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ZoneCardCount {
                            zone: ZoneRef::Library,
                            card_types: vec![],
                            scope: CountScope::ScopedPlayer,
                            filter: None,
                        },
                    }),
                    divisor: 2,
                    rounding: RoundingMode::Up,
                },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].graveyard.len(),
            0,
            "caster must NOT be milled — player_scope=Opponent only iterates opponents"
        );
        assert_eq!(
            state.players[1].graveyard.len(),
            3,
            "opponent 1 (library=6) must mill ceil(6/2)=3 — counted from their library, not caster's"
        );
        assert_eq!(
            state.players[2].graveyard.len(),
            5,
            "opponent 2 (library=10) must mill ceil(10/2)=5 — counted from their library, not caster's"
        );
    }

    /// Issue #310: `CountScope::Controller` (caster's "your library") MUST
    /// continue to mean the caster — even inside a `player_scope` iteration.
    /// "Each player sacrifices a land for each card in YOUR hand"
    /// (Thoughts of Ruin shape) is the canonical case: the count is the
    /// caster's hand size regardless of which iterated player is sacrificing.
    /// Pin this so any future change to the per-iteration semantics keeps
    /// `Controller` distinct from `ScopedPlayer`.
    #[test]
    fn player_scope_controller_count_scope_remains_caster_perspective() {
        use crate::types::ability::{CountScope, ZoneRef};
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        // Caster (P0) hand: 5 cards. Iterated players (P1, P2) hand: 1 card each.
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("Caster Hand {i}"),
                Zone::Hand,
            );
        }
        for p in 1u8..3 {
            create_object(
                &mut state,
                CardId(200 + u64::from(p)),
                PlayerId(p),
                format!("P{p} Hand"),
                Zone::Hand,
            );
        }
        // P1 / P2 each have a 10-card library so Mill is observable.
        for p in 1u8..3 {
            for i in 0..10 {
                create_object(
                    &mut state,
                    CardId(300 + u64::from(p) * 20 + i),
                    PlayerId(p),
                    format!("P{p} Library {i}"),
                    Zone::Library,
                );
            }
        }

        // Mill N where N = "cards in your hand" (CountScope::Controller).
        // player_scope=Opponent → each opponent mills 5 (caster's hand size),
        // not 1 (their own hand size).
        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Hand,
                        card_types: vec![],
                        scope: CountScope::Controller,
                        filter: None,
                    },
                },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].graveyard.len(), 0, "caster not milled");
        assert_eq!(
            state.players[1].graveyard.len(),
            5,
            "opponent 1 mills 5 — count uses CASTER's hand size (5), not their own (1)"
        );
        assert_eq!(
            state.players[2].graveyard.len(),
            5,
            "opponent 2 mills 5 — count uses CASTER's hand size (5), not their own (1)"
        );
    }

    /// Issue #477 — Renegade Reaper: "Mill four cards. If at least one Angel
    /// card is milled this way, you gain 4 life." The `GainLife` sub-ability
    /// carries `AbilityCondition::ZoneChangedThisWay { Angel }`; the life gain
    /// must fire ONLY when an Angel was among the milled cards.
    ///
    /// CR 608.2c + CR 400.7: the conditional gate references the cards moved
    /// by the preceding `Mill` this resolution (`last_zone_changed_ids`).
    ///
    /// This drives the real pipeline: `resolve_ability_chain` → `Mill` (emits
    /// `ZoneChanged`, populates `last_zone_changed_ids`) → sub-ability
    /// condition check (`evaluate_condition` for `ZoneChangedThisWay`) →
    /// `GainLife`. It is a runtime test, not a shape test.
    fn renegade_reaper_chain() -> ResolvedAbility {
        use crate::types::ability::{
            AbilityCondition, TargetFilter as TF, TypeFilter, TypedFilter,
        };
        ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 4 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability({
            let mut gain = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 4 },
                    player: TargetFilter::Controller,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            );
            gain.condition = Some(AbilityCondition::ZoneChangedThisWay {
                filter: TF::Typed(TypedFilter::new(TypeFilter::Subtype("Angel".to_string()))),
                destination: None,
            });
            gain
        })
    }

    #[test]
    fn renegade_reaper_gains_life_only_when_angel_milled() {
        // --- Case A: an Angel IS among the milled cards → life gained. ---
        let mut state = GameState::new_two_player(42);
        let life_before = state.players[0].life;
        // Top of library: 3 plain cards + 1 Angel within the milled top-4.
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Plain {i}"),
                Zone::Library,
            );
        }
        let angel = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Test Angel".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&angel)
            .unwrap()
            .card_types
            .subtypes
            .push("Angel".to_string());

        let ability = renegade_reaper_chain();
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].graveyard.len(), 4, "all 4 cards milled");
        assert_eq!(
            state.players[0].life,
            life_before + 4,
            "life must increase by 4 — an Angel was milled this way"
        );

        // --- Case B: NO Angel among the milled cards → life unchanged. ---
        let mut state = GameState::new_two_player(42);
        let life_before = state.players[0].life;
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Plain {i}"),
                Zone::Library,
            );
        }
        let ability = renegade_reaper_chain();
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].graveyard.len(), 4, "all 4 cards milled");
        assert_eq!(
            state.players[0].life, life_before,
            "life must be unchanged — no Angel was milled this way"
        );
    }

    /// CR 107.1b: a mill count that resolves negative must clamp to 0, not wrap
    /// through the `as usize` cast and mill the whole library. Mill shares the
    /// Draw/Mill/Discard dynamic-count parser, so "mill cards equal to A minus B"
    /// (with B > A) resolves negative. Revert-probe: without the `.max(0)` the
    /// downstream library-size `min` mills the target's entire library instead of
    /// nothing.
    #[test]
    fn mill_negative_count_clamps_to_zero() {
        use crate::types::ability::{AggregateFunction, PlayerScope};

        let mut state = GameState::new_two_player(7);
        // Controller (P0): 1 card in hand, 2 in library. Opponent (P1): 3 in hand.
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hand".into(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "LibA".into(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "LibB".into(),
            Zone::Library,
        );
        for i in 0..3u64 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(1),
                "Theirs".into(),
                Zone::Hand,
            );
        }

        // count = HandSize{You} − HandSize{Opponent} = 1 − 3 = −2.
        let count = QuantityExpr::Sum {
            exprs: vec![
                QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Opponent {
                                aggregate: AggregateFunction::Sum,
                            },
                        },
                    }),
                },
            ],
        };
        let ability = ResolvedAbility::new(
            Effect::Mill {
                count,
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].library.len(),
            2,
            "CR 107.1b: a negative mill count must mill 0, not the whole library"
        );
        assert!(
            state.players[0].graveyard.is_empty(),
            "no card may be milled"
        );
    }
}
