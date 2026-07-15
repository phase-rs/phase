use std::collections::HashSet;

use rand::Rng;

use crate::game::effects::change_zone;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ParentTargetMissingReason, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{AppliedReplacementKey, ProposedEvent};
use crate::types::zones::Zone;

/// Outcome of a discard attempt routed through the replacement pipeline.
pub(crate) enum DiscardOutcome {
    /// Discard completed (normally or via replacement redirect).
    Complete,
    /// A replacement effect requires player choice before discard can proceed.
    /// Callers must handle this by surfacing the replacement choice to the player.
    NeedsReplacementChoice(PlayerId),
}

/// CR 701.9a: To discard a card, move it from its owner's hand to their graveyard.
/// CR 614.6: A `Moved` replacement ("if a card would be put into a graveyard,
/// exile it instead" — Rest in Peace / Leyline of the Void) watches the
/// hand → graveyard zone change, so the discard's inner move must be proposed
/// as a `ZoneChange` and run through the replacement pipeline rather than moved
/// raw. CR 702.187b: Mayhem's cast permission is gated by the graveyard card
/// having been discarded this turn, so stamp that marker only when the card
/// actually reaches the graveyard.
///
/// `applied` carries the `HashSet<AppliedReplacementKey>` from the outer Discard pass so
/// re-proposing the inner `ZoneChange` does not re-run Discard-level definitions
/// (madness) that already applied — this mirrors `discard_applier`'s lowering.
pub(crate) fn complete_discard_to_graveyard(
    state: &mut GameState,
    object_id: ObjectId,
    player_id: PlayerId,
    source_id: Option<ObjectId>,
    applied: HashSet<AppliedReplacementKey>,
    events: &mut Vec<GameEvent>,
) -> DiscardOutcome {
    // CR 614.6 + CR 701.9a: lower the accepted discard to an inner hand →
    // graveyard `ZoneChange` carrying the outer pass's `applied` set, then run
    // it through the pipeline so `Moved` redirects (Rest in Peace class) get
    // their consult. A plain discard previously moved raw and never saw them.
    let proposed = ProposedEvent::ZoneChange {
        object_id,
        from: Zone::Hand,
        to: Zone::Graveyard,
        cause: source_id,
        attach_to: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_with_counters: Vec::new(),
        controller_override: None,
        enter_transformed: false,
        face_down_profile: None,
        applied,
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            change_zone::deliver_replaced_zone_change(
                state,
                event,
                source_id,
                None,
                false,
                crate::types::game_state::PostReplacementDrainOwner::DeliveryTail,
                // Discard delivers to the graveyard — no library placement.
                None,
                events,
            );
        }
        ReplacementResult::Prevented => {
            // CR 614.6: a prevented event never happens — the card never left
            // the hand, so per CR 701.9a (to discard = move hand → graveyard)
            // NO discard occurred. Skip record_discard / the Mayhem stamp / the
            // `Discarded` event. This is distinct from a REDIRECTED discard
            // (CR 701.9c: a card put elsewhere instead is still discarded —
            // the Execute arm above and the madness path both record + emit).
            return DiscardOutcome::Complete;
        }
        ReplacementResult::NeedsChoice(player) => {
            // KNOWN GAP (documented, counter.rs resolve_all style): a CR 616.1
            // ordering choice on the inner ZoneChange (TWO materially-different
            // Moved redirects simultaneously applicable to one discard — e.g.
            // Rest in Peace + Wheel of Sun and Moon) parks here BEFORE the
            // discard bookkeeping. The paused card delivers via the generic
            // replacement-choice resume (`handle_replacement_choice`'s
            // ZoneChange arm), which has no discard context — so for that card
            // `record_discard` / the Mayhem stamp / the `Discarded` event
            // ("whenever you discard" triggers, Megrim class) are skipped, and
            // a multi-card discard's remaining cards are abandoned by the
            // caller's early return (same systemic shape as the forced-discard
            // EffectResolved limitation noted in `resolve`). A resume-side
            // discard-bookkeeping continuation needs a new serialized state
            // slot + resume wiring; not built for a two-material-redirect board
            // no parsed deck assembles. Single-redirect boards (RIP alone)
            // never prompt and are fully correct.
            return DiscardOutcome::NeedsReplacementChoice(player);
        }
    }
    crate::game::restrictions::record_discard(state, player_id);
    // CR 701.9c + CR 702.187b: stamp the Mayhem discard marker only if the card
    // actually landed in the graveyard — a redirect (RIP → exile) leaves it
    // elsewhere, matching the Madness → exile path.
    if state.objects.get(&object_id).map(|o| o.zone) == Some(Zone::Graveyard) {
        crate::game::restrictions::record_card_discarded(state, object_id);
    }
    events.push(GameEvent::Discarded {
        player_id,
        object_id,
        source_id,
    });
    DiscardOutcome::Complete
}

