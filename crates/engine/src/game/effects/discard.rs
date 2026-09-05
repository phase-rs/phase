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
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef, LEGACY_INCARNATION};
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

/// Retires a Recruit-owned discard frame after its discard event was prevented.
fn retire_discard_frame(
    state: &mut GameState,
    frame_id: crate::types::identifiers::DiscardFrameId,
) {
    if let Ok(Some(frame)) = state.resolution_stack.take_active_discard() {
        debug_assert_eq!(frame.id, frame_id);
    }
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
    discard_frame: Option<crate::types::identifiers::DiscardFrameId>,
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
        enters_attacking: false,
        enter_with_counters: Vec::new(),
        controller_override: None,
        enter_transformed: false,
        face_down_profile: None,
        chain_referent: crate::types::zones::ChainReferentIntent::Silent,
        enter_as_copy: None,
        discard_frame,
        applied,
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            change_zone::deliver_replaced_zone_change(
                state,
                event,
                source_id,
                None,
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
            if let Some(frame_id) = discard_frame {
                retire_discard_frame(state, frame_id);
            }
            return DiscardOutcome::Complete;
        }
        ReplacementResult::NeedsChoice(player) => {
            // CR 614.1: The replacement-effect pipeline retains `discard_frame` on the paused
            // ZoneChange. Generic replacement resume returns to terminal zone
            // delivery, which appends the exact result and emits bookkeeping.
            return DiscardOutcome::NeedsReplacementChoice(player);
        }
    }
    if discard_frame.is_some() {
        // Provenance-backed delivery already emitted the single discard event
        // and recorded the normal bookkeeping at its terminal point.
        return DiscardOutcome::Complete;
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

/// Hands the terminal result of a Recruit discard to its directly contingent
/// continuation. This is called only after terminal zone delivery: a
/// replacement choice may keep the operation parked until that point.
pub(crate) fn hand_off_recruit_discard_result(
    state: &mut GameState,
    frame_id: crate::types::identifiers::DiscardFrameId,
) -> bool {
    let result = state
        .resolution_stack
        .active_ability_continuation_discard_parent_result(frame_id);
    let Some(continuation) = state
        .resolution_stack
        .active_ability_continuation_with_discard_parent_mut(frame_id)
    else {
        return false;
    };
    if let Some(result) = result {
        continuation
            .pending
            .chain
            .set_direct_discard_result_for_immediate_node(result);
    }
    true
}

/// Park what this seat's discard instruction still owes so the replacement
/// resume can finish it.
///
/// CR 614.1: a replacement effect can pause this instruction while it is being
/// applied. CR 701.9a is what is still owed: each remaining card must still be
/// moved from its owner's hand to their graveyard. This is the SINGLE AUTHORITY
/// for "this batch paused" — both selection modes park through it, so the two
/// cannot drift on what a parked batch means.
///
/// Deliberately private and called ONLY from `resolve`, the effect layer. The
/// cost layer owns its own typed cursor (`PendingCostMoveResume::
/// RandomDiscardUnlessPayment`), because it additionally owes an unless-payment
/// this carrier knows nothing about; sharing one carrier across the two would
/// launder a cost payment into an effect, which is exactly what [`DiscardCause`]
/// exists to make unrepresentable.
#[allow(clippy::too_many_arguments)]
fn park_discard_batch(
    state: &mut GameState,
    player: PlayerId,
    cursor: crate::types::game_state::DiscardBatchCursor,
    source_id: ObjectId,
    effect_kind: EffectKind,
    paused_card: ObjectIncarnationRef,
    discard_frame: Option<crate::types::identifiers::DiscardFrameId>,
    preceding_events: Vec<GameEvent>,
    completion: crate::types::game_state::PendingDiscardBatchCompletion,
) {
    let paused_events = preceding_events.clone();
    state.pending_discard_batch = Some(Box::new(crate::types::game_state::PendingDiscardBatch {
        player,
        cursor,
        completion,
        source_id,
        effect_kind,
        paused_card,
        discard_frame,
        // The `player_scope` driver installs the fan-out remainder, if any,
        // as it unwinds — this layer only knows about one seat.
        fan_out: None,
        preceding_events,
    }));
    crate::game::engine_resolution_choices::defer_observer_triggers_for_paused_choice(
        state,
        &paused_events,
        0,
    );
}

/// CR 400.7: pin the occurrence a replacement pause parked, while the card is
/// still in its pre-move zone.
///
/// A pause is only ever raised for a live hand card, so the lookup cannot
/// legitimately miss. The fallback pins `LEGACY_INCARNATION`, which no live
/// object can carry — the resume match then fails closed instead of letting a
/// bare `ObjectId` settle the pause against whichever occurrence happens to be
/// leaving the hand.
pub(crate) fn pin_paused_occurrence(
    state: &GameState,
    object_id: ObjectId,
) -> ObjectIncarnationRef {
    state
        .objects
        .get(&object_id)
        .map(ObjectIncarnationRef::from_object)
        .unwrap_or_else(|| ObjectIncarnationRef::of(object_id, LEGACY_INCARNATION))
}

/// CR 701.9a: To discard a card, move it from owner's hand to their graveyard.
/// If targets specify specific cards, discard those; otherwise discard from end of hand.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let discard_frame = ability
        .sub_ability
        .as_ref()
        .and_then(|sub| match sub.condition.as_ref() {
            Some(crate::types::ability::AbilityCondition::DiscardedCardMatchesFilter {
                ..
            }) => Some(
                state
                    .resolution_stack
                    .begin_discard(Some(ability.source_id)),
            ),
            _ => None,
        });
    // CR 608.2i: the terminal count window for this instruction starts here.
    // Everything this node emits before a replacement-application pause is
    // carried into the parked batch so the reunited window is exactly what the
    // un-paused path would have published. The `player_scope` driver widens it
    // to the whole clause's span when the pause interrupted a fan-out.
    let events_before_self = events.len();
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

    // CR 400.7 + CR 603.7c + CR 603.7b: a delayed discard whose pinned referent
    // became a new object discards nothing, and the trigger still resolves.
    //
    // EARLY RETURN IS MANDATORY HERE — substitution alone would be a live bug,
    // not a redundancy. Emptying `specific_targets` below falls through the
    // `!specific_targets.is_empty()` gate into the GENERIC hand-choice/random
    // path, which picks some OTHER card out of the player's hand. That is a
    // fallback re-binding the effect to a different object, so the decision rule
    // requires the guard rather than the substitution.
    //
    // Deliberately placed above the `specific_targets` computation so it fires
    // before either gate is evaluated. `EffectKind::from(&ability.effect)`
    // (not a literal) because this resolver serves BOTH `DiscardCard` and
    // `Discard`, which the Tier C census counts as distinct effect types.
    if ability.pinned_object_targets_all_stale(state) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // Check if targets specify specific cards to discard. Parent chain
    // propagation can inherit non-hand object targets (e.g. Traumatic Critique's
    // damage recipient) — those must not short-circuit the hand-choice path.
    //
    // Issue #3257: A bounce head's chosen graveyard creatures are propagated onto
    // a trailing "discard a card" sub-ability for chain context, but they are NOT
    // discard targets. Once the bounce moves them to hand they must not bypass the
    // interactive DiscardChoice path via this fast path — only a *declared* targeted
    // discard (Oracle uses "target") may consume `ability.targets` here.
    // Partially-stale case: the all-stale case already returned above, so this
    // substitution only ever drops individual dead referents from a list that
    // still has at least one live member — it cannot empty the list and so
    // cannot reach the generic-path fallback the guard above protects.
    let live_targets = ability.live_object_targets(state);
    let specific_targets: Vec<_> = live_targets
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
                | TargetFilter::LastZoneChanged
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
        for (index, obj_id) in specific_targets.iter().copied().enumerate() {
            let obj = state
                .objects
                .get(&obj_id)
                .ok_or(EffectError::ObjectNotFound(obj_id))?;
            if obj.zone != Zone::Hand {
                continue;
            }
            let player_id = obj.owner;

            // CR 701.9a + CR 609.3: Tamiyo, Collector of Tales class — an
            // opponent's spell or ability can't force the protected player to
            // discard ANY card, regardless of which one.
            if crate::game::static_abilities::forced_action_muzzled(
                state,
                ability.controller,
                player_id,
                crate::types::ability::CostCategory::Discards,
            ) {
                continue;
            }

            let proposed = ProposedEvent::Discard {
                player_id,
                object_id: obj_id,
                source_id: Some(ability.source_id),
                caused_by_effect: true,
                discard_frame,
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    match event {
                        ProposedEvent::Discard {
                            player_id: pid,
                            object_id: oid,
                            discard_frame,
                            applied,
                            ..
                        } => {
                            if let DiscardOutcome::NeedsReplacementChoice(player) =
                                complete_discard_to_graveyard(
                                    state,
                                    oid,
                                    pid,
                                    Some(ability.source_id),
                                    discard_frame,
                                    applied,
                                    events,
                                )
                            {
                                state.waiting_for =
                                    crate::game::replacement::replacement_choice_waiting_for(
                                        player, state,
                                    );
                                park_discard_batch(
                                    state,
                                    player_id,
                                    crate::types::game_state::DiscardBatchCursor::Ordered {
                                        remaining: specific_targets[index + 1..]
                                            .iter()
                                            .filter_map(|id| state.objects.get(id))
                                            .map(ObjectIncarnationRef::from_object)
                                            .collect(),
                                    },
                                    ability.source_id,
                                    EffectKind::from(&ability.effect),
                                    pin_paused_occurrence(state, obj_id),
                                    discard_frame,
                                    events[events_before_self..].to_vec(),
                                    crate::types::game_state::PendingDiscardBatchCompletion::Standard,
                                );
                                return Ok(());
                            }
                        }
                        zone_event @ ProposedEvent::ZoneChange {
                            object_id: oid,
                            discard_frame,
                            ..
                        } => {
                            // Replacement redirected (e.g., Madness → exile instead of graveyard).
                            // The lowered ZoneChange already re-looped through the
                            // pipeline (CR 616.1f), so `Moved` redirects were consulted.
                            change_zone::deliver_replaced_zone_change(
                                state,
                                zone_event,
                                None,
                                None,
                                None,
                                false,
                                crate::types::game_state::PostReplacementDrainOwner::DeliveryTail,
                                None,
                                events,
                            );
                            if discard_frame.is_none() {
                                crate::game::restrictions::record_discard(state, player_id);
                                events.push(GameEvent::Discarded {
                                    player_id,
                                    object_id: oid,
                                    source_id: Some(ability.source_id),
                                });
                            }
                        }
                        _ => {}
                    }
                }
                ReplacementResult::Prevented => {
                    if let Some(frame_id) = discard_frame {
                        retire_discard_frame(state, frame_id);
                    }
                }
                ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    park_discard_batch(
                        state,
                        player_id,
                        crate::types::game_state::DiscardBatchCursor::Ordered {
                            remaining: specific_targets[index + 1..]
                                .iter()
                                .filter_map(|id| state.objects.get(id))
                                .map(ObjectIncarnationRef::from_object)
                                .collect(),
                        },
                        ability.source_id,
                        EffectKind::from(&ability.effect),
                        pin_paused_occurrence(state, obj_id),
                        discard_frame,
                        events[events_before_self..].to_vec(),
                        crate::types::game_state::PendingDiscardBatchCompletion::Standard,
                    );
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

        // CR 701.9a + CR 609.3: Tamiyo, Collector of Tales class — an
        // opponent's spell or ability can't force the protected player to
        // discard ANY card, regardless of which one. Treated as an impossible
        // action for that player: a mandatory discard is a failed no-op
        // (mirrors the empty-hand mandatory-discard path below), an "up to"
        // discard simply discards zero.
        if crate::game::static_abilities::forced_action_muzzled(
            state,
            ability.controller,
            discard_player,
            crate::types::ability::CostCategory::Discards,
        ) {
            if !up_to {
                state.cost_payment_failed_flag = true;
            }
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        }

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
            // CR 701.9a: this is a resolving effect, so Library-of-Leng-class
            // replacements DO apply — `DiscardCause::Effect`.
            //
            // CR 614.1: a replacement-application choice mid-batch parks the
            // cursor `discard_at_random` returns rather than dropping it;
            // `drain_pending_discard_batch` (effects/mod.rs) finishes the
            // remaining picks and publishes the terminal marker. The COST caller
            // persists the same cursor in its own carrier, because it
            // additionally owes an unless-payment this layer has no business
            // settling.
            if let RandomDiscardOutcome::NeedsReplacementChoice {
                remaining_eligible,
                remaining_count,
                paused_card,
                // `discard_at_random` already set `waiting_for` from this value
                // and this path parks without re-setting it, so there is
                // nothing here to keep in step. The drain that RE-parks does
                // consume it.
                chooser: _,
            } = discard_at_random(
                state,
                RandomDiscardRequest {
                    player: discard_player,
                    source_id: ability.source_id,
                    count,
                    eligible: hand_cards,
                    cause: DiscardCause::Effect,
                    discard_frame,
                },
                events,
            ) {
                park_discard_batch(
                    state,
                    discard_player,
                    crate::types::game_state::DiscardBatchCursor::Random {
                        pool: remaining_eligible,
                        remaining: remaining_count,
                    },
                    ability.source_id,
                    EffectKind::from(&ability.effect),
                    paused_card,
                    discard_frame,
                    events[events_before_self..].to_vec(),
                    crate::types::game_state::PendingDiscardBatchCompletion::Standard,
                );
                return Ok(());
            }
        } else if hand_cards.is_empty() {
            // up_to=true with empty hand — choosing 0 is the only option, skip interaction.
        } else if !up_to && hand_cards.len() <= count {
            // Forced discard — no choice needed, discard all eligible cards.
            // When up_to=true, always present the choice (player may discard fewer).
            for (i, obj_id) in hand_cards.iter().enumerate() {
                if let DiscardOutcome::NeedsReplacementChoice(player) =
                    discard_caused_by_effect_with_source_and_frame(
                        state,
                        *obj_id,
                        discard_player,
                        Some(ability.source_id),
                        discard_frame,
                        events,
                    )
                {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    // CR 614.1 + CR 701.9a: park the un-iterated tail instead of
                    // abandoning it. `hand_cards[i + 1..]` and not `[i..]`: the
                    // paused card is settled by the replacement itself, exactly
                    // as `discard_at_random`'s cursor documents. The terminal
                    // `EffectResolved` below is unreachable from here, so the
                    // drain emits it — see `drain_pending_discard_batch`.
                    park_discard_batch(
                        state,
                        discard_player,
                        crate::types::game_state::DiscardBatchCursor::All {
                            remaining: hand_cards[i + 1..].to_vec(),
                        },
                        ability.source_id,
                        EffectKind::from(&ability.effect),
                        // CR 400.7: the pause parks the pre-move occurrence.
                        pin_paused_occurrence(state, *obj_id),
                        discard_frame,
                        events[events_before_self..].to_vec(),
                        crate::types::game_state::PendingDiscardBatchCompletion::Standard,
                    );
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
                discard_frame,
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
    route_discard(state, object_id, player, source_id, false, None, events)
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
    discard_caused_by_effect_with_source_and_frame(
        state, object_id, player, source_id, None, events,
    )
}

/// Resolving-effect discard with optional operation-owned provenance.
/// CR 701.9a vs CR 118.12 / CR 601.2h: WHY a card is being discarded.
///
/// This is the `caused_by_effect` axis of `route_discard`, surfaced as a type
/// rather than a bool because it is load-bearing and silently mis-set: it gates
/// `ReplacementCondition::EffectCausedDiscard`. Library of Leng replaces a
/// discard caused by a spell or ability, and must NOT touch a discard made to
/// pay a cost — the boundary `library_of_leng_does_not_apply_to_discard_cost`
/// pins.
///
/// Callers must state which one they are; there is deliberately no default. A
/// shared discard helper that hard-codes one of these silently launders a cost
/// payment into an effect (or vice versa), which is exactly the bug this enum
/// exists to make unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscardCause {
    /// A resolving spell or ability discards the card (CR 701.9a).
    /// Library-of-Leng-class replacements gate on this.
    Effect,
    /// The discard IS the payment of a cost (CR 118.12 unless-cost,
    /// CR 601.2h additional cost). Effect-caused replacements must not apply.
    Cost,
}

/// One game-selected discard batch: who, how many, from what pool, and why.
///
/// Bundled rather than passed as positional arguments because the caller-supplied
/// axes are all easy to transpose — `player` vs the source's controller,
/// `count` vs pool length, and especially `cause`, which is silently wrong
/// rather than loudly wrong. Named fields make each call site state its intent.
pub(crate) struct RandomDiscardRequest {
    /// The discarding player.
    pub player: PlayerId,
    /// Discard source, for replacement provenance.
    pub source_id: ObjectId,
    /// How many cards to pick.
    pub count: usize,
    /// The already-filtered, already-length-checked pool to pick from.
    pub eligible: Vec<ObjectId>,
    /// Effect or cost — see [`DiscardCause`].
    pub cause: DiscardCause,
    /// Operation-owned discard frame, when the caller has one.
    pub discard_frame: Option<crate::types::identifiers::DiscardFrameId>,
}

/// Result of a game-selected (random) discard batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RandomDiscardOutcome {
    /// Every requested card was discarded (or replacement-redirected).
    Completed,
    /// A replacement effect needs a player choice before the batch can finish.
    /// `state.waiting_for` has been set; callers MUST return without treating
    /// the batch as complete.
    ///
    /// The payload is the batch cursor: what a caller needs to finish the job
    /// after the choice settles. It is returned rather than stored globally so
    /// each caller can persist it in ITS own typed continuation — the cost
    /// caller owns an unless-payment that must still be settled, which is not
    /// the effect caller's problem.
    NeedsReplacementChoice {
        /// Cards still un-picked. Excludes the card whose replacement paused.
        remaining_eligible: Vec<ObjectId>,
        /// Picks still owed AFTER the paused one resolves.
        remaining_count: usize,
        /// The card whose replacement raised the choice. CR 614.6: the replaced
        /// event never happens and a modified event happens instead, so this
        /// card was still discarded and the effect layer's drain needs its
        /// identity to stamp the terminal `Discarded` the resumed zone-change
        /// arm cannot emit. The cost layer does not consume it.
        ///
        /// CR 400.7: the PRE-move occurrence, captured while the card is still
        /// in hand. The drain settles the pause against this exact occurrence
        /// leaving the hand, so a later same-id occurrence cannot claim it.
        paused_card: ObjectIncarnationRef,
        /// The replacement pipeline's selected chooser. Published by this
        /// authority rather than re-derived at the call site, because it is NOT
        /// always the discarding player — see the commander carve-out in
        /// `replacement_choice_player`, where the choice belongs to a seat other
        /// than the affected one. A re-parking caller that assumed
        /// `request.player` would prompt the wrong seat the moment such a case
        /// reaches a random discard. Mirrors the `chooser` the single-card
        /// `DiscardOutcome::NeedsReplacementChoice` already carries,
        /// so both cursor arms read one contract.
        chooser: PlayerId,
    },
}

/// CR 701.9b: "Some effects … require a random discard." Move `count` cards
/// picked uniformly at random from `eligible` to their owner's graveyard.
///
/// SINGLE AUTHORITY for game-selected discard. Both layers call it:
///
/// * the EFFECT layer — `Effect::Discard { selection: Random }` (Wheel of
///   Torture class), and
/// * the COST layer — an `AbilityCost::Discard { selection: Random }`
///   unless-payment (Balduvian Horde class).
///
/// Keeping one implementation is what stops the two from drifting on the four
/// things that are easy to get subtly wrong independently: which RNG is used,
/// how a replacement effect mid-batch is surfaced, whether a short pool
/// discards partially, and — via `cause` — whether the discard counts as
/// effect-caused.
///
/// RNG: `state.rng` — the seeded, replay-deterministic game RNG. Never
/// `rand::thread_rng()`: a replayed game (and the CR 732.2a loop replay) must
/// reproduce the identical discards, and a thread RNG would desync them.
///
/// `cause` is REQUIRED and has no default. Sharing one helper across an effect
/// and a cost is only safe if provenance travels with the call — hard-coding
/// `Effect` here made Balduvian Horde's *cost* payment trip
/// `ReplacementCondition::EffectCausedDiscard`, so Library of Leng put the paid
/// card on top of the library. See [`DiscardCause`].
///
/// Caller contract: `eligible` must already be filtered and length-checked.
/// This function discards `min(count, eligible.len())` cards — it does NOT
/// enforce CR 118.3's all-or-nothing rule, because the two layers disagree on
/// what a short pool means (an effect discards what it can; a cost is simply
/// unpayable). The cost caller performs that check before calling.
pub(crate) fn discard_at_random(
    state: &mut GameState,
    request: RandomDiscardRequest,
    events: &mut Vec<GameEvent>,
) -> RandomDiscardOutcome {
    let RandomDiscardRequest {
        player,
        source_id,
        count,
        eligible,
        cause,
        discard_frame,
    } = request;
    let mut remaining = eligible;
    for pick in 0..count {
        if remaining.is_empty() {
            break;
        }
        let index = state.rng.random_range(0..remaining.len());
        let obj_id = remaining.swap_remove(index);
        // CR 701.9a + CR 614.1a: route with this call site's OWN provenance.
        // `route_discard` is the shared tail; only the `caused_by_effect` flag
        // differs, and it is exactly what Library-of-Leng-class replacements
        // gate on.
        let outcome = match cause {
            DiscardCause::Effect => discard_caused_by_effect_with_source_and_frame(
                state,
                obj_id,
                player,
                Some(source_id),
                discard_frame,
                events,
            ),
            DiscardCause::Cost => route_discard(
                state,
                obj_id,
                player,
                Some(source_id),
                false,
                discard_frame,
                events,
            ),
        };
        if let DiscardOutcome::NeedsReplacementChoice(chooser) = outcome {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(chooser, state);
            return RandomDiscardOutcome::NeedsReplacementChoice {
                remaining_eligible: remaining,
                // The paused pick is settled by the replacement itself, so the
                // resumed batch owes only the picks after it.
                remaining_count: count - pick - 1,
                // CR 400.7: pinned before the redirect moves it, so the resume
                // settles against this occurrence and not a later same-id one.
                paused_card: pin_paused_occurrence(state, obj_id),
                // Same value this function just set `waiting_for` from, so a
                // re-parking caller cannot drift from the prompt actually shown.
                chooser,
            };
        }
    }
    RandomDiscardOutcome::Completed
}

pub(crate) fn discard_caused_by_effect_with_source_and_frame(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    source_id: Option<ObjectId>,
    discard_frame: Option<crate::types::identifiers::DiscardFrameId>,
    events: &mut Vec<GameEvent>,
) -> DiscardOutcome {
    route_discard(
        state,
        object_id,
        player,
        source_id,
        true,
        discard_frame,
        events,
    )
}

fn route_discard(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    source_id: Option<ObjectId>,
    caused_by_effect: bool,
    discard_frame: Option<crate::types::identifiers::DiscardFrameId>,
    events: &mut Vec<GameEvent>,
) -> DiscardOutcome {
    // CR 701.9a: "To discard a card, move it from its owner's hand to that
    // player's graveyard." A card that is not in a hand cannot be discarded, so
    // there is no event to propose.
    //
    // Placed here because every *proposed* discard routes through this function
    // — effect and cost layers, whole-hand and random cursors — so one guard
    // covers them all. (Not the same as every discard: three callers reach
    // `complete_discard_to_graveyard` directly, at `:397` here and in
    // `engine_replacement.rs` / `engine_payment_choices.rs`. Those are RESUMES of
    // an event this function already proposed and guarded, which is why they are
    // not a hole — but the claim is "every proposal", not "every discard".)
    //
    // It became load-bearing with the parked batch: a cursor is a hand snapshot
    // latched BEFORE an action boundary and drained after one, so anything that
    // moved a listed card in between would otherwise be "discarded" out of
    // whatever zone it now occupies — `complete_discard_to_graveyard` lowers to
    // a hard-coded `from: Hand`. Un-paused callers build and consume their
    // snapshot inside one action and cannot observe a difference.
    //
    // Modelled on this file's `Prevented` arms, which are its existing answer to
    // "the card never left the hand, so no discard occurred": both retire the
    // frame and report `Complete`. Retiring matters — a
    // `DiscardedCardMatchesFilter` frame left active would leak when every
    // listed card has already moved.
    //
    // WHICH arms, stated because an earlier revision of this comment named the
    // wrong one: the two that retire are in `complete_discard_to_graveyard` and
    // in `resolve`'s specific-target loop, both ABOVE. This function's own
    // `Prevented` arm below does NOT retire — an inherited asymmetry left
    // untouched, since whether that arm is reachable at all with a frame present
    // was not measured here, and writing a fix for an unmeasured path is how the
    // wrong-arm claim got in.
    //
    // `Complete` is a known imprecision INHERITED from those arms, not introduced
    // here: `DiscardOutcome` has no "nothing happened" variant, so a cost caller
    // reads `Complete` as paid. A prevented discard already launders an unpayable
    // cost the same way (CR 118.3 wants all-or-nothing). Fixing it means a third
    // variant threaded through every caller, which is a change this PR has no
    // mandate for and no test for; the shape is recorded here rather than in a
    // commit message so the next person to touch `DiscardOutcome` finds it.
    if state.objects.get(&object_id).map(|obj| obj.zone) != Some(Zone::Hand) {
        if let Some(frame_id) = discard_frame {
            retire_discard_frame(state, frame_id);
        }
        return DiscardOutcome::Complete;
    }
    let proposed = ProposedEvent::Discard {
        player_id: player,
        object_id,
        source_id,
        caused_by_effect,
        discard_frame,
        applied: HashSet::new(),
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => match event {
            ProposedEvent::Discard {
                player_id: pid,
                object_id: oid,
                discard_frame,
                applied,
                ..
            } => {
                if let DiscardOutcome::NeedsReplacementChoice(choice_player) =
                    complete_discard_to_graveyard(
                        state,
                        oid,
                        pid,
                        source_id,
                        discard_frame,
                        applied,
                        events,
                    )
                {
                    return DiscardOutcome::NeedsReplacementChoice(choice_player);
                }
            }
            zone_event @ ProposedEvent::ZoneChange {
                object_id: oid,
                discard_frame,
                ..
            } => {
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
                    None,
                    false,
                    crate::types::game_state::PostReplacementDrainOwner::DeliveryTail,
                    None,
                    events,
                );
                if discard_frame.is_none() {
                    crate::game::restrictions::record_discard(state, player);
                    events.push(GameEvent::Discarded {
                        player_id: player,
                        object_id: oid,
                        source_id,
                    });
                }
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
mod random_discard_authority_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// Stage `n` cards in P0's hand on a game seeded with `seed`.
    fn hand_of(seed: u64, n: usize) -> (GameState, Vec<ObjectId>) {
        let mut state = GameState::new_two_player(seed);
        let hand = (0..n)
            .map(|i| {
                create_object(
                    &mut state,
                    CardId(10 + i as u64),
                    PlayerId(0),
                    format!("Hand {i}"),
                    Zone::Hand,
                )
            })
            .collect();
        (state, hand)
    }

    fn discarded(state: &GameState, hand: &[ObjectId]) -> Vec<ObjectId> {
        hand.iter()
            .copied()
            .filter(|id| state.objects[id].zone == Zone::Graveyard)
            .collect()
    }

    /// A plain EFFECT-caused request. Tests that care about the provenance axis
    /// build their own request so the `cause` they exercise is visible at the
    /// call site rather than hidden in this default.
    fn request(count: usize, eligible: Vec<ObjectId>) -> RandomDiscardRequest {
        RandomDiscardRequest {
            player: PlayerId(0),
            source_id: ObjectId(500),
            count,
            eligible,
            cause: DiscardCause::Effect,
            discard_frame: None,
        }
    }

    /// CR 701.9b: the authority moves exactly `count` cards from the eligible
    /// pool to the graveyard.
    #[test]
    fn discard_at_random_moves_exactly_count_cards() {
        let (mut state, hand) = hand_of(42, 5);
        let mut events = Vec::new();
        let outcome = discard_at_random(&mut state, request(2, hand.clone()), &mut events);
        assert_eq!(outcome, RandomDiscardOutcome::Completed);
        assert_eq!(discarded(&state, &hand).len(), 2);
        assert_eq!(state.players[0].hand.len(), 3, "the rest stay in hand");
    }

    /// The RNG must be the seeded, replay-deterministic `state.rng` — NOT a
    /// thread RNG. Two games with the same seed must discard the same cards,
    /// or a replayed game (and the CR 732.2a accept-time loop replay) desyncs.
    /// A `thread_rng` implementation passes the count test above but fails this.
    #[test]
    fn discard_at_random_is_seed_deterministic() {
        let pick = |seed: u64| {
            let (mut state, hand) = hand_of(seed, 6);
            let mut events = Vec::new();
            discard_at_random(&mut state, request(3, hand.clone()), &mut events);
            discarded(&state, &hand)
        };
        assert_eq!(
            pick(7),
            pick(7),
            "same seed must reproduce the same random discards"
        );
    }

    /// Reach-guard for the determinism test: the selection genuinely varies
    /// with the seed, so `pick(7) == pick(7)` above is not passing merely
    /// because the function always takes the same positions.
    #[test]
    fn discard_at_random_varies_across_seeds() {
        let pick = |seed: u64| {
            let (mut state, hand) = hand_of(seed, 8);
            let mut events = Vec::new();
            discard_at_random(&mut state, request(3, hand.clone()), &mut events);
            // Compare by hand POSITION, not ObjectId: ids are assigned in the
            // same order every game, so positions are the comparable signal.
            discarded(&state, &hand)
                .iter()
                .map(|id| hand.iter().position(|h| h == id).unwrap())
                .collect::<Vec<_>>()
        };
        let seeds: Vec<Vec<usize>> = (0u64..12).map(pick).collect();
        assert!(
            seeds.windows(2).any(|w| w[0] != w[1]),
            "picks must depend on the seed, got identical selections: {seeds:?}"
        );
    }

    /// CR 701.9a + CR 118.12: `DiscardCause` must actually reach
    /// `route_discard`'s `caused_by_effect` flag, because that is what
    /// `ReplacementCondition::EffectCausedDiscard` gates on.
    ///
    /// Library of Leng replaces an EFFECT-caused discard (card goes to the top
    /// of the library instead of the graveyard) and must not touch a COST
    /// payment. The shared random helper originally hard-coded the effect
    /// route, so paying Balduvian Horde's cost wrongly offered the replacement.
    /// This is the random-selection twin of
    /// `library_of_leng_does_not_apply_to_discard_cost`.
    ///
    /// Both arms run in ONE test so the pair cannot drift: the Cost arm alone
    /// would still pass if `DiscardCause` were ignored and everything routed as
    /// a cost.
    #[test]
    fn discard_at_random_honors_cost_vs_effect_provenance() {
        let setup = || {
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
                .push(super::tests::library_of_leng_discard_replacement());
            (state, card)
        };

        // COST: no effect-caused replacement may fire — the card hits the
        // graveyard and nothing pauses for a choice.
        let (mut state, card) = setup();
        let mut events = Vec::new();
        let outcome = discard_at_random(
            &mut state,
            RandomDiscardRequest {
                player: PlayerId(0),
                source_id: ObjectId(500),
                count: 1,
                eligible: vec![card],
                cause: DiscardCause::Cost,
                discard_frame: None,
            },
            &mut events,
        );
        assert_eq!(
            outcome,
            RandomDiscardOutcome::Completed,
            "a cost payment must not stop for an effect-caused replacement"
        );
        assert!(
            state.players[0].graveyard.contains(&card),
            "cost discard goes to the graveyard, not the top of the library"
        );

        // EFFECT: the same replacement IS offered, proving the flag is read and
        // the Cost arm above is not passing vacuously.
        let (mut state, card) = setup();
        let mut events = Vec::new();
        let outcome = discard_at_random(
            &mut state,
            RandomDiscardRequest {
                player: PlayerId(0),
                source_id: ObjectId(500),
                count: 1,
                eligible: vec![card],
                cause: DiscardCause::Effect,
                discard_frame: None,
            },
            &mut events,
        );
        assert!(
            matches!(outcome, RandomDiscardOutcome::NeedsReplacementChoice { .. }),
            "an effect-caused random discard must offer Library of Leng, got {outcome:?}"
        );
    }

    /// The batch cursor returned on a pause must describe the work still owed,
    /// so the cost caller's persisted continuation can finish it. The paused
    /// pick is settled by the replacement itself and must NOT be re-counted.
    #[test]
    fn discard_at_random_pause_reports_the_remaining_batch() {
        let mut state = GameState::new_two_player(42);
        let leng = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Library of Leng".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&leng)
            .unwrap()
            .replacement_definitions
            .push(super::tests::library_of_leng_discard_replacement());
        let hand: Vec<ObjectId> = (0..4)
            .map(|i| {
                create_object(
                    &mut state,
                    CardId(10 + i as u64),
                    PlayerId(0),
                    format!("Hand {i}"),
                    Zone::Hand,
                )
            })
            .collect();

        let mut events = Vec::new();
        let outcome = discard_at_random(
            &mut state,
            RandomDiscardRequest {
                player: PlayerId(0),
                source_id: ObjectId(500),
                count: 3,
                eligible: hand.clone(),
                cause: DiscardCause::Effect,
                discard_frame: None,
            },
            &mut events,
        );
        let RandomDiscardOutcome::NeedsReplacementChoice {
            remaining_eligible,
            remaining_count,
            paused_card,
            chooser,
        } = outcome
        else {
            panic!("expected a replacement pause, got {outcome:?}");
        };
        assert_eq!(
            remaining_count, 2,
            "3 requested, the 1st paused and is settled by the replacement, so 2 remain"
        );
        assert_eq!(
            remaining_eligible.len(),
            3,
            "the un-picked pool excludes only the paused card"
        );
        // The cursor's two halves must agree on WHICH card paused: the reported
        // paused card is the one missing from the un-picked pool.
        assert!(
            hand.contains(&paused_card.object_id)
                && !remaining_eligible.contains(&paused_card.object_id),
            "the paused card must be a hand card that left the un-picked pool"
        );
        // CR 400.7: the pin is the PRE-move occurrence, so it must still name
        // the live hand card. A pin taken after the redirect would carry the
        // bumped incarnation and never match the departure it is meant to settle.
        assert_eq!(
            Some(paused_card),
            state
                .objects
                .get(&paused_card.object_id)
                .map(ObjectIncarnationRef::from_object),
            "the parked pin must equal the live pre-move occurrence"
        );
        // The published chooser must be the seat this authority actually
        // prompted. A re-parking caller reads `chooser` to rebuild the
        // prompt, so if the two ever disagree the wrong seat is asked. Compared
        // against `waiting_for` rather than against the request's player,
        // because agreeing with the request is the very assumption this pins
        // against — the drain used to re-derive it that way.
        let prompted = match &state.waiting_for {
            crate::types::game_state::WaitingFor::ReplacementChoice { player, .. } => *player,
            other => panic!("expected an installed ReplacementChoice, got {other:?}"),
        };
        assert_eq!(
            chooser, prompted,
            "the outcome's chooser must equal the seat `waiting_for` was built from"
        );
    }

    /// Caller contract (documented on the authority): a pool shorter than
    /// `count` discards what it can and reports `Completed`. Enforcing
    /// CR 118.3's all-or-nothing rule is the COST caller's job, because the
    /// effect layer legitimately discards a short hand.
    #[test]
    fn discard_at_random_short_pool_discards_what_it_can() {
        let (mut state, hand) = hand_of(42, 2);
        let mut events = Vec::new();
        let outcome = discard_at_random(&mut state, request(5, hand.clone()), &mut events);
        assert_eq!(outcome, RandomDiscardOutcome::Completed);
        assert_eq!(discarded(&state, &hand).len(), 2);
    }

    /// CR 701.9a: "To discard a card, move it from its owner's hand to that
    /// player's graveyard." A card that is no longer in a hand when its
    /// proposal is reached cannot be discarded, so `route_discard` must propose
    /// nothing for it.
    ///
    /// The real shape is a parked batch — a cursor latches a hand snapshot
    /// BEFORE an action boundary and drains after one, so a listed card can have
    /// left the hand in between, and `complete_discard_to_graveyard` lowers to a
    /// hard-coded `from: Hand`. Staged directly here rather than through the
    /// batch machinery so a failure names the guard and not the driver.
    ///
    /// NON-VACUITY is the first assertion, not the second: an inert
    /// `route_discard` that discarded nothing at all would satisfy the negative
    /// half. The in-hand card must actually be discarded for the moved card's
    /// silence to mean anything.
    ///
    /// REVERT PROBE (RUN, not reasoned): delete the `!= Some(Zone::Hand)` early
    /// return at the top of `route_discard`. Observed first failure is the
    /// `discarded_ids` assertion, which goes `[stays]` -> `[stays, moved]`.
    ///
    /// The `relowered` assertion below is therefore DOMINATED under that probe —
    /// it never gets to run. It is kept deliberately, and its scope is stated
    /// here rather than left implied: it covers a DIFFERENT failure, one that
    /// lowers the hand -> graveyard `ZoneChange` while suppressing the
    /// `Discarded` push. No probe in this lane exercises that one, and this
    /// fixture passes `discard_frame: None`, so it cannot reach the frame-borne
    /// route where that split is what actually happens today.
    #[test]
    fn route_discard_skips_a_card_that_already_left_the_hand() {
        let (mut state, hand) = hand_of(42, 2);
        let (stays, moved) = (hand[0], hand[1]);
        let mut setup = Vec::new();
        crate::game::zones::move_to_zone(&mut state, moved, Zone::Graveyard, &mut setup);
        assert_eq!(
            state.objects[&moved].zone,
            Zone::Graveyard,
            "reach guard: the card under test must genuinely be out of the hand"
        );

        let mut events = Vec::new();
        for card in [stays, moved] {
            route_discard(&mut state, card, PlayerId(0), None, true, None, &mut events);
        }

        let discarded_ids: Vec<ObjectId> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::Discarded { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            discarded_ids,
            vec![stays],
            "the in-hand card must be discarded (non-vacuity) and the already-moved \
             card must produce no discard"
        );
        let relowered = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::ZoneChanged { object_id, from: Some(Zone::Hand), .. }
                        if *object_id == moved
                )
            })
            .count();
        assert_eq!(
            relowered, 0,
            "no hand -> graveyard move may be lowered for a card that was not in a hand"
        );
    }

    /// The frame half of the same guard: a `DiscardedCardMatchesFilter` frame is
    /// opened by `resolve` for the whole instruction, so bailing out of a listed
    /// card without retiring it leaves an active frame owning nothing — and
    /// `active_discard` is LIFO, so the next operation reads it as its own.
    ///
    /// TWO frames are installed, and what that buys is ARITY AND DIRECTION, not
    /// identity: a single-frame fixture cannot separate "retired one frame" from
    /// "emptied the stack", while nesting catches a retirement that pops zero,
    /// pops two, or pops from the wrong end.
    ///
    /// It does NOT establish that the guard retired the frame it was HANDED, and
    /// an earlier revision of this doc claimed it did. The fixture hands the
    /// guard the frame already on top, so "retire the handed frame" and "retire
    /// the top" are one action here — and they are one action in PRODUCTION too:
    /// `retire_discard_frame` calls `take_active_discard`, which pops the top
    /// WHEN THAT TOP IS A `Discard` FRAME — returning `Err(UnexpectedTop)`
    /// otherwise — with `frame_id` consulted only by a `debug_assert_eq!`.
    /// The id-keyed property is therefore ABSENT FROM THE CODE rather than
    /// merely unmeasured, so a test demanding it would red on HEAD. Recorded
    /// here instead of asserted: a failing test for a property the design does
    /// not claim is noise, not coverage.
    ///
    /// DISCLOSED, NOT REPAIRED, because the qualifier above is load-bearing:
    /// `retire_discard_frame` swallows that `Err` (and the empty case) in an
    /// `if let Ok(Some(..))`, so retirement is BEST-EFFORT. If a non-`Discard`
    /// frame sits on top when this guard fires, the retirement silently no-ops
    /// and the frame survives owning nothing — precisely the hazard the first
    /// paragraph of this doc names. Its reachability was not measured, and
    /// making retirement total is a change to the resolution stack's error
    /// contract rather than to this guard. Same disposition as `route_discard`'s
    /// own non-retiring `Prevented` arm.
    ///
    /// REVERT PROBES (RUN): delete the `retire_discard_frame` call from inside
    /// the guard, keeping the early return — reds at this test's own assertion.
    /// Calling it TWICE also reds, but through `retire_discard_frame`'s
    /// `debug_assert_eq!`, NOT through this test: `[profile.test] inherits =
    /// "dev"`, `[profile.release]` never sets `debug-assertions`, and no
    /// `--release` test invocation exists in the Tiltfile or any workflow — so
    /// the production assertion fires first in every venue this repo runs.
    #[test]
    fn route_discard_retires_the_frame_for_a_card_that_left_the_hand() {
        let (mut state, hand) = hand_of(7, 1);
        let card = hand[0];
        let mut setup = Vec::new();
        crate::game::zones::move_to_zone(&mut state, card, Zone::Graveyard, &mut setup);

        let outer = state.resolution_stack.begin_discard(Some(ObjectId(499)));
        let frame = state.resolution_stack.begin_discard(Some(ObjectId(500)));
        assert_eq!(
            state
                .resolution_stack
                .active_discard()
                .expect("reach guard: a frame must be active before the call")
                .id,
            frame,
            "reach guard: the INNER frame must be the one on top, or the pop below proves nothing"
        );

        let mut events = Vec::new();
        route_discard(
            &mut state,
            card,
            PlayerId(0),
            None,
            true,
            Some(frame),
            &mut events,
        );

        assert_eq!(
            state
                .resolution_stack
                .active_discard()
                .expect("exactly one frame may be retired, leaving the outer one active")
                .id,
            outer,
            "the guard must retire EXACTLY ONE frame, popped from the top: the outer frame survives"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityDefinition, AbilityKind, ControllerRef, DiscardedCardResult,
        EffectOutcomeSignal, LibraryPosition, QuantityExpr, ReplacementCondition,
        ReplacementDefinition, ReplacementMode, ResolvedAbility, SubAbilityLink, TargetFilter,
        TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::counter::CounterType;
    use crate::types::game_state::{PendingContinuation, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::resolution::ChangeZoneFrame;

    #[test]
    fn recruit_handoff_waits_for_direct_continuation_and_clears_descendants() {
        let mut state = GameState::new_two_player(91);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Discarded nonland".to_string(),
            Zone::Hand,
        );
        let result = DiscardedCardResult {
            object_id: card,
            lki: state.objects[&card].snapshot_for_mana_spent(),
            final_zone: Zone::Graveyard,
        };
        let discard_frame = state.resolution_stack.begin_discard(Some(ObjectId(99)));
        state
            .resolution_stack
            .active_discard_mut()
            .expect("new Recruit frame exists")
            .results
            .push(result.clone());

        let grandchild = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(99),
            PlayerId(0),
        );
        let else_branch = grandchild.clone();
        let mut direct_child = grandchild.clone();
        direct_child.sub_ability = Some(Box::new(grandchild));
        direct_child.else_ability = Some(Box::new(else_branch));
        direct_child
            .sub_ability
            .as_mut()
            .expect("test grandchild exists")
            .context
            .direct_discard_result = Some(result.clone());
        direct_child
            .else_ability
            .as_mut()
            .expect("test else branch exists")
            .context
            .direct_discard_result = Some(result.clone());
        let continuation = PendingContinuation::new(Box::new(direct_child), &state);
        state.park_ability_continuation(continuation);

        // Model the nested ZoneChange child owned by a replacement resume:
        // the continuation is deliberately buried until that child settles.
        state.push_change_zone_frame(ChangeZoneFrame {
            pending: None,
            devour_eligible_snapshot: None,
        });
        assert!(
            !hand_off_recruit_discard_result(&mut state, discard_frame),
            "a buried continuation must not consume the operation-owned result"
        );
        state
            .take_active_change_zone_frame()
            .expect("nested child is active")
            .expect("nested child exists");
        assert!(
            hand_off_recruit_discard_result(&mut state, discard_frame),
            "the direct continuation receives the result once it becomes active"
        );
        let direct = state
            .active_ability_continuation()
            .expect("direct continuation remains active after hand-off");
        assert!(direct.chain.context.direct_discard_result.is_some());
        assert!(
            direct
                .chain
                .sub_ability
                .as_ref()
                .is_some_and(|sub| sub.context.direct_discard_result.is_none()),
            "a Recruit result must not leak into a grandchild"
        );
        assert!(
            direct
                .chain
                .else_ability
                .as_ref()
                .is_some_and(|branch| branch.context.direct_discard_result.is_none()),
            "a Recruit result must not leak into an alternate branch"
        );
    }

    pub(super) fn library_of_leng_discard_replacement() -> ReplacementDefinition {
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
            discard_frame: None,
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
