use std::collections::HashSet;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{CounterPlacement, ProposedEvent, ReplacementId};
use crate::types::zones::Zone;

/// CR 701.50a: Connive — draw N cards, then discard N cards. For each nonland
/// card discarded this way, put a +1/+1 counter on the conniving creature.
///
/// If the player has more cards than `count` after drawing, sets
/// `WaitingFor::ConniveDiscard` for the player to choose which cards to discard.
/// Otherwise auto-discards (0 or 1 card) and adds counters inline.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.50d + CR 107.3i: Dynamic connive counts (e.g. creatures that died
    // this turn) resolve at ability resolution via the shared quantity pipeline.
    let count = match &ability.effect {
        Effect::Connive { count, .. } => {
            resolve_quantity_with_targets(state, count, ability).max(0) as u32
        }
        _ => 1,
    };

    // Determine conniving creature: first object target, or source
    let conniver_id = ability
        .targets
        .iter()
        .find_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .unwrap_or(ability.source_id);

    // CR 701.50a + CR 614.1a: Consult connive replacements (Leader,
    // Super-Genius — "If a creature you control would connive, instead you draw
    // a card, then that creature connives") before the draw/discard/counter
    // pipeline runs. The top-level resolve seeds an empty `applied` set.
    propose_connive(state, conniver_id, count, HashSet::new(), events)
}

/// CR 701.50a + CR 614.1a + CR 616.1f: Propose a connive action through the
/// replacement pipeline, then run the surviving connive once. `applied` carries
/// the replacements already applied to this connive event chain so the pipeline
/// excludes them (CR 614.5) — the top-level resolver seeds it empty; a connive
/// replacement's nested "then that creature connives" link seeds it with the
/// just-applied replacement marked, so the process repeats over only the
/// still-applicable connive replacements (CR 616.1f) without self-invoking.
pub(crate) fn propose_connive(
    state: &mut GameState,
    conniver_id: ObjectId,
    count: u32,
    applied: HashSet<ReplacementId>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let proposed = ProposedEvent::Connive {
        object_id: conniver_id,
        count,
        applied,
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(ProposedEvent::Connive {
            object_id,
            count: final_count,
            ..
        }) => resolve_connive_effect(state, object_id, final_count, events),
        ReplacementResult::Execute(_) => {
            // Defensive: a non-Connive survivor cannot occur (the pipeline only
            // substitutes same-variant survivor events for count-modifier
            // replacements). Fall back to the original count.
            resolve_connive_effect(state, conniver_id, count, events)
        }
        ReplacementResult::Prevented => {
            // CR 701.50f + CR 701.50b: A replacement fully replaced the connive
            // action (Leader's applier already ran the modified action and its
            // own `EffectResolved`). Nothing more to do here.
            Ok(())
        }
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            Ok(())
        }
    }
}

/// CR 701.50a: Run the connive draw/discard/counter pipeline without consulting
/// connive replacements. Used both by `resolve` after the replacement pipeline
/// clears the event and by the connive replacement applier's "instead [...],
/// then that creature connives" chain (nested connives must not re-enter the
/// replacement pipeline — CR 614.5).
pub(crate) fn resolve_connive_effect(
    state: &mut GameState,
    conniver_id: ObjectId,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.50e: If a permanent would connive 0, no connive event occurs and
    // abilities that trigger whenever a permanent connives won't trigger. Return
    // before any draw/discard/counter, before parking a ConniveDiscard, and
    // without emitting EffectResolved{Connive} (the event match_connives keys on).
    // Reachable via the dynamic count path (Spymaster's Vault X = creatures that
    // died this turn) and via replacement count-modifiers reducing the count to 0
    // (engine_replacement.rs Execute survivor). Printed "Connive N" (CR 701.50d)
    // is always N>=1, so this never regresses the normal count>=1 flow.
    if count == 0 {
        return Ok(());
    }

    // CR 701.50a: The conviving permanent's controller draws and discards.
    let controller = state
        .objects
        .get(&conniver_id)
        .map(|obj| obj.controller)
        .unwrap_or(PlayerId(0));

    // Step 1: Draw `count` cards for the controller.
    // CR 614.1a + CR 614.6 + CR 704.3: Route through the single-authority
    // helper so post-replacement continuations drain in the same step.
    let result = super::draw::draw_through_replacement(
        state,
        controller,
        count,
        events,
        |state, event, events| {
            let ProposedEvent::Draw {
                player_id,
                count: draw_count,
                ..
            } = event
            else {
                return;
            };
            // CR 121.1 + CR 613.11: route card selection through the single
            // `select_cards_to_draw` authority so a `DrawFromBottom` static is
            // honored on the connive draw too.
            let cards_to_draw =
                super::draw::select_cards_to_draw(state, player_id, draw_count as usize);

            if draw_count > 0 && cards_to_draw.len() < draw_count as usize {
                if let Some(p) = state.players.iter_mut().find(|p| p.id == player_id) {
                    p.drew_from_empty_library = true;
                }
            }

            for obj_id in cards_to_draw {
                zones::move_to_zone(state, obj_id, Zone::Hand, events);
                // CR 121.1 + CR 504.1: Increment counters first; embed the
                // resulting per-step ordinal into the event.
                let (nth_in_turn, nth_in_step) =
                    if let Some(p) = state.players.iter_mut().find(|p| p.id == player_id) {
                        p.cards_drawn_this_turn = p.cards_drawn_this_turn.saturating_add(1);
                        p.cards_drawn_this_step = p.cards_drawn_this_step.saturating_add(1);
                        (p.cards_drawn_this_turn, p.cards_drawn_this_step)
                    } else {
                        (1, 1)
                    };
                events.push(GameEvent::CardDrawn {
                    player_id,
                    object_id: obj_id,
                    nth_in_turn,
                    nth_in_step,
                });
                super::drawn_this_turn_choice::record_drawn_card(state, player_id, obj_id);
                // CR 702.94a: Connive draws count as draws for miracle tracking.
                super::draw::record_first_draw_and_enqueue_miracle(state, player_id, obj_id);
            }
        },
    );
    match result {
        ReplacementResult::Execute(_) => {}
        ReplacementResult::Prevented => {
            // Draw was prevented — skip the discard step
            // CR 701.50f + CR 701.50b: the EffectResolved carries the CONNIVER's
            // id (LKI if it left the battlefield) so "whenever a creature you
            // control connives" matches the conniving permanent, not the source.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Connive,
                source_id: conniver_id,
            });
            return Ok(());
        }
        ReplacementResult::NeedsChoice(_) => {
            return Ok(());
        }
    }

    // Step 2: Discard `count` cards.
    let hand_cards: Vec<ObjectId> = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .map(|p| p.hand.iter().copied().collect())
        .unwrap_or_default();

    let discard_count = count as usize;

    if hand_cards.is_empty() {
        // No cards to discard — skip
    } else if hand_cards.len() <= discard_count {
        // Auto-discard all cards in hand (no choice needed)
        let Some(nonland_count) =
            discard_all_and_count_nonlands(state, &hand_cards, controller, events)
        else {
            // Replacement choice interrupted the discard loop — waiting_for already set.
            return Ok(());
        };
        add_connive_counters(state, conniver_id, nonland_count, events);
    } else {
        // Player must choose which cards to discard
        state.waiting_for = WaitingFor::ConniveDiscard {
            player: controller,
            conniver_id,
            // CR 701.50b: metadata only (the discard handler ignores this field);
            // the conniving permanent is the natural source reference here.
            source_id: conniver_id,
            cards: hand_cards,
            count: discard_count,
        };
        // Don't emit EffectResolved yet — it will be emitted when the choice is made
        return Ok(());
    }

    // CR 701.50f + CR 701.50b: the EffectResolved carries the CONNIVER's id (LKI
    // if it left the battlefield) so "whenever a creature you control connives"
    // matches the conniving permanent, not the causing source.
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Connive,
        source_id: conniver_id,
    });
    Ok(())
}