/// CR 701.9a: To discard a card, move it from owner's hand to their graveyard.
/// If targets specify specific cards, discard those; otherwise discard from end of hand.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.9b + CR 608.2d: Peel `UpTo` from the count expression to derive
    // the upper-bound expression and the may-pick-fewer flag. Plain
    // `QuantityExpr` means a mandatory count; wrapped in `UpTo` means the
    // player may discard 0..=count.
    let (num_cards, up_to, unless_filter, target_filter, random) = match &ability.effect {
        Effect::DiscardCard { count, target } => (*count, false, None, target.clone(), false),
        Effect::Discard {
            count,
            unless_filter,
            target,
            selection,
            ..
        } => {
            let (inner, up_to) = count.peel_up_to();
            (
                // CR 107.1b: a calculation that would yield a negative number
                // uses zero instead. Clamp before the `as u32` cast — a
                // subtractive count ("discard cards equal to A minus B" when
                // B > A) would otherwise wrap to ~4 billion and let the player
                // discard their whole hand. Ability context also resolves X
                // against the caster's chosen value. Mirrors `draw.rs`.
                resolve_quantity_with_targets(state, inner, ability).max(0) as u32,
                up_to,
                unless_filter.clone(),
                target.clone(),
                selection.is_random(),
            )
        }
        _ => (1, false, None, TargetFilter::Any, false),
    };

    // Check if targets specify specific cards to discard. Parent chain
    // propagation can inherit non-hand object targets (e.g. Traumatic Critique's
    // damage recipient) — those must not short-circuit the hand-choice path.
    //
    // Issue #3257: A bounce head's chosen graveyard creatures are propagated onto
    // a trailing "discard a card" sub-ability for chain context, but they are NOT
    // discard targets. Once the bounce moves them to hand they must not bypass the
    // interactive DiscardChoice path via this fast path — only a *declared* targeted
    // discard (Oracle uses "target") may consume `ability.targets` here.
    let specific_targets: Vec<_> = ability
        .targets
        .iter()
        .filter_map(|t| {
            let TargetRef::Object(obj_id) = t else {
                return None;
            };
            let obj = state.objects.get(obj_id)?;
            if obj.zone == Zone::Hand {
                Some(*obj_id)
            } else {
                None
            }
        })
        .collect();

    // CR 115.1d: Only a declared targeted discard (using the word "target") may consume targets chosen at cast time.
    let declared_target_discard =
        crate::game::triggers::extract_target_filter_from_effect(&ability.effect).is_some();
    // CR 608.2c: "That player discards that card" (Dread Fugue) binds the reveal
    // choice via `ParentTarget` — not a cast-time target slot, but the forwarded
    // object id must still be discarded. Controller-scoped "discard a card"
    // (Macabre Waltz) must not consume propagated bounce targets (issue #3257).
    let object_bound_discard = ability.effect.target_filter().is_some_and(|t| {
        matches!(
            t,
            TargetFilter::ParentTarget
                | TargetFilter::ParentTargetSlot { .. }
                | TargetFilter::LastRevealed
                | TargetFilter::SelfRef
                | TargetFilter::TriggeringSource
                | TargetFilter::LastCreated
                | TargetFilter::AttachedTo
                | TargetFilter::CostPaidObject
        )
    });

    // Issue #4950 (Thoughtseize) — corrected root cause: `declared_target_discard`
    // and `object_bound_discard` say nothing about whether the discard's
    // target *filter* resolves to an OBJECT (a specific card) or a PLAYER
    // (whose hand a card gets chosen from separately, at resolution time).
    // Both shapes can leave `specific_targets` empty:
    //   - Thoughtseize: `DiscardCard{target: ParentTarget}` forwards the card
    //     CHOSEN by the preceding reveal-choice. When that choice's eligible
    //     set was empty (no nonland card), CR 608.2c says there is nothing to
    //     choose — this must be a hard no-op.
    //   - Tinybones/Chain of Smog/Skullscorch/Archon: `Discard{target: Player}`
    //     ("target player discards a card[/two/at random]") is a *declared*
    //     target (CR 115.1d, hence `declared_target_discard`), but the target
    //     IS the player, not a card — which specific card(s) get discarded is
    //     decided generically below (interactive choice or at random).
    //   - Sonic Shrieker: "they discard a card" forwards the damaged PLAYER
    //     via `ParentTarget` (hence `object_bound_discard`), not a chosen
    //     card — same as the Player case above, just via a different filter.
    // The ORIGINAL (pre-#4950) code gated on `!specific_targets.is_empty()`,
    // which correctly sent all four shapes above to the generic path — but
    // that ALSO sent Thoughtseize's empty-reveal case there, force-discarding
    // an unrelated card whenever hand size happened to equal the discard
    // count. The one bit those four PLAYER-scoped shapes never carry, and
    // that Thoughtseize's empty-reveal case DOES, is a
    // `parent_target_missing_reason` of `RevealHandChoice` — stamped ONLY by
    // `apply_parent_chain_context` immediately after a `RevealHand`
    // reveal-choice whose eligible set was empty (see
    // `GameState::last_parent_target_missing_reason`). So: enter the specific-
    // targets loop (a no-op when `specific_targets` is empty, which IS the
    // desired Thoughtseize behavior) whenever either the original condition
    // holds, OR the parent reveal-choice explicitly found nothing to choose.
    // Any OTHER empty-`specific_targets`, declared/object-bound discard falls
    // through to the generic hand-choice/random path below, exactly as
    // before #4950's broken fix.
    let parent_reveal_choice_found_nothing =
        ability.parent_target_missing_reason == Some(ParentTargetMissingReason::RevealHandChoice);
    if (!specific_targets.is_empty() && (declared_target_discard || object_bound_discard))
        || (object_bound_discard && parent_reveal_choice_found_nothing)
    {
        // Discard specific targeted cards
        for obj_id in specific_targets {
            let obj = state
                .objects
                .get(&obj_id)
                .ok_or(EffectError::ObjectNotFound(obj_id))?;
            if obj.zone != Zone::Hand {
                continue;
            }
            let player_id = obj.owner;

            let proposed = ProposedEvent::Discard {
                player_id,
                object_id: obj_id,
                source_id: Some(ability.source_id),
                caused_by_effect: true,
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    match event {
                        ProposedEvent::Discard {
                            player_id: pid,
                            object_id: oid,
                            applied,
                            ..
                        } => {
                            if let DiscardOutcome::NeedsReplacementChoice(player) =
                                complete_discard_to_graveyard(
                                    state,
                                    oid,
                                    pid,
                                    Some(ability.source_id),
                                    applied,
                                    events,
                                )
                            {
                                state.waiting_for =
                                    crate::game::replacement::replacement_choice_waiting_for(
                                        player, state,
                                    );
                                return Ok(());
                            }
                        }
                        zone_event @ ProposedEvent::ZoneChange { object_id: oid, .. } => {
                            // Replacement redirected (e.g., Madness → exile instead of graveyard).
                            // The lowered ZoneChange already re-looped through the
                            // pipeline (CR 616.1f), so `Moved` redirects were consulted.
                            change_zone::deliver_replaced_zone_change(
                                state,
                                zone_event,
                                None,
                                None,
                                false,
                                crate::types::game_state::PostReplacementDrainOwner::DeliveryTail,
                                None,
                                events,
                            );
                            // CR 702.35: The card was still discarded — record and emit event
                            // so "whenever you discard" triggers fire.
                            crate::game::restrictions::record_discard(state, player_id);
                            events.push(GameEvent::Discarded {
                                player_id,
                                object_id: oid,
                                source_id: Some(ability.source_id),
                            });
                        }
                        _ => {}
                    }
                }
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return Ok(());
                }
            }
        }
    } else {
        // CR 701.9a + CR 115.1: Mirror Draw/Mill/Scry/Surveil — context-ref target
        // filters (Controller, etc.) must consult state slots, not `ability.targets`,
        // so a Discard sub-ability chained off a Player-targeted parent (e.g.
        // Traumatic Critique: damage to any target → "Draw two cards, then discard
        // a card") does not inherit the parent's chosen player and discard from
        // the wrong hand. `resolve_player_for_context_ref` skips `ability.targets`
        // when the filter is a context-ref and falls back to `ability.controller`.
        let discard_player = super::resolve_player_for_context_ref(state, ability, &target_filter);

        // CR 701.9b: Player chooses which card(s) to discard (not "at random").
        let hand_cards: Vec<ObjectId> = state
            .players
            .iter()
            .find(|p| p.id == discard_player)
            .ok_or(EffectError::PlayerNotFound)?
            .hand
            .iter()
            .copied()
            .collect();

        // CR 701.9b: For "up to N" discards, present the full N to the player.
        // The available cards list naturally constrains actual selection.
        let count = if up_to {
            num_cards as usize
        } else {
            (num_cards as usize).min(hand_cards.len())
        };
        if count == 0 && !up_to {
            // CR 608.2c: Effect resolved as no-op (empty hand) — veto downstream IfYouDo.
            state.cost_payment_failed_flag = true;
        } else if random {
            let mut remaining = hand_cards;
            for _ in 0..count {
                if remaining.is_empty() {
                    break;
                }
                let index = state.rng.random_range(0..remaining.len());
                let obj_id = remaining.swap_remove(index);
                if let DiscardOutcome::NeedsReplacementChoice(player) =
                    discard_caused_by_effect_with_source(
                        state,
                        obj_id,
                        discard_player,
                        Some(ability.source_id),
                        events,
                    )
                {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return Ok(());
                }
            }
        } else if hand_cards.is_empty() {
            // up_to=true with empty hand — choosing 0 is the only option, skip interaction.
        } else if !up_to && hand_cards.len() <= count {
            // Forced discard — no choice needed, discard all eligible cards.
            // When up_to=true, always present the choice (player may discard fewer).
            for obj_id in &hand_cards {
                if let DiscardOutcome::NeedsReplacementChoice(player) =
                    discard_caused_by_effect_with_source(
                        state,
                        *obj_id,
                        discard_player,
                        Some(ability.source_id),
                        events,
                    )
                {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    // Known limitation: EffectResolved is not emitted when replacement
                    // choice interrupts forced-discard (same systemic gap as sacrifice).
                    return Ok(());
                }
            }
        } else if count > 0 || up_to {
            // CR 701.9b: Player chooses — present interactive selection.
            state.waiting_for = crate::types::game_state::WaitingFor::DiscardChoice {
                player: discard_player,
                count,
                cards: hand_cards,
                source_id: ability.source_id,
                effect_kind: EffectKind::from(&ability.effect),
                up_to,
                unless_filter,
            };
            // EffectResolved is emitted by the engine handler after the player chooses.
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 207.2c + CR 118.12a: Discard a card as part of an ability cost (Channel).
/// Routes through the replacement pipeline so Madness (CR 702.35) etc. can intercept.
pub(crate) fn discard_as_cost(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> DiscardOutcome {
    discard_as_cost_with_source(state, object_id, player, None, events)
}

pub(crate) fn discard_as_cost_with_source(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    source_id: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> DiscardOutcome {
    route_discard(state, object_id, player, source_id, false, events)
}

/// CR 701.9a + CR 614.1a: Discard caused by resolving a spell or ability effect
/// (not cost payment or cleanup). Routes through the replacement pipeline with
/// `caused_by_effect: true` so Library-of-Leng-class replacements can gate.
pub(crate) fn discard_caused_by_effect_with_source(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    source_id: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> DiscardOutcome {
    route_discard(state, object_id, player, source_id, true, events)
}

fn route_discard(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    source_id: Option<ObjectId>,
    caused_by_effect: bool,
    events: &mut Vec<GameEvent>,
) -> DiscardOutcome {
    let proposed = ProposedEvent::Discard {
        player_id: player,
        object_id,
        source_id,
        caused_by_effect,
        applied: HashSet::new(),
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => match event {
            ProposedEvent::Discard {
                player_id: pid,
                object_id: oid,
                applied,
                ..
            } => {
                if let DiscardOutcome::NeedsReplacementChoice(choice_player) =
                    complete_discard_to_graveyard(state, oid, pid, source_id, applied, events)
                {
                    return DiscardOutcome::NeedsReplacementChoice(choice_player);
                }
            }
            zone_event @ ProposedEvent::ZoneChange { object_id: oid, .. } => {
                // CR 614.1c: Replacement redirected destination (e.g., Madness → exile).
                // The lowered ZoneChange already re-looped through the pipeline
                // (CR 616.1f), so `Moved` redirects were consulted.
                // CR 702.35: The card was still discarded — record and emit event
                // so "whenever you discard" triggers fire.
                change_zone::deliver_replaced_zone_change(
                    state,
                    zone_event,
                    None,
                    None,
                    false,
                    crate::types::game_state::PostReplacementDrainOwner::DeliveryTail,
                    None,
                    events,
                );
                crate::game::restrictions::record_discard(state, player);
                events.push(GameEvent::Discarded {
                    player_id: player,
                    object_id: oid,
                    source_id,
                });
            }
            _ => {}
        },
        ReplacementResult::Prevented => {
            // CR 614.1a: If the discard is prevented, the cost was not fully paid.
            // This is extremely rare during cost payment. The card stays in hand.
        }
        ReplacementResult::NeedsChoice(choice_player) => {
            return DiscardOutcome::NeedsReplacementChoice(choice_player);
        }
    }
    DiscardOutcome::Complete
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityDefinition, AbilityKind, ControllerRef, EffectOutcomeSignal,
        LibraryPosition, QuantityExpr, ReplacementCondition, ReplacementDefinition,
        ReplacementMode, SubAbilityLink, TargetFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::counter::CounterType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;

    fn library_of_leng_discard_replacement() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Discard)
            .mode(ReplacementMode::Optional { decline: None })
            .condition(ReplacementCondition::EffectCausedDiscard)
            .valid_card(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutAtLibraryPosition {
                    target: TargetFilter::ParentTarget,
                    count: QuantityExpr::Fixed { value: 1 },
                    position: LibraryPosition::Top,
                },
            ))
    }

    fn discard_to_battlefield_with_two_counters_replacement() -> ReplacementDefinition {
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::Discard);
        replacement.valid_card = Some(TargetFilter::SelfRef);
        replacement.condition = Some(ReplacementCondition::EventSourceControlledBy {
            controller: ControllerRef::Opponent,
        });
        replacement.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![(
                    CounterType::Plus1Plus1,
                    QuantityExpr::Fixed { value: 2 },
                )],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        )));
        replacement
    }

    /// Rest in Peace / Leyline of the Void class: "If a card would be put into a
    /// graveyard from anywhere, exile it instead." A `Moved` replacement keyed on
    /// `destination_zone = Graveyard`, hosted on the battlefield permanent (it
    /// watches OTHER cards, so `valid_card` is `None` = any card). Mirrors the
    /// parser output of `parse_graveyard_exile_replacement`.
    fn rest_in_peace_exile_replacement() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::SelfRef,
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
            ))
            .destination_zone(Zone::Graveyard)
    }

    /// D1 discriminating test (CR 614.6): a PLAIN discard's inner hand →
    /// graveyard move must consult `Moved` redirects. With Rest in Peace on the
    /// battlefield, a discarded card is exiled, not put into the graveyard. On
    /// the old path (`complete_discard_to_graveyard` moved raw) the card landed
    /// in the graveyard and this assertion failed.
    #[test]
    fn plain_discard_consults_rest_in_peace_and_exiles() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".into(),
            Zone::Hand,
        );
        // Rest in Peace permanent hosting the graveyard → exile Moved replacement.
        let rip = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Rest in Peace".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&rip)
            .unwrap()
            .replacement_definitions
            .push(rest_in_peace_exile_replacement());

        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 614.6: the discard was redirected to exile, never reaching the graveyard.
        assert!(
            state.exile.contains(&card),
            "RIP must exile the discarded card"
        );
        assert!(!state.players[0].graveyard.contains(&card));
        // CR 701.9c: still counts as a discard — the event fires for "whenever you discard".
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Discarded { object_id, .. } if *object_id == card)));
        // CR 702.187b: the Mayhem marker is NOT stamped when the card was redirected.
        assert_eq!(state.objects[&card].discarded_turn, None);
    }

    /// CR 107.1b: a discard count that resolves negative must clamp to 0, not
    /// wrap through the `as u32` cast to ~4 billion. "Discard up to (cards in
    /// your hand − cards in an opponent's hand)" with the opponent holding more
    /// cards yields a negative count; the player must be offered a discard of 0,
    /// never their whole hand. This mirrors the clamp `draw.rs` already applies
    /// for the analogous Mr. Foxglove subtractive-draw shape. Revert-probe:
    /// without the `.max(0)` the presented count is `u32::MAX - 1`.
    #[test]
    fn discard_negative_count_clamps_to_zero() {
        use crate::types::ability::{
            AggregateFunction, CardSelectionMode, PlayerScope, QuantityRef,
        };

        let mut state = GameState::new_two_player(7);
        // Caster (P0) hand: 1 card. Opponent (P1) hand: 3 cards.
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mine".into(),
            Zone::Hand,
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

        // count = up to (HandSize{You} − HandSize{Opponent}) = 1 − 3 = −2.
        let count = QuantityExpr::up_to(QuantityExpr::Sum {
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
        });

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count,
                target: TargetFilter::Controller,
                selection: CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DiscardChoice { count, player, .. } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(
                    *count,
                    0,
                    "CR 107.1b: a negative discard count must clamp to 0, not wrap to {}",
                    u32::MAX - 1
                );
            }
            other => panic!("expected a DiscardChoice of up-to-0, got {other:?}"),
        }
    }

    /// D1 double-consult guard: the madness class (a Discard-level definition
    /// lowering hand → exile) still works and is not re-applied. The discarded
    /// card ends in exile via the madness redirect, with no Moved present, and
    /// the `applied` set prevents the Discard definition from running twice.
    #[test]
    fn madness_class_discard_still_works_without_double_consult() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Madness Spell".into(),
            Zone::Hand,
        );
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::Discard);
        replacement.valid_card = Some(TargetFilter::SelfRef);
        replacement.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
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
        )));
        state
            .objects
            .get_mut(&card)
            .unwrap()
            .replacement_definitions
            .push(replacement);

        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.exile.contains(&card),
            "madness redirects discard to exile"
        );
        assert!(!state.players[0].graveyard.contains(&card));
        // Discarded exactly once.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    GameEvent::Discarded { object_id, .. } if *object_id == card
                ))
                .count(),
            1,
            "discard recorded exactly once (no double-consult)"
        );
    }

    #[test]
    fn discard_moves_card_from_hand_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].hand.contains(&card));
        assert!(state.players[0].graveyard.contains(&card));
    }

    #[test]
    fn discard_specific_target() {
        use crate::types::ability::{FilterProp, TypedFilter};

        let mut state = GameState::new_two_player(42);
        let c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Keep".to_string(),
            Zone::Hand,
        );
        let c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Discard".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Typed(
                    TypedFilter::default()
                        .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
                ),
            },
            vec![TargetRef::Object(c2)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&c1));
        assert!(!state.players[0].hand.contains(&c2));
    }

    #[test]
    fn discard_replacement_can_exile_card_and_still_emit_discarded() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Madness Spell".to_string(),
            Zone::Hand,
        );
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::Discard);
        replacement.valid_card = Some(TargetFilter::SelfRef);
        replacement.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
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
        )));
        state
            .objects
            .get_mut(&card)
            .unwrap()
            .replacement_definitions
            .push(replacement);

        let mut events = Vec::new();
        let outcome = discard_as_cost(&mut state, card, PlayerId(0), &mut events);

        assert!(matches!(outcome, DiscardOutcome::Complete));
        assert!(state.exile.contains(&card));
        assert!(!state.players[0].graveyard.contains(&card));
        assert_eq!(state.objects[&card].discarded_turn, None);
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::Discarded { object_id, .. } if *object_id == card)
        ));
    }

    #[test]
    fn opponent_source_discard_replacement_enters_with_counters() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dodecapod".to_string(),
            Zone::Hand,
        );
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Discard Spell".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&card)
            .unwrap()
            .replacement_definitions
            .push(discard_to_battlefield_with_two_counters_replacement());

        let mut events = Vec::new();
        let outcome =
            discard_as_cost_with_source(&mut state, card, PlayerId(0), Some(source), &mut events);

        assert!(matches!(outcome, DiscardOutcome::Complete));
        assert!(state.battlefield.contains(&card));
        assert!(!state.players[0].graveyard.contains(&card));
        assert_eq!(
            state.objects[&card]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2)
        );
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::Discarded { object_id, .. } if *object_id == card)
        ));
    }

    #[test]
    fn self_source_discard_replacement_condition_does_not_apply() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dodecapod".to_string(),
            Zone::Hand,
        );
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Self Discard Spell".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&card)
            .unwrap()
            .replacement_definitions
            .push(discard_to_battlefield_with_two_counters_replacement());

        let mut events = Vec::new();
        let outcome =
            discard_as_cost_with_source(&mut state, card, PlayerId(0), Some(source), &mut events);

        assert!(matches!(outcome, DiscardOutcome::Complete));
        assert!(state.players[0].graveyard.contains(&card));
        assert!(!state.battlefield.contains(&card));
        assert!(!state.objects[&card]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
    }

    #[test]
    fn library_of_leng_does_not_apply_to_discard_cost() {
        let mut state = GameState::new_two_player(42);
        let leng = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Library of Leng".to_string(),
            Zone::Battlefield,
        );
        let card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hand Card".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&leng)
            .unwrap()
            .replacement_definitions
            .push(library_of_leng_discard_replacement());

        let mut events = Vec::new();
        let outcome = discard_as_cost(&mut state, card, PlayerId(0), &mut events);

        assert!(matches!(outcome, DiscardOutcome::Complete));
        assert!(state.players[0].graveyard.contains(&card));
        assert!(!state.players[0].library.contains(&card));
    }

    #[test]
    fn library_of_leng_offers_replacement_for_effect_caused_discard() {
        let mut state = GameState::new_two_player(42);
        let leng = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Library of Leng".to_string(),
            Zone::Battlefield,
        );
        let card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hand Card".to_string(),
            Zone::Hand,
        );
        let source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Traumatic Critique".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&leng)
            .unwrap()
            .replacement_definitions
            .push(library_of_leng_discard_replacement());

        let mut events = Vec::new();
        let outcome = discard_caused_by_effect_with_source(
            &mut state,
            card,
            PlayerId(0),
            Some(source),
            &mut events,
        );

        assert!(matches!(
            outcome,
            DiscardOutcome::NeedsReplacementChoice(PlayerId(0))
        ));
        assert!(state.players[0].hand.contains(&card));
        assert!(!state.players[0].graveyard.contains(&card));
    }

    #[test]
    fn discard_emits_discarded_event() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Discarded { object_id, .. } if *object_id == card)));
    }

    #[test]
    fn discard_as_cost_moves_to_graveyard_and_records() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Channel Card".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();

        discard_as_cost(&mut state, card, PlayerId(0), &mut events);

        // Card moved hand → graveyard
        assert!(!state.players[0].hand.contains(&card));
        assert!(state.players[0].graveyard.contains(&card));
        // Discarded event emitted
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Discarded { object_id, .. } if *object_id == card)));
        // Restriction tracking updated
        assert!(state
            .players_who_discarded_card_this_turn
            .contains(&PlayerId(0)));
        assert_eq!(state.objects[&card].discarded_turn, Some(state.turn_number));
        assert_eq!(
            state
                .cards_discarded_this_turn_by_player
                .get(&PlayerId(0))
                .copied(),
            Some(1)
        );
    }

    #[test]
    fn non_targeted_discard_creates_waiting_for() {
        use crate::types::ability::QuantityExpr;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        let c1 = create_object(&mut state, CardId(1), PlayerId(0), "A".into(), Zone::Hand);
        let c2 = create_object(&mut state, CardId(2), PlayerId(0), "B".into(), Zone::Hand);
        let c3 = create_object(&mut state, CardId(3), PlayerId(0), "C".into(), Zone::Hand);

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert!(cards.contains(&c1));
                assert!(cards.contains(&c2));
                assert!(cards.contains(&c3));
            }
            other => panic!("Expected DiscardChoice, got {:?}", other),
        }
    }

    #[test]
    fn non_targeted_discard_auto_when_hand_equals_count() {
        use crate::types::ability::QuantityExpr;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        let c1 = create_object(&mut state, CardId(1), PlayerId(0), "A".into(), Zone::Hand);
        let c2 = create_object(&mut state, CardId(2), PlayerId(0), "B".into(), Zone::Hand);

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should auto-discard without WaitingFor
        assert!(
            !matches!(state.waiting_for, WaitingFor::DiscardChoice { .. }),
            "Should not create DiscardChoice when hand == count"
        );
        assert!(!state.players[0].hand.contains(&c1));
        assert!(!state.players[0].hand.contains(&c2));
    }

    #[test]
    fn non_targeted_discard_noop_when_hand_empty() {
        use crate::types::ability::QuantityExpr;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        // No cards in hand

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::DiscardChoice { .. }),
            "Should not create DiscardChoice when hand is empty"
        );
    }

    #[test]
    fn non_targeted_discard_multiple_creates_waiting_for() {
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        // Create 5 cards in hand
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Hand,
            );
        }
        assert_eq!(state.players[0].hand.len(), 5);

        // Non-targeted discard of 2 → interactive choice
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 2,
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 2);
                assert_eq!(cards.len(), 5);
            }
            other => panic!("Expected DiscardChoice, got {:?}", other),
        }
        // Hand unchanged until player selects
        assert_eq!(state.players[0].hand.len(), 5);
    }

    #[test]
    fn opponent_discard_targets_opponent_hand() {
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        // Give player 1 (opponent) 3 cards
        let _c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opp A".into(),
            Zone::Hand,
        );
        let _c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp B".into(),
            Zone::Hand,
        );
        let _c3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opp C".into(),
            Zone::Hand,
        );
        // Give player 0 (controller) 1 card
        create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Mine".into(),
            Zone::Hand,
        );

        // "Target opponent discards a card" — controller is P0, target is P1
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Opponent (P1) should see the discard choice, not controller (P0)
        match &state.waiting_for {
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                assert_eq!(*player, PlayerId(1), "Opponent should make the choice");
                assert_eq!(*count, 1);
                assert_eq!(
                    cards.len(),
                    3,
                    "Should show opponent's 3 cards, not controller's 1"
                );
            }
            other => panic!("Expected DiscardChoice, got {:?}", other),
        }
    }

    #[test]
    fn opponent_discard_auto_when_one_card() {
        let mut state = GameState::new_two_player(42);
        // Opponent has exactly 1 card — should auto-discard without choice
        let opp_card = create_object(&mut state, CardId(1), PlayerId(1), "Opp".into(), Zone::Hand);
        // Controller has cards too (should not be affected)
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mine".into(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Opponent's card should be discarded
        assert!(!state.players[1].hand.contains(&opp_card));
        assert!(state.players[1].graveyard.contains(&opp_card));
        // Controller's hand unchanged
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn target_player_defaults_to_controller() {
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        assert_eq!(ability.target_player(), PlayerId(0));
    }

    #[test]
    fn target_player_extracts_from_mixed_targets() {
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            vec![
                TargetRef::Object(ObjectId(50)),
                TargetRef::Player(PlayerId(1)),
            ],
            ObjectId(100),
            PlayerId(0),
        );
        assert_eq!(ability.target_player(), PlayerId(1));
    }

    #[test]
    fn discard_as_cost_returns_complete() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();

        let outcome = discard_as_cost(&mut state, card, PlayerId(0), &mut events);

        assert!(matches!(outcome, DiscardOutcome::Complete));
        assert!(!state.players[0].hand.contains(&card));
        assert!(state.players[0].graveyard.contains(&card));
    }

    #[test]
    fn up_to_discard_presents_choice_even_when_hand_small() {
        use crate::types::ability::QuantityExpr;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        // Only 1 card in hand, but "discard up to 2" should still present a choice
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 2 }),
                target: TargetFilter::Any,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.9b: up_to=true must present choice even when hand ≤ count
        match &state.waiting_for {
            WaitingFor::DiscardChoice {
                up_to,
                count,
                cards,
                ..
            } => {
                assert!(*up_to);
                // CR 701.9b: up_to presents uncapped count (2), not min(2, hand=1)
                assert_eq!(*count, 2);
                assert_eq!(cards.len(), 1);
            }
            other => panic!(
                "Expected DiscardChoice with up_to, got {:?}",
                std::mem::discriminant(other)
            ),
        }
    }

    #[test]
    fn up_to_discard_allows_zero_selection() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Card {i}"),
                Zone::Hand,
            );
        }

        // Set up a DiscardChoice with up_to=true
        state.waiting_for = WaitingFor::DiscardChoice {
            player: PlayerId(0),
            count: 2,
            cards: state.players[0].hand.iter().copied().collect::<Vec<_>>(),
            source_id: ObjectId(100),
            effect_kind: crate::types::ability::EffectKind::Discard,
            up_to: true,
            unless_filter: None,
        };

        // Select zero cards — should succeed with up_to=true
        let result = apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] });
        assert!(
            result.is_ok(),
            "Zero selection should succeed for up_to discard"
        );
    }

    #[test]
    fn empty_hand_discard_sets_cost_payment_failed_flag() {
        use crate::types::ability::QuantityExpr;

        let mut state = GameState::new_two_player(42);
        // No cards in hand — discard should set veto flag

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 608.2c: No-op discard vetoes downstream IfYouDo conditions
        assert!(
            state.cost_payment_failed_flag,
            "cost_payment_failed_flag should be set when discard count is 0 (empty hand)"
        );
    }

    #[test]
    fn controller_filter_ignores_inherited_non_hand_object_targets() {
        // CR 115.1 regression — Traumatic Critique: damage target is an
        // inherited Object target, but "discard a card" is a hand choice for
        // the spell's controller.
        use crate::types::ability::QuantityExpr;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        let p0_card_a = create_object(&mut state, CardId(1), PlayerId(0), "A".into(), Zone::Hand);
        let _p0_card_b = create_object(&mut state, CardId(3), PlayerId(0), "B".into(), Zone::Hand);
        let damage_target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Creature".into(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![TargetRef::Object(damage_target)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::DiscardChoice { player: PlayerId(0), count: 1, .. }
            ),
            "must prompt controller to discard from hand, not silently skip inherited battlefield target"
        );
        assert!(
            state.players[0].hand.contains(&p0_card_a),
            "no discard should happen before the player chooses"
        );
    }

    #[test]
    fn controller_filter_does_not_inherit_parent_player_target() {
        // CR 115.1 regression — Traumatic Critique:
        // "Deals X damage to any target. Draw two cards, then discard a card."
        // The sub Discard's `target: Controller` must NOT inherit the parent's
        // Player target (the damage victim) — the controller of the spell discards.
        use crate::types::ability::QuantityExpr;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        // Controller is P0 (the AI). Damage victim is P1 (the user).
        // Give P0 a hand to discard from; give P1 a hand to confirm we don't discard theirs.
        let p0_card = create_object(&mut state, CardId(1), PlayerId(0), "AI".into(), Zone::Hand);
        let _p1_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "User".into(),
            Zone::Hand,
        );

        // Sub-ability inherits parent target (P1) per resolve_ability_chain semantics.
        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![TargetRef::Player(PlayerId(1))], // inherited parent target
            ObjectId(100),
            PlayerId(0), // spell controller = P0
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // P0 (controller) has exactly one card → auto-discard, no choice prompt.
        // The bug would have triggered an interactive choice on P1's hand instead.
        assert!(
            !state.players[0].hand.contains(&p0_card),
            "controller (P0) should have discarded their card"
        );
        assert!(
            state.players[0].graveyard.contains(&p0_card),
            "P0's card should be in graveyard"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::DiscardChoice { player, .. } if player == PlayerId(1)),
            "must not prompt P1 (parent target) for discard — Controller filter must resolve to spell controller"
        );
    }

    #[test]
    fn empty_hand_up_to_discard_does_not_set_failed_flag() {
        use crate::types::ability::QuantityExpr;

        let mut state = GameState::new_two_player(42);
        // No cards in hand, but up_to=true — choosing 0 is valid success

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 2 }),
                target: TargetFilter::Any,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // up_to=true with empty hand is not a failure — it's a valid 0 selection
        assert!(
            !state.cost_payment_failed_flag,
            "cost_payment_failed_flag should NOT be set for up_to discard with empty hand"
        );
    }

    /// CR 608.2c: "Discard a card. If you do, draw a card." — when the discard
    /// goes through interactive WaitingFor::DiscardChoice (hand > count),
    /// optional_effect_performed must be set on the pending continuation so the
    /// IfYouDo sub_ability fires after the player selects a card.
    ///
    /// Regression for issue #2001 (Shadow of the Goblin draw never fires).
    #[test]
    fn if_you_do_draw_fires_after_interactive_discard_choice() {
        let mut state = GameState::new_two_player(42);

        // Give the controller 3 cards in hand so the interactive DiscardChoice path fires.
        let c1 = create_object(&mut state, CardId(1), PlayerId(0), "A".into(), Zone::Hand);
        let c2 = create_object(&mut state, CardId(2), PlayerId(0), "B".into(), Zone::Hand);
        let _c3 = create_object(&mut state, CardId(3), PlayerId(0), "C".into(), Zone::Hand);
        // Put a card in the library so the IfYouDo draw has something to find.
        let library_card = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lib".into(),
            Zone::Library,
        );
        // Build "Discard a card. If you do, draw a card." as a ResolvedAbility chain.
        let mut draw_sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        draw_sub.condition = Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        });
        draw_sub.sub_link = SubAbilityLink::SequentialSibling;

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(draw_sub));

        // Use resolve_ability_chain so the sub_ability is stashed into
        // pending_continuation before the DiscardChoice pause, matching the
        // real engine path.
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Should be waiting for a discard choice (3 cards, choose 1).
        assert!(
            matches!(state.waiting_for, WaitingFor::DiscardChoice { .. }),
            "expected DiscardChoice, got {:?}",
            std::mem::discriminant(&state.waiting_for)
        );

        // Player selects c2 to discard.
        apply_as_current(&mut state, GameAction::SelectCards { cards: vec![c2] })
            .expect("select cards should succeed");

        // c2 discarded, then "If you do, draw a card" must have fired.
        assert!(
            !state.players[0].hand.contains(&c2),
            "c2 should have been discarded"
        );
        assert!(
            state.players[0].hand.contains(&library_card),
            "library_card should have been drawn into hand by the IfYouDo draw"
        );
        // Sanity: c1 is still in hand (we only discarded c2).
        assert!(
            state.players[0].hand.contains(&c1),
            "c1 should still be in hand"
        );
    }

    /// Issue #3257: Macabre Waltz — bounce targets propagated onto a trailing
    /// "discard a card" sub must not auto-discard the just-returned creature.
    #[test]
    fn bounce_then_discard_does_not_auto_discard_propagated_return_targets() {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::resolve_ability_chain;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;

        let mut state = GameState::new_two_player(42);
        let returned = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Returned Bear".into(),
            Zone::Graveyard,
        );
        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other Rat".into(),
            Zone::Hand,
        );

        let def = parse_effect_chain(
            "Return up to two target creature cards from your graveyard to your hand, then discard a card.",
            AbilityKind::Spell,
        );
        let ability = build_resolved_from_def_with_targets(
            &def,
            ObjectId(100),
            PlayerId(0),
            vec![TargetRef::Object(returned)],
        );
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            matches!(state.waiting_for, WaitingFor::DiscardChoice { .. }),
            "non-targeted discard must prompt, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            state.objects.get(&returned).map(|o| o.zone),
            Some(Zone::Hand),
            "returned creature must remain in hand pending the discard choice"
        );
        assert_eq!(
            state.objects.get(&other).map(|o| o.zone),
            Some(Zone::Hand),
            "other hand card must not be discarded automatically"
        );
    }

    /// Issue #4950 (Thoughtseize): "Target player reveals their hand. You
    /// choose a nonland card from it. That player discards that card. You
    /// lose 2 life." When the revealed hand has NO nonland card, CR 608.2c
    /// means there's nothing to choose — `reveal_hand::resolve`'s
    /// empty-eligible path (correctly) never opens a `RevealChoice` and never
    /// rebinds `pending_continuation.chain.targets` to a chosen card. Before
    /// this fix, the chained `DiscardCard{target: ParentTarget}` sub-ability
    /// then found zero `specific_targets` and fell through to the generic
    /// "discard from the whole hand" path, force-discarding the land whenever
    /// the opponent's hand size happened to equal the discard count. It must
    /// now resolve as a no-op instead — only the life loss applies.
    #[test]
    fn reveal_choose_nonland_discard_is_noop_when_hand_has_no_nonland_card() {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::resolve_ability_chain;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let opp_land = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Forest".into(),
            Zone::Hand,
        );
        // `create_object` does not infer type from the name — it must be
        // stamped explicitly, same as `reveal_hand_offset_count_truncates_to_inner_plus_one`
        // does for CoreType::Creature. Without this the Non(Land) filter treats
        // "Forest" as an eligible nonland card and the whole scenario is moot.
        state
            .objects
            .get_mut(&opp_land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let def = parse_effect_chain(
            "Target player reveals their hand. You choose a nonland card from it. \
             That player discards that card. You lose 2 life.",
            AbilityKind::Spell,
        );
        let ability = build_resolved_from_def_with_targets(
            &def,
            ObjectId(100),
            PlayerId(0),
            vec![TargetRef::Player(PlayerId(1))],
        );
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            state.players[1].hand.contains(&opp_land),
            "no nonland card was eligible — the land must NOT be discarded"
        );
        assert!(
            !state.players[1].graveyard.contains(&opp_land),
            "the land must not have moved to the graveyard"
        );
        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::DiscardChoice { .. } | WaitingFor::RevealChoice { .. }
            ),
            "empty-eligible reveal must not leave the game waiting on a stale discard/reveal choice"
        );
        assert_eq!(
            state.players[0].life, 18,
            "the life-loss clause still applies even when the discard whiffs"
        );
    }

    /// Companion case for #4950: with a nonland card present, the reveal
    /// choice fires normally, the chosen nonland is discarded, and the land
    /// is left alone — confirms the fix's surviving `if` branch (looping over
    /// non-empty `specific_targets`) is unchanged.
    #[test]
    fn reveal_choose_nonland_discard_targets_the_chosen_nonland_card() {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let opp_land = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Forest".into(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&opp_land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        let opp_spell = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Spell".into(),
            Zone::Hand,
        );

        let def = parse_effect_chain(
            "Target player reveals their hand. You choose a nonland card from it. \
             That player discards that card. You lose 2 life.",
            AbilityKind::Spell,
        );
        let ability = build_resolved_from_def_with_targets(
            &def,
            ObjectId(100),
            PlayerId(0),
            vec![TargetRef::Player(PlayerId(1))],
        );
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let (chooser, cards) = match state.waiting_for.clone() {
            WaitingFor::RevealChoice { player, cards, .. } => (player, cards),
            other => panic!("expected RevealChoice for the nonland pick, got {other:?}"),
        };
        assert_eq!(chooser, PlayerId(0));
        assert_eq!(cards, vec![opp_spell], "only the nonland card is eligible");

        let waiting = state.waiting_for.clone();
        handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards {
                cards: vec![opp_spell],
            },
            &mut events,
        )
        .expect("choosing the nonland card should succeed");

        assert!(
            !state.players[1].hand.contains(&opp_spell),
            "the chosen nonland card must be discarded"
        );
        assert!(
            state.players[1].graveyard.contains(&opp_spell),
            "the chosen nonland card must land in the graveyard"
        );
        assert!(
            state.players[1].hand.contains(&opp_land),
            "the land must be left alone"
        );
        assert_eq!(state.players[0].life, 18);
    }
}