/// Discard all given cards and return how many were nonland.
/// Returns `None` if a replacement effect needs a player choice (interrupts the loop).
/// Caller is responsible for setting `state.waiting_for` when `None` is returned.
pub(crate) fn discard_all_and_count_nonlands(
    state: &mut GameState,
    cards: &[ObjectId],
    player: crate::types::player::PlayerId,
    events: &mut Vec<GameEvent>,
) -> Option<u32> {
    let mut nonland_count = 0;
    for &card_id in cards {
        let is_nonland = is_nonland_card(state, card_id);
        if let super::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) =
            super::discard::discard_caused_by_effect_with_source(
                state, card_id, player, None, events,
            )
        {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(choice_player, state);
            return None;
        }
        if is_nonland {
            nonland_count += 1;
        }
    }
    Some(nonland_count)
}

/// Check if a card is nonland (before discarding it, while it's still accessible).
fn is_nonland_card(state: &GameState, object_id: ObjectId) -> bool {
    state.objects.get(&object_id).is_some_and(|obj| {
        !obj.card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Land)
    })
}

/// Add +1/+1 counters to the conniving creature via the replacement pipeline.
/// CR 701.50b: If the creature left the battlefield, skip the counter.
pub(crate) fn add_connive_counters(
    state: &mut GameState,
    conniver_id: ObjectId,
    count: u32,
    events: &mut Vec<GameEvent>,
) {
    if count == 0 {
        return;
    }
    // CR 701.50b: Skip if the conniver has left the battlefield
    let on_battlefield = state
        .objects
        .get(&conniver_id)
        .is_some_and(|o| o.zone == Zone::Battlefield);
    if !on_battlefield {
        return;
    }

    let proposed = ProposedEvent::AddCounter {
        placement: CounterPlacement::Object {
            actor: state
                .objects
                .get(&conniver_id)
                .map(|obj| obj.controller)
                .unwrap_or(crate::types::player::PlayerId(0)),
            object_id: conniver_id,
            counter_type: CounterType::Plus1Plus1,
        },
        count,
        applied: HashSet::new(),
    };
    if let ReplacementResult::Execute(ProposedEvent::AddCounter {
        placement:
            CounterPlacement::Object {
                actor,
                object_id,
                counter_type,
            },
        count: final_count,
        ..
    }) = replacement::replace_event(state, proposed, events)
    {
        super::counters::apply_counter_addition(
            state,
            actor,
            object_id,
            counter_type,
            final_count,
            events,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{QuantityExpr, TargetFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    fn make_connive_ability(source: ObjectId, target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Connive {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        )
    }

    fn add_card_to_library(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        is_land: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            player,
            name.to_string(),
            Zone::Library,
        );
        if is_land {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }
        id
    }

    fn add_card_to_hand(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        is_land: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            player,
            name.to_string(),
            Zone::Hand,
        );
        if is_land {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }
        id
    }

    #[test]
    fn connive_sets_waiting_for_when_multiple_cards_in_hand() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let conniver = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conniver".to_string(),
            Zone::Battlefield,
        );

        // Add a card to library (will be drawn)
        add_card_to_library(&mut state, PlayerId(0), "Drawn", false);
        // Add an existing card in hand (so after draw, hand has 2 cards — choice needed)
        add_card_to_hand(&mut state, PlayerId(0), "Existing", false);

        let ability = make_connive_ability(source, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ConniveDiscard { count: 1, .. }
        ));
    }

    #[test]
    fn connive_auto_discards_single_card_nonland_adds_counter() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let conniver = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conniver".to_string(),
            Zone::Battlefield,
        );

        // Add a nonland card to library — will be drawn, then auto-discarded
        add_card_to_library(&mut state, PlayerId(0), "Spell", false);
        // Empty hand, so after draw there's exactly 1 card → auto-discard

        let ability = make_connive_ability(source, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have added a +1/+1 counter (nonland discarded)
        let obj = state.objects.get(&conniver).unwrap();
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1
        );
    }

    #[test]
    fn connive_auto_discards_land_no_counter() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let conniver = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conniver".to_string(),
            Zone::Battlefield,
        );

        // Add a land card to library — will be drawn, then auto-discarded
        add_card_to_library(&mut state, PlayerId(0), "Forest", true);

        let ability = make_connive_ability(source, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No counter (land discarded)
        let obj = state.objects.get(&conniver).unwrap();
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn connive_empty_hand_after_draw_from_empty_library() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let conniver = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conniver".to_string(),
            Zone::Battlefield,
        );

        // Empty library, empty hand — draw fails, no discard
        let ability = make_connive_ability(source, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should emit EffectResolved without panic
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Connive,
                ..
            }
        )));
    }

    #[test]
    fn connive_skips_counter_if_conniver_left_battlefield() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let conniver = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conniver".to_string(),
            Zone::Graveyard, // Not on battlefield
        );

        add_card_to_library(&mut state, PlayerId(0), "Spell", false);

        let ability = make_connive_ability(source, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No counter — conniver not on battlefield
        let obj = state.objects.get(&conniver).unwrap();
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
        );
    }

    /// Full-card parse: Leader, Super-Genius produces a connive replacement and
    /// the combat-trigger connive line, with NO residual Unimplemented effects.
    #[test]
    fn leader_super_genius_card_parses_with_no_unimplemented() {
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::replacements::ReplacementEvent;

        let parsed = parse_oracle_text(
            "If a creature you control would connive, instead you draw a card, then that creature connives.\nAt the beginning of combat on your turn, target creature you control connives. (Draw a card, then discard a card. If you discarded a nonland card, put a +1/+1 counter on that creature.)",
            "Leader, Super-Genius",
            &[],
            &["Legendary".to_string(), "Creature".to_string()],
            &["Human".to_string(), "Scientist".to_string()],
        );

        assert!(
            parsed
                .abilities
                .iter()
                .all(|a| !matches!(*a.effect, Effect::Unimplemented { .. })),
            "no spell ability should be Unimplemented, got {:#?}",
            parsed.abilities
        );
        assert!(
            parsed.triggers.iter().all(|t| t
                .execute
                .as_deref()
                .map(|e| !matches!(*e.effect, Effect::Unimplemented { .. }))
                .unwrap_or(true)),
            "the combat trigger should not be Unimplemented, got {:#?}",
            parsed.triggers
        );
        assert!(
            parsed
                .replacements
                .iter()
                .any(|r| r.event == ReplacementEvent::Connive),
            "the connive replacement should be present, got {:#?}",
            parsed.replacements
        );
        assert!(
            !parsed.triggers.is_empty(),
            "the combat connive trigger should parse"
        );
    }

    /// CR 614.1a + CR 701.50a (Leader, Super-Genius): the replacement line
    /// parses into a `ReplacementEvent::Connive` whose `valid_card` is "a
    /// creature you control" and whose execute chain is `Draw 1` then `Connive`.
    #[test]
    fn leader_connive_replacement_parses_to_connive_event() {
        use crate::parser::oracle_replacement::parse_replacement_line;
        use crate::types::ability::{ControllerRef, TypedFilter};
        use crate::types::replacements::ReplacementEvent;

        let def = parse_replacement_line(
            "If a creature you control would connive, instead you draw a card, then that creature connives.",
            "Leader, Super-Genius",
        )
        .expect("connive replacement must parse");

        assert_eq!(def.event, ReplacementEvent::Connive);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            )),
            "connive replacement must scope to a creature you control"
        );
        let execute = def.execute.as_deref().expect("execute chain");
        assert!(
            matches!(&*execute.effect, Effect::Draw { .. }),
            "first effect of the execute chain must be a draw, got {:?}",
            execute.effect
        );
        let then_connive = execute.sub_ability.as_deref().expect("then-connive link");
        assert!(
            matches!(&*then_connive.effect, Effect::Connive { .. }),
            "second effect of the execute chain must be connive, got {:?}",
            then_connive.effect
        );
    }

    /// Install the parsed Leader replacement on a battlefield object owned by
    /// `controller`. Returns the Leader object id.
    fn install_leader_replacement(state: &mut GameState, controller: PlayerId) -> ObjectId {
        use crate::parser::oracle_replacement::parse_replacement_line;

        let leader = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Leader, Super-Genius".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&leader)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let def = parse_replacement_line(
            "If a creature you control would connive, instead you draw a card, then that creature connives.",
            "Leader, Super-Genius",
        )
        .expect("connive replacement must parse");
        state
            .objects
            .get_mut(&leader)
            .unwrap()
            .replacement_definitions
            .push(def);
        leader
    }

    /// Make a battlefield creature controlled by `controller`.
    fn make_battlefield_creature(state: &mut GameState, controller: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Conniver".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    /// CR 614.1a + CR 701.50a (Leader, Super-Genius): when a creature the Leader's
    /// controller controls connives, the replacement fires — the controller draws
    /// an EXTRA card first, then the connive (draw + discard, +1/+1 for the
    /// nonland discarded) proceeds. After the extra draw plus the connive's own
    /// draw there are two cards in hand, so the connive correctly pauses on a
    /// `ConniveDiscard` choice (count = 1). Driving that choice through the real
    /// resolution-action pipeline produces the +1/+1 counter and leaves the extra
    /// card in hand.
    ///
    /// Revert probe: reverting the connive event-ification (removing the
    /// `replace_event` propose step in `resolve`, the `ProposedEvent::Connive`
    /// variant, or the connive applier) makes the replacement never fire — only
    /// the connive's own single card is drawn, so the hand never reaches two
    /// cards and the connive AUTO-discards with no `ConniveDiscard` pause. The
    /// `WaitingFor::ConniveDiscard` assertion below then fails.
    #[test]
    fn leader_connive_replacement_draws_extra_then_connives() {
        let mut state = GameState::new_two_player(42);
        let _leader = install_leader_replacement(&mut state, PlayerId(0));
        let conniver = make_battlefield_creature(&mut state, PlayerId(0));

        // Two nonland cards in library: one for the EXTRA replacement draw, one
        // for the connive's own draw.
        let extra = add_card_to_library(&mut state, PlayerId(0), "Extra", false);
        let connive_draw = add_card_to_library(&mut state, PlayerId(0), "ConniveDraw", false);

        // Drive the real connive entry point (as the combat trigger would).
        let ability = make_connive_ability(conniver, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The extra draw (replacement) + the connive's own draw = two cards in
        // hand, so the connive pauses for the controller to choose which card to
        // discard. Without the replacement only one card would be drawn and the
        // connive would auto-discard with no pause.
        let waiting = state.waiting_for.clone();
        match &waiting {
            WaitingFor::ConniveDiscard {
                player,
                conniver_id,
                cards,
                count,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*conniver_id, conniver);
                assert_eq!(*count, 1);
                let hand_cards: HashSet<ObjectId> = cards.iter().copied().collect();
                let expected: HashSet<ObjectId> = [extra, connive_draw].into_iter().collect();
                assert_eq!(
                    hand_cards, expected,
                    "both the extra draw and the connive draw must be in hand at the discard choice"
                );
            }
            other => panic!(
                "expected ConniveDiscard pause (proves the extra replacement draw happened), got {other:?}"
            ),
        }

        // Discard the connive's own drawn card; keep the extra draw.
        crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            waiting,
            crate::types::actions::GameAction::SelectCards {
                cards: vec![connive_draw],
            },
            &mut events,
        )
        .unwrap();

        // CR 701.50a: a nonland was discarded → +1/+1 counter on the conniver.
        assert_eq!(
            state.objects[&conniver]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1,
            "conniver should get a +1/+1 counter for the discarded nonland"
        );
        // The extra replacement draw remains in hand; the connive draw was discarded.
        let hand: Vec<ObjectId> = state.players[0].hand.iter().copied().collect();
        assert_eq!(
            hand,
            vec![extra],
            "only the extra replacement draw should remain in hand"
        );
        assert!(
            state.players[0].graveyard.contains(&connive_draw),
            "the discarded connive draw should be in the graveyard"
        );
        // Exactly one connive completion event (the replaced action), so connive
        // triggers fire once.
        let connive_resolved = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Connive,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            connive_resolved, 1,
            "connive should complete exactly once after the replacement"
        );
    }

    /// CR 701.50a + CR 701.50d + CR 614.5 (Leader, Super-Genius): the replacement
    /// chain's "then that creature connives" is a PLAIN connive (draw 1, discard
    /// 1), independent of the count of the connive event that was replaced. When a
    /// creature connives with N=2 (e.g. a "connive 2" source), the replacement
    /// fires, the controller draws ONE extra card, then the creature connives
    /// PLAIN — drawing exactly ONE card and discarding ONE — NOT N=2.
    ///
    /// Setup: library `[extra, c1, c2]` (front-to-back; drawn front-first). The
    /// replacement draws `extra`; the plain connive draws exactly `c1`, leaving
    /// `c2` in the library. Hand = {extra, c1} (2 cards) → ConniveDiscard count=1.
    ///
    /// Revert probe (Step-1 applier fix in replacement.rs): with the fix, the
    /// chain's Connive link resolves its OWN count (Fixed(1)), so the plain
    /// connive draws 1 → `WaitingFor::ConniveDiscard { count: 1 }`, hand has 2
    /// cards, and `c2` is STILL in the library. Reverting the fix (passing the
    /// replaced event's `count` = 2 to `resolve_connive_effect`) makes the plain
    /// connive draw 2 → `ConniveDiscard { count: 2 }`, hand has 3 cards, and `c2`
    /// is drawn OUT of the library. Observed pre-fix: count=2, c2 absent from
    /// library; post-fix: count=1, c2 present in library.
    #[test]
    fn leader_connive_n2_replacement_runs_plain_connive() {
        let mut state = GameState::new_two_player(42);
        let _leader = install_leader_replacement(&mut state, PlayerId(0));
        let conniver = make_battlefield_creature(&mut state, PlayerId(0));

        // Library front-to-back: [extra, c1, c2]. The replacement draws `extra`;
        // a PLAIN connive then draws exactly `c1`, leaving `c2` in the library.
        let extra = add_card_to_library(&mut state, PlayerId(0), "Extra", false);
        let c1 = add_card_to_library(&mut state, PlayerId(0), "C1", false);
        let c2 = add_card_to_library(&mut state, PlayerId(0), "C2", false);

        // Drive the entry point at N=2 (as a "connive 2" source would). The
        // replacement chain's "that creature connives" must still be PLAIN.
        let ability = ResolvedAbility::new(
            Effect::Connive {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 2 },
            },
            vec![TargetRef::Object(conniver)],
            conniver,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.50a + CR 701.50d: plain connive draws 1 → discard 1 (count=1),
        // NOT 2. Pre-fix (inheriting N=2) this is count=2.
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::ConniveDiscard { count: 1, .. }
            ),
            "plain connive must discard exactly 1 (pre-fix would be count=2), got {:?}",
            state.waiting_for
        );

        // The plain connive drew exactly one card (`c1`), so `c2` is STILL in the
        // library. Pre-fix the connive drew 2, pulling `c2` out of the library.
        assert!(
            state.players[0].library.contains(&c2),
            "plain connive must draw exactly 1, leaving c2 in library (pre-fix draws 2), library={:?}",
            state.players[0].library
        );

        // Drive the discard choice through the real resolution-action pipeline:
        // discard the connive draw (`c1`), keep the extra replacement draw.
        let waiting = state.waiting_for.clone();
        crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            waiting,
            crate::types::actions::GameAction::SelectCards { cards: vec![c1] },
            &mut events,
        )
        .unwrap();

        // CR 701.50a: exactly one nonland discarded → exactly one +1/+1 counter.
        assert_eq!(
            state.objects[&conniver]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1,
            "plain connive discards 1 nonland → exactly 1 counter"
        );
        // The extra replacement draw stays in hand; `c1` was discarded.
        let hand: Vec<ObjectId> = state.players[0].hand.iter().copied().collect();
        assert_eq!(
            hand,
            vec![extra],
            "only the extra replacement draw should remain in hand"
        );
        assert!(
            state.players[0].graveyard.contains(&c1),
            "the discarded connive draw should be in the graveyard"
        );
        // CR 614.5: exactly one connive completion (one opportunity).
        let connive_resolved = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Connive,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            connive_resolved, 1,
            "connive should complete exactly once after the replacement"
        );
    }

    /// CR 616.1f + CR 614.5 (two Leaders, Super-Genius): when a creature connives
    /// and TWO distinct connive replacements ("If a creature you control would
    /// connive, instead you draw a card, then that creature connives") are both
    /// applicable, the replacement process must REPEAT (CR 616.1f) after the first
    /// one applies, so the SECOND replacement also fires. Each replacement draws
    /// one extra card; CR 614.5 still bars EACH replacement from re-applying to
    /// the chain (the `applied` set excludes a fired rid), so the eventual plain
    /// connive runs exactly once.
    ///
    /// Setup: library front-to-back `[extra1, extra2, connive_draw, tail]`.
    /// Replacement A draws `extra1`; the repeat applies replacement B which draws
    /// `extra2`; the surviving plain connive then draws exactly `connive_draw`,
    /// leaving `tail` in the library. Hand = {extra1, extra2, connive_draw}
    /// (3 cards) → ConniveDiscard count = 1.
    ///
    /// Production path: two Draw-execute connive replacements classify
    /// Unconditional, so `replace_event` returns `NeedsChoice` and parks a
    /// `WaitingFor::ReplacementChoice`. The ordering choice is driven through the
    /// real top-level dispatcher (`apply_as_current` → the
    /// `(WaitingFor::ReplacementChoice, ChooseReplacement)` arm). The entire
    /// A → B → plain-connive chain runs synchronously within that single
    /// `ChooseReplacement` resume: after applying A, the nested "that creature
    /// connives" re-enters the pipeline with `applied={A}`, finds B as the lone
    /// remaining (non-material) candidate, applies it inline (NO second ordering
    /// prompt), and B's nested connive (applied={A,B}) finds no candidate and runs
    /// the plain connive, which pauses on `ConniveDiscard`.
    ///
    /// Revert probe (STEP 2 — the nested `Effect::Connive` arm in replacement.rs):
    /// with the fix, A's nested connive RE-ENTERS the pipeline (`propose_connive`)
    /// so B fires too → ReplacementApplied(Connive) count == 2 and BOTH extra1 and
    /// extra2 leave the library. Reverting the fix (nested arm calls
    /// `resolve_connive_effect` directly, bypassing the pipeline) means A's applier
    /// runs the plain connive and returns Prevented BEFORE B is ever evaluated →
    /// ReplacementApplied(Connive) count == 1 and only extra1 leaves the library.
    /// The count == 2 and both-extras assertions then fail. The "tail still in
    /// library" assertion proves the plain connive drew exactly 1 (the prior
    /// SHA 32db02979 count fix is preserved).
    #[test]
    fn two_connive_replacements_both_apply_once() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        // Two distinct Leader replacements on two battlefield objects (distinct
        // ReplacementIds), both controlled by P0.
        let _leader_a = install_leader_replacement(&mut state, PlayerId(0));
        let _leader_b = install_leader_replacement(&mut state, PlayerId(0));
        let conniver = make_battlefield_creature(&mut state, PlayerId(0));

        // Library front-to-back: [extra1, extra2, connive_draw, tail].
        let extra1 = add_card_to_library(&mut state, PlayerId(0), "Extra1", false);
        let extra2 = add_card_to_library(&mut state, PlayerId(0), "Extra2", false);
        let connive_draw = add_card_to_library(&mut state, PlayerId(0), "ConniveDraw", false);
        let tail = add_card_to_library(&mut state, PlayerId(0), "Tail", false);

        // Drive the real connive entry point (as the combat trigger would).
        let ability = make_connive_ability(conniver, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Two Unconditional connive replacements → a CR 616.1 ordering prompt.
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "two connive replacements must park a replacement-ordering choice, got {:?}",
            state.waiting_for
        );

        // Drive the ordering choice through the real top-level action dispatcher.
        // The whole A → B → plain-connive chain runs synchronously within this one
        // resume (exactly ONE ordering prompt precedes the discard pause). The
        // events produced by the resume are returned in the `ActionResult`, not
        // appended to the `resolve`-time `events` vec — capture and fold them in.
        let resume = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("replacement ordering choice");
        events.extend(resume.events.iter().cloned());

        // After the repeat, the surviving plain connive pauses for the discard
        // choice (hand now has 3 cards: extra1, extra2, connive_draw → discard 1).
        match state.waiting_for.clone() {
            WaitingFor::ConniveDiscard {
                player,
                conniver_id,
                count,
                cards,
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(conniver_id, conniver);
                assert_eq!(count, 1, "the plain connive discards exactly 1");
                let hand_cards: HashSet<ObjectId> = cards.iter().copied().collect();
                let expected: HashSet<ObjectId> =
                    [extra1, extra2, connive_draw].into_iter().collect();
                assert_eq!(
                    hand_cards, expected,
                    "both extra replacement draws plus the connive draw must be in hand"
                );
            }
            other => panic!(
                "expected ConniveDiscard pause after both connive replacements applied, got {other:?}"
            ),
        }

        // DISCRIMINATING: both connive replacements applied (CR 616.1f repeat).
        // Pre-fix (STEP 2 reverted) this is 1.
        let connive_replacements_applied = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::ReplacementApplied { event_type, .. } if event_type == "Connive"
                )
            })
            .count();
        assert_eq!(
            connive_replacements_applied, 2,
            "both connive replacements must apply once each (CR 616.1f repeat); pre-fix == 1"
        );

        // DISCRIMINATING: both extra draws left the library (one per replacement).
        // Pre-fix only extra1 (the first replacement) leaves.
        assert!(
            !state.players[0].library.contains(&extra1),
            "extra1 (replacement A's draw) must have left the library"
        );
        assert!(
            !state.players[0].library.contains(&extra2),
            "extra2 (replacement B's draw) must have left the library; pre-fix it stays"
        );
        // Count-fix preserved: the single plain connive drew exactly 1, so `tail`
        // is still in the library.
        assert!(
            state.players[0].library.contains(&tail),
            "the plain connive must draw exactly 1, leaving tail in the library"
        );

        // Drive the discard choice through the real resolution-action pipeline:
        // discard the connive draw, keep both extras.
        let waiting = state.waiting_for.clone();
        crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            waiting,
            crate::types::actions::GameAction::SelectCards {
                cards: vec![connive_draw],
            },
            &mut events,
        )
        .unwrap();

        // CR 614.5: the connive completes exactly once (one opportunity) despite
        // the two replacements.
        let connive_resolved = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Connive,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            connive_resolved, 1,
            "connive must complete exactly once even with two replacements (CR 614.5)"
        );
    }

    /// CR 616.1f + CR 614.5 (THREE Leaders, Super-Genius): with three co-applicable
    /// connive replacements the CR 616.1f repeat must drive ALL THREE — not just
    /// the first. Each replacement draws one extra card; the eventual plain connive
    /// still runs exactly once (CR 614.5 bars each fired rid from re-applying).
    ///
    /// Setup: library front-to-back `[extra_a, extra_b, extra_c, connive_draw,
    /// tail]` (5 nonland). The three replacements draw `extra_a`/`extra_b`/
    /// `extra_c`; the surviving plain connive then draws exactly `connive_draw`,
    /// leaving `tail` in the library. Hand = {extra_a, extra_b, extra_c,
    /// connive_draw} (4 cards) → ConniveDiscard count = 1.
    ///
    /// Production path (the 3+ case the `pending_replacement.is_some()` guard
    /// clause fixes): each connive replacement's `execute` is a `Draw 1` then
    /// `Connive` chain whose first link is `Effect::Draw`, classified
    /// `CandidateMateriality::Unconditional` — so ANY set of two or more candidates
    /// is ordering-material (`replacement_ordering_is_material` short-circuits on
    /// the first Unconditional candidate). `replace_event` therefore parks a
    /// `WaitingFor::ReplacementChoice` (candidate_count == 3) at the top level.
    /// Driving `ChooseReplacement { index: 0 }` applies one replacement; its nested
    /// "that creature connives" re-enters `propose_connive` with the applied rid
    /// excluded (CR 614.5). The first resume's nested re-entry finds {B,C} (len==2,
    /// still material) and parks a FRESH `ReplacementChoice` — which the
    /// `pending_replacement.is_some()` guard clause surfaces instead of clobbering.
    /// The second resume applies B; its nested re-entry finds {C} (the LONE
    /// remaining candidate, non-optional) and auto-applies it inline with no third
    /// prompt; C's nested re-entry finds no candidates and runs the plain connive,
    /// which pauses on `ConniveDiscard`. Exactly TWO ordering prompts precede the
    /// discard pause (3 → choose among 3, 2 → choose among 2, lone last auto-applies).
    ///
    /// Revert probe (remove the `state.pending_replacement.is_some() ||` clause):
    /// the second (freshly-parked nested) `ReplacementChoice` is then matched by the
    /// bare whitelist and FALLS THROUGH the `Prevented` arm, which resets
    /// `waiting_for = Priority` and orphans `state.pending_replacement = Some(..)`.
    /// Only A and B fire (count 1 after the first resume; the second prompt never
    /// surfaces), C never fires, `extra_c`/`connive_draw` stay in the library, and
    /// `pending_replacement` is left `Some`. Observed pre-fix on the FIRST resume:
    /// `ReplacementApplied("Connive")` count == 1, `extra_b`/`extra_c` still in
    /// library, `waiting_for == Priority` (no second ordering prompt). The
    /// `ReplacementApplied == 3` and `pending_replacement.is_none()` assertions then
    /// fail. (N=2 works without the clause because after A applies, B is the LONE
    /// remaining candidate → auto-applied inline with no parked prompt to clobber.)
    #[test]
    fn three_connive_replacements_all_apply_once() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        // Three distinct Leader replacements on three battlefield objects (distinct
        // ReplacementIds), all controlled by P0.
        let _leader_a = install_leader_replacement(&mut state, PlayerId(0));
        let _leader_b = install_leader_replacement(&mut state, PlayerId(0));
        let _leader_c = install_leader_replacement(&mut state, PlayerId(0));
        let conniver = make_battlefield_creature(&mut state, PlayerId(0));

        // Library front-to-back: [extra_a, extra_b, extra_c, connive_draw, tail].
        let extra_a = add_card_to_library(&mut state, PlayerId(0), "ExtraA", false);
        let extra_b = add_card_to_library(&mut state, PlayerId(0), "ExtraB", false);
        let extra_c = add_card_to_library(&mut state, PlayerId(0), "ExtraC", false);
        let connive_draw = add_card_to_library(&mut state, PlayerId(0), "ConniveDraw", false);
        let tail = add_card_to_library(&mut state, PlayerId(0), "Tail", false);

        // Drive the real connive entry point (as the combat trigger would).
        let ability = make_connive_ability(conniver, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Three Unconditional connive replacements → a CR 616.1 ordering prompt
        // over all three.
        match state.waiting_for.clone() {
            WaitingFor::ReplacementChoice {
                candidate_count, ..
            } => assert_eq!(
                candidate_count, 3,
                "the first ordering prompt must offer all three connive replacements"
            ),
            other => panic!(
                "three connive replacements must park a replacement-ordering choice, got {other:?}"
            ),
        }

        // Drive the ordering choices through the real top-level action dispatcher
        // until the surviving plain connive pauses on its discard choice. Each
        // resume applies one replacement and re-parks the next ordering choice for
        // the remaining candidates; the lone last candidate auto-applies inline.
        // Count the prompts we traverse so we can assert the observed shape.
        let mut ordering_prompts = 0;
        loop {
            match state.waiting_for {
                WaitingFor::ReplacementChoice { .. } => {
                    ordering_prompts += 1;
                    let resume =
                        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                            .expect("replacement ordering choice");
                    events.extend(resume.events.iter().cloned());
                }
                WaitingFor::ConniveDiscard { .. } => break,
                ref other => panic!(
                    "expected a ReplacementChoice or ConniveDiscard while driving the \
                     CR 616.1f repeat, got {other:?} after {ordering_prompts} prompts \
                     (pre-fix: the second prompt is clobbered to Priority)"
                ),
            }
        }

        // Observed prompt count: 3 replacements → first prompt picks among 3,
        // second picks among the remaining 2, the lone last auto-applies. Exactly
        // two ordering prompts precede the discard pause. Pre-fix this is 1 (the
        // second prompt is clobbered and the loop hits Priority → panic above).
        assert_eq!(
            ordering_prompts, 2,
            "three replacements yield exactly two ordering prompts (3, then 2; lone last auto-applies)"
        );

        // After the full repeat, the surviving plain connive pauses for the discard
        // choice (hand now has 4 cards: the three extras + connive_draw → discard 1).
        match state.waiting_for.clone() {
            WaitingFor::ConniveDiscard {
                player,
                conniver_id,
                count,
                cards,
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(conniver_id, conniver);
                assert_eq!(count, 1, "the plain connive discards exactly 1");
                let hand_cards: HashSet<ObjectId> = cards.iter().copied().collect();
                let expected: HashSet<ObjectId> =
                    [extra_a, extra_b, extra_c, connive_draw].into_iter().collect();
                assert_eq!(
                    hand_cards, expected,
                    "all three extra replacement draws plus the connive draw must be in hand"
                );
            }
            other => panic!(
                "expected ConniveDiscard pause after all three connive replacements applied, got {other:?}"
            ),
        }

        // DISCRIMINATING: all three connive replacements applied (CR 616.1f repeat).
        // Pre-fix (missing `pending_replacement.is_some()` clause) this is 1 — the
        // second freshly-parked ordering prompt is clobbered and C onward dropped.
        let connive_replacements_applied = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::ReplacementApplied { event_type, .. } if event_type == "Connive"
                )
            })
            .count();
        assert_eq!(
            connive_replacements_applied, 3,
            "all three connive replacements must apply once each (CR 616.1f repeat); pre-fix == 1"
        );

        // DISCRIMINATING: all three extra draws left the library (one per
        // replacement). Pre-fix only extra_a (the first) leaves.
        for (label, card) in [
            ("extra_a", extra_a),
            ("extra_b", extra_b),
            ("extra_c", extra_c),
        ] {
            assert!(
                !state.players[0].library.contains(&card),
                "{label} (a replacement's extra draw) must have left the library; pre-fix only extra_a leaves"
            );
        }
        // Count-fix preserved: the single plain connive drew exactly 1, so `tail`
        // is still in the library.
        assert!(
            state.players[0].library.contains(&tail),
            "the plain connive must draw exactly 1, leaving tail in the library"
        );

        // DISCRIMINATING: no orphaned pending replacement after the full repeat.
        // Pre-fix the second freshly-parked record is clobbered (waiting_for reset
        // to Priority) but never `.take()`-consumed, so it is left `Some`.
        assert!(
            state.pending_replacement.is_none(),
            "no replacement record may be orphaned after the CR 616.1f repeat completes; pre-fix it is left Some"
        );

        // Drive the discard choice through the real resolution-action pipeline:
        // discard the connive draw, keep all three extras.
        let waiting = state.waiting_for.clone();
        crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            waiting,
            crate::types::actions::GameAction::SelectCards {
                cards: vec![connive_draw],
            },
            &mut events,
        )
        .unwrap();

        // CR 614.5: the connive completes exactly once (one opportunity) despite
        // the three replacements.
        let connive_resolved = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Connive,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            connive_resolved, 1,
            "connive must complete exactly once even with three replacements (CR 614.5)"
        );
    }

    /// CR 614.1d (Leader, Super-Genius): the replacement is scoped to "a creature
    /// you control" — when a creature an OPPONENT controls connives, the Leader's
    /// controller does NOT draw the extra card, and the opponent's plain connive
    /// runs unchanged.
    ///
    /// Revert probe: if the `valid_card` controller scope were dropped (or the
    /// affected-object/affected-player wiring on `ProposedEvent::Connive` were
    /// wrong), the replacement would fire for the opponent and the Leader's
    /// controller would gain an extra card — failing the hand-size assertions.
    #[test]
    fn leader_connive_replacement_skips_opponent_creature() {
        let mut state = GameState::new_two_player(42);
        let _leader = install_leader_replacement(&mut state, PlayerId(0));
        // Opponent's conniving creature.
        let opp_conniver = make_battlefield_creature(&mut state, PlayerId(1));

        // Opponent library: a single nonland for the plain connive draw.
        let opp_draw = add_card_to_library(&mut state, PlayerId(1), "OppDraw", false);
        // Leader controller's library: a card that must NOT be drawn.
        let leader_card = add_card_to_library(&mut state, PlayerId(0), "LeaderCard", false);

        // The opponent's creature connives (controller = opponent).
        let ability = ResolvedAbility::new(
            Effect::Connive {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![TargetRef::Object(opp_conniver)],
            opp_conniver,
            PlayerId(1),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The Leader's controller drew nothing — the replacement did not fire.
        assert!(
            state.players[0].hand.is_empty(),
            "Leader's controller must not draw for an opponent's connive, hand={:?}",
            state.players[0].hand
        );
        assert!(
            state.players[0].library.contains(&leader_card),
            "Leader controller's card must stay in library"
        );
        // The opponent's plain connive ran: drew its card (auto-discarded the
        // single card), got a +1/+1 counter for the nonland.
        assert!(
            !state.players[1].library.contains(&opp_draw),
            "opponent's connive should have drawn its card"
        );
        assert_eq!(
            state.objects[&opp_conniver]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1,
            "opponent's plain connive should add a +1/+1 counter"
        );
    }

    /// CR 701.50a + CR 614.5 + CR 616.1f (Leader, Super-Genius): when the
    /// connive replacement's LEADING draw is ITSELF replaced and parks an
    /// interactive `ReplacementChoice` (the controller's own draw is replaced),
    /// the "then that creature connives" link must NOT run early. CR 701.50a's
    /// replacement reads "you draw a card, THEN that creature connives" — the
    /// "then" fixes the printed order, so the connive runs only AFTER the parked
    /// draw choice resolves. The applier defers the connive into the DEDICATED
    /// `state.pending_connive_reentry` slot and returns `Prevented`; the
    /// post-replacement-choice epilogue
    /// (`engine_replacement::handle_replacement_choice`) drains that slot once the
    /// leading draw fully delivers (AFTER the Priority reset), so the resulting
    /// `ConniveDiscard` survives. The dedicated slot is invisible to the shared
    /// zone-delivery tail (`apply_zone_delivery_tail`, `DeliveryTail` owner) that
    /// drains `post_replacement_continuation` mid-draw — that mid-draw drain is
    /// exactly what clobbered the round-1 continuation-slot mechanism.
    ///
    /// Fixture (SINGLE intent for the leading draw): two ONE-SHOT,
    /// NON-COMMUTING count-modifier draw replacements on P0 battlefield objects
    /// (`Times { factor: 2 }` Multiplicative + `Plus { value: 1 }` Additive).
    /// Two co-applicable, order-material candidates on the connive replacement's
    /// `Draw 1` event force a CR 616.1 (`is_optional: false`) ordering
    /// `ReplacementChoice` — the leading draw parks. Count modifiers carry NO
    /// `execute`, so they never touch any continuation slot (a count-modifier
    /// produces no `mandatory_post_effect`). `consume_on_apply` retires both after
    /// the leading draw so they do not re-park the connive's OWN later draw.
    ///
    /// REVERT PROBE — three independent reverts each break a distinct assertion:
    /// (1) Revert STEP C+E2 together (back to the `post_replacement_continuation`
    /// stash + variant drain): the post-resume CRUX FAILS — the DeliveryTail drains
    /// the continuation mid-draw, the connive's `ConniveDiscard` is then clobbered
    /// to `Priority` by the epilogue reset, so `waiting_for` is NOT
    /// `ConniveDiscard`. (2) Revert ONLY E2 (defer still writes the field, no
    /// epilogue drain): post-resume `state.pending_connive_reentry` is stranded
    /// `Some(..)` instead of `None`, and the connive never resumes
    /// (`waiting_for == Priority`). (3) Revert ONLY C (no field stashed, applier
    /// runs `Effect::Connive` synchronously): the pre-resume "0 counters" / "0
    /// Connive events" assertions FAIL because the connive ran early. All three are
    /// discriminating and non-vacuous.
    #[test]
    fn leader_connive_parked_draw_defers_then_resumes() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::QuantityModification;
        use crate::types::ability::ReplacementDefinition;
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingConniveReentry;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let _leader = install_leader_replacement(&mut state, PlayerId(0));
        let conniver = make_battlefield_creature(&mut state, PlayerId(0));

        // Two one-shot, non-commuting count-modifier Draw replacements on P0
        // battlefield objects. Two material candidates on the leading `Draw 1`
        // force a CR 616.1 ordering ReplacementChoice (is_optional: false) so the
        // leading draw PARKS. No execute => no continuation-slot competition.
        let install_count_modifier = |state: &mut GameState, modification: QuantityModification| {
            let host = make_battlefield_creature(state, PlayerId(0));
            let mut repl = ReplacementDefinition::new(ReplacementEvent::Draw);
            repl.quantity_modification = Some(modification);
            // Retire after the leading draw so the connive's own later draw is
            // not re-parked by these helpers.
            repl.consume_on_apply = true;
            state
                .objects
                .get_mut(&host)
                .unwrap()
                .replacement_definitions
                .push(repl);
        };
        install_count_modifier(&mut state, QuantityModification::Times { factor: 2 });
        install_count_modifier(&mut state, QuantityModification::Plus { value: 1 });

        // Plenty of nonland library cards: the modified leading draw pulls several
        // (1 -> 2 or 3 depending on chosen order), and the connive's own draw
        // pulls one more.
        for i in 0..6 {
            add_card_to_library(&mut state, PlayerId(0), &format!("Card{i}"), false);
        }

        // Drive the real connive entry point (as the combat trigger would).
        let ability = make_connive_ability(conniver, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // POST-FIX: the leading DRAW parked its replacement-ordering choice; the
        // connive was DEFERRED into the dedicated slot, not run. (Pre-fix without
        // STEP C: the connive ran early and clobbered this with ConniveDiscard, and
        // the field was never stashed.)
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "leading draw must park a ReplacementChoice (the draw's choice), got {:?}",
            state.waiting_for
        );
        assert!(
            matches!(
                state.pending_connive_reentry,
                Some(PendingConniveReentry { .. })
            ),
            "the deferred connive must be stashed in pending_connive_reentry, got {:?}",
            state.pending_connive_reentry
        );
        // The connive has NOT run yet: no +1/+1 counter, no Connive completion.
        assert_eq!(
            state.objects[&conniver]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0,
            "the connive must not have added a counter before the draw choice resolves"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Connive,
                        ..
                    }
                ))
                .count(),
            0,
            "the connive must not complete before the draw choice resolves"
        );

        // Resolve the draw's ordering choice through the REAL action pipeline.
        // The whole resume (apply both count-modifiers, complete the modified
        // leading draw, then drain the deferred pending_connive_reentry) runs
        // within this one resume; fold its returned events in.
        let resume = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("draw replacement ordering choice");
        events.extend(resume.events.iter().cloned());

        // POST-FIX CRUX: the deferred connive resumed in printed order. The
        // dedicated slot is drained to None, and the surviving plain connive now
        // pauses on its own ConniveDiscard (the modified leading draw plus the
        // connive's own draw leave 2+ cards in hand). This ConniveDiscard surviving
        // the epilogue's Priority reset is exactly what the dedicated-slot drain
        // (run AFTER the reset) buys over the round-1 mid-draw DeliveryTail drain.
        assert!(
            state.pending_connive_reentry.is_none(),
            "pending_connive_reentry must be drained to None after the draw choice resolves, got {:?}",
            state.pending_connive_reentry
        );
        match state.waiting_for.clone() {
            WaitingFor::ConniveDiscard {
                player,
                conniver_id,
                count,
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(conniver_id, conniver);
                assert_eq!(count, 1, "the resumed plain connive discards exactly 1");
            }
            other => {
                panic!("expected ConniveDiscard after the resumed connive draws, got {other:?}")
            }
        }

        // Drive the discard through the REAL resolution-action pipeline: discard a
        // nonland card so the conniver gets its +1/+1.
        let waiting = state.waiting_for.clone();
        let WaitingFor::ConniveDiscard { cards, .. } = &waiting else {
            unreachable!("matched ConniveDiscard above");
        };
        let to_discard = *cards.first().expect("hand has a card to discard");
        crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards {
                cards: vec![to_discard],
            },
            &mut events,
        )
        .unwrap();

        // CR 701.50a: a nonland was discarded -> EXACTLY ONE +1/+1 counter.
        assert_eq!(
            state.objects[&conniver]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1,
            "the resumed connive must add exactly one +1/+1 counter"
        );
        // CR 614.5: the connive completes EXACTLY once.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Connive,
                        ..
                    }
                ))
                .count(),
            1,
            "the connive must complete exactly once after the deferred resume"
        );
    }

    /// CR 701.50a + CR 614.5 + CR 616.1f: the Leader connive replacement's
    /// leading draw is itself replaced by a draw-PREVENT replacement (Living
    /// Conundrum "skip that draw" shape). CR 701.50a's "instead you draw a card,
    /// THEN that creature connives" makes the connive step INDEPENDENT of whether
    /// the inner draw happened — the prevention replaced only the DRAW, not the
    /// connive — so the deferred connive must STILL run when the leading draw
    /// resolves to `ReplacementResult::Prevented`.
    ///
    /// Fixture intent: install the Leader replacement plus TWO co-applicable
    /// Draw-PREVENT replacements on P0 (Living Conundrum "skip that draw" shape).
    /// Two applicable mandatory replacements on the leading `Draw 1` force a
    /// CR 616.1 ordering `ReplacementChoice` so the leading draw PARKS (a lone
    /// Prevent would auto-apply without a choice) and the connive DEFERS into
    /// `pending_connive_reentry`. Choosing one Prevent fully prevents the leading
    /// draw -> `continue_replacement` returns `ReplacementResult::Prevented` ->
    /// the Prevented arm of `handle_replacement_choice`. The chosen Prevent is
    /// consumed (`consume_on_apply`); the OTHER Prevent survives. The drain added
    /// there (Step 3) runs the deferred connive: its OWN draw (a fresh Draw 1
    /// event) is the lone surviving Prevent's only candidate, so it auto-applies
    /// and the connive's draw is prevented too. Per `resolve_connive_effect`'s
    /// Prevented arm (CR 701.50f/b) the connive still emits its `EffectResolved`
    /// and completes. A second Prevent (rather than a count-modifier survivor)
    /// keeps the connive's own draw deterministic — no leftover count-modifier
    /// can non-deterministically scale the connive's draw or re-park it.
    ///
    /// Discriminating observables (the flip): post-fix, the deferred slot is
    /// drained to None, exactly one `EffectResolved { Connive }` is emitted (the
    /// connive ran), and control returns to Priority. Pre-fix (Step 3 reverted),
    /// the Prevented arm never drains the slot: it stays STRANDED `Some(..)`, NO
    /// `EffectResolved { Connive }` is emitted, and waiting_for falls through to
    /// Priority. So the `pending_connive_reentry.is_none()` AND the
    /// `EffectResolved { Connive }` count == 1 assertions BOTH flip on revert.
    /// (Verified non-vacuous by neutering Step 3 and watching this test fail.)
    #[test]
    fn leader_connive_prevented_draw_still_connives() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::QuantityModification;
        use crate::types::ability::ReplacementDefinition;
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingConniveReentry;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let _leader = install_leader_replacement(&mut state, PlayerId(0));
        let conniver = make_battlefield_creature(&mut state, PlayerId(0));

        // Two co-applicable Draw-PREVENT replacements on P0 battlefield objects.
        // Both are mandatory and applicable to the leading `Draw 1`, forcing a
        // CR 616.1 ordering `ReplacementChoice` so the leading draw PARKS (a lone
        // Prevent would auto-apply without a choice). Each carries
        // `consume_on_apply`: the chosen one is retired after preventing the
        // leading draw, leaving the other as the sole applicable replacement for
        // the connive's own later draw (so that draw auto-applies — no second
        // ordering choice — and is cleanly prevented, keeping the test
        // deterministic).
        let install_draw_prevent = |state: &mut GameState| -> ObjectId {
            let host = make_battlefield_creature(state, PlayerId(0));
            let mut repl = ReplacementDefinition::new(ReplacementEvent::Draw);
            repl.quantity_modification = Some(QuantityModification::Prevent);
            repl.consume_on_apply = true;
            state
                .objects
                .get_mut(&host)
                .unwrap()
                .replacement_definitions
                .push(repl);
            host
        };
        install_draw_prevent(&mut state);
        install_draw_prevent(&mut state);

        // Library cards present so a draw COULD happen — proving the prevention,
        // not an empty library, is what stops the draws.
        for i in 0..6 {
            add_card_to_library(&mut state, PlayerId(0), &format!("Card{i}"), false);
        }

        // Drive the real connive entry point (as the combat trigger would).
        let ability = make_connive_ability(conniver, conniver);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // PRECONDITION: the leading draw parked its ordering choice and the connive
        // deferred into the dedicated slot (same park as the Execute-path test).
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "leading draw must park a ReplacementChoice, got {:?}",
            state.waiting_for
        );
        assert!(
            matches!(
                state.pending_connive_reentry,
                Some(PendingConniveReentry { .. })
            ),
            "the deferred connive must be stashed before the draw choice resolves, got {:?}",
            state.pending_connive_reentry
        );

        // Resolve the leading draw's ordering choice (either index is a Prevent).
        // Choosing it fully prevents the leading draw -> ReplacementResult::Prevented
        // -> the Prevented arm of handle_replacement_choice.
        let resume = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("draw replacement ordering choice (Prevent)");
        events.extend(resume.events.iter().cloned());

        // CRUX (the flip): the leading draw was PREVENTED, but the deferred connive
        // STILL RAN. The dedicated slot is drained to None (NOT stranded) and the
        // connive completed (its own draw was prevented by the surviving Prevent,
        // so per CR 701.50f/b it emits EffectResolved and finishes). Both
        // assertions are FALSE pre-fix (Step 3 reverted: the slot stays Some and no
        // EffectResolved { Connive } is emitted).
        assert!(
            state.pending_connive_reentry.is_none(),
            "pending_connive_reentry must be drained to None after the prevented draw, got {:?}",
            state.pending_connive_reentry
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "the connive completed (its own draw prevented), returning to Priority, got {:?}",
            state.waiting_for
        );
        // CR 614.5 + CR 701.50f/b: the connive completes EXACTLY once even though
        // both its leading (Leader-replacement) draw and its own draw were
        // prevented — the connive step itself is independent of the draws.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Connive,
                        ..
                    }
                ))
                .count(),
            1,
            "the connive must complete exactly once even though the leading draw was prevented"
        );
    }

    /// CR 701.50e: a connive whose resolved count is 0 is a complete no-op. Driven
    /// through the top-level `resolve()` entry (the production path the combat
    /// trigger / activated ability use): `resolve` resolves the `QuantityExpr` to
    /// 0, `propose_connive` finds no connive replacement and returns the
    /// `Execute(Connive { count: 0 })` survivor, which funnels into
    /// `resolve_connive_effect(.., 0, ..)`. That must perform no
    /// draw/discard/counter, park no `ConniveDiscard`, and emit no
    /// `EffectResolved{Connive}` — so "whenever a permanent connives" abilities do
    /// not trigger.
    ///
    /// Revert probe (the revert-failing assertion is `!ConniveDiscard`): without
    /// the `count == 0` guard, the draw-of-0 is a no-op, but the discard step then
    /// runs with `discard_count == 0`. The single hand card makes
    /// `hand_cards.len() (1) <= discard_count (0)` false, so the else arm parks
    /// `WaitingFor::ConniveDiscard { count: 0 }` and returns — the `!ConniveDiscard`
    /// assertion flips. (With an empty hand the revert would instead reach the tail
    /// and emit `EffectResolved{Connive}`; the one hand card pins the discard-park
    /// arm, the discriminating no-op the guard prevents.)
    #[test]
    fn connive_count_zero_no_ops() {
        let mut state = GameState::new_two_player(42);
        let conniver = make_battlefield_creature(&mut state, PlayerId(0));

        // A distinguishable card on library top and a card in hand. Neither must
        // move if the connive is a complete no-op.
        let top = add_card_to_library(&mut state, PlayerId(0), "Top", false);
        let hand_card = add_card_to_hand(&mut state, PlayerId(0), "HandCard", false);

        // Drive the entry point with a count that resolves to 0 (e.g. a dynamic
        // "connive X" where X = 0). `resolve` -> `propose_connive(.., 0, ..)` ->
        // `Execute(Connive { count: 0 })` survivor -> `resolve_connive_effect`.
        let ability = ResolvedAbility::new(
            Effect::Connive {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 0 },
            },
            vec![TargetRef::Object(conniver)],
            conniver,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.50e: the connive is a complete no-op.
        // No draw: the library-top card is untouched.
        assert!(
            state.players[0].library.contains(&top),
            "connive 0 must not draw: library top must stay in library"
        );
        // No discard: the hand card is untouched and not in the graveyard.
        assert!(
            state.players[0].hand.contains(&hand_card),
            "connive 0 must not discard: hand card must stay in hand"
        );
        assert!(
            !state.players[0].graveyard.contains(&hand_card),
            "connive 0 must not discard: hand card must not reach the graveyard"
        );
        // No counter.
        assert_eq!(
            state.objects[&conniver]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0,
            "connive 0 must place no +1/+1 counter"
        );
        // No ConniveDiscard pause.
        assert!(
            !matches!(state.waiting_for, WaitingFor::ConniveDiscard { .. }),
            "connive 0 must not park a ConniveDiscard, got {:?}",
            state.waiting_for
        );
        // No connive completion event — "whenever a permanent connives" never fires.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Connive,
                        ..
                    }
                ))
                .count(),
            0,
            "connive 0 must emit no EffectResolved{{Connive}} (no connive event occurs)"
        );
    }
}
