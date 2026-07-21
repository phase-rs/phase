//! Finite, fail-closed pre-cast shortcut for a self-targeted Chain-shaped
//! spell plus a fixed Magecraft drain observer.
//!
//! This is deliberately not an extension of the legacy generic loop shortcut:
//! it proves one deterministic reducer route before offering it, carries its
//! private transcript outside the serialized state, and commits only a cloned
//! normal-reducer replay after every responder has answered.

use crate::types::ability::{
    AbilityDefinition, CardSelectionMode, CopyRetargetPermission, Effect, PlayerFilter,
    QuantityExpr, ResolvedAbility, TargetFilter,
};
use crate::types::actions::{GameAction, PrecastCopyShortcutResponse};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, PrecastShortcutBreakpoint, PrecastShortcutOfferRuntime, PrecastShortcutReplayStep,
    WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;

use super::{engine::EngineError, turn_control};

/// Bounded proof length for the finite clone route, keeping shortcut replay
/// nonlethal and below the engine's per-action work limit.
const COPY_COUNT: usize = 3;
const REPLAY_STEP_CAP: usize = 256;

/// Intercept a post-cast-trigger priority window. `outgoing` remains the
/// ordinary priority result; `caster` is the semantic caster whose exact route
/// may be offered even when normal priority belongs to the active player.
pub(super) fn maybe_offer_after_cast_triggers(
    state: &mut GameState,
    caster: PlayerId,
    outgoing: WaitingFor,
) -> WaitingFor {
    if !matches!(outgoing, WaitingFor::Priority { .. }) {
        return outgoing;
    }
    if state.format_config.topology().has_shared_team_turns()
        || state.precast_shortcut_runtime.materializing
        || state.precast_shortcut_runtime.offer.is_some()
    {
        return outgoing;
    }

    let Some(spell_id) = eligible_chain_spell(state, caster) else {
        return outgoing;
    };
    if state.precast_shortcut_runtime.suppressed_cast == Some(spell_id) {
        return outgoing;
    }
    if !has_fixed_magecraft_observer(state, caster) {
        return outgoing;
    }
    if !preflight_route(state, caster, spell_id) {
        return outgoing;
    }

    let responders: Vec<PlayerId> = crate::game::players::apnap_order_from(
        state,
        Some(crate::types::ability::ControllerRef::You),
        caster,
    )
    .into_iter()
    .filter(|player| *player != caster)
    .collect();
    let (transcript, breakpoints) = issued_breakpoints(state, caster, &responders);
    let epoch = state
        .precast_shortcut_runtime
        .next_epoch
        .wrapping_add(1)
        .max(1);
    state.precast_shortcut_runtime.next_epoch = epoch;
    state.precast_shortcut_runtime.offer = Some(PrecastShortcutOfferRuntime {
        caster,
        spell_id,
        epoch,
        route_id: epoch,
        responders,
        transcript,
        breakpoints,
        shortened: None,
    });
    WaitingFor::PrecastCopyShortcutOffer {
        proposer: caster,
        epoch,
        route_count: 1,
    }
}

/// Handles all pre-cast protocol actions. The public state carries only opaque
/// epoch/breakpoint ids; route validation and replay data are read solely from
/// the private sidecar.
pub(super) fn handle(
    state: &mut GameState,
    actor: PlayerId,
    epoch: u64,
    response: PrecastCopyShortcutResponse,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(offer) = state.precast_shortcut_runtime.offer.clone() else {
        return Err(EngineError::InvalidAction(
            "No live pre-cast shortcut offer".to_string(),
        ));
    };
    if offer.epoch != epoch {
        return Err(EngineError::InvalidAction(
            "Stale pre-cast shortcut epoch".to_string(),
        ));
    }

    match (&state.waiting_for, response) {
        (
            WaitingFor::PrecastCopyShortcutOffer { proposer, .. },
            PrecastCopyShortcutResponse::Decline,
        ) if actor == turn_control::authorized_submitter_for_player(state, *proposer) => {
            suppress_current_offer(state, &offer);
            Ok(priority_for(offer.caster))
        }
        (
            WaitingFor::PrecastCopyShortcutOffer { proposer, .. },
            PrecastCopyShortcutResponse::Propose { route_id },
        ) if actor == turn_control::authorized_submitter_for_player(state, *proposer)
            && route_id == offer.route_id =>
        {
            if let Some((&next, rest)) = offer.responders.split_first() {
                Ok(responder_wait(next, offer.epoch, rest, &offer.breakpoints))
            } else {
                materialize(state, &offer, None, events)
            }
        }
        (
            WaitingFor::RespondToPrecastCopyShortcut {
                player,
                remaining_players,
                ..
            },
            PrecastCopyShortcutResponse::Accept,
        ) if actor == turn_control::authorized_submitter_for_player(state, *player) => {
            if let Some((&next, rest)) = remaining_players.split_first() {
                Ok(responder_wait(next, offer.epoch, rest, &offer.breakpoints))
            } else {
                materialize(state, &offer, offer.shortened.clone(), events)
            }
        }
        (
            WaitingFor::RespondToPrecastCopyShortcut {
                player,
                remaining_players,
                ..
            },
            PrecastCopyShortcutResponse::Shorten { breakpoint_id },
        ) if actor == turn_control::authorized_submitter_for_player(state, *player) => {
            let Some(breakpoint) = offer
                .breakpoints
                .iter()
                .find(|breakpoint| breakpoint.id == breakpoint_id && breakpoint.owner == *player)
                .cloned()
            else {
                return Err(EngineError::InvalidAction(
                    "Pre-cast shortcut breakpoint is not issued to this responder".to_string(),
                ));
            };
            let mut updated = offer;
            // CR 732.2c: a later responder still gets to answer, but cannot
            // replace an earlier actual pass boundary. Prefix order is APNAP
            // order, so the smallest generated prefix wins (and equal prefixes
            // retain the first response deterministically).
            if updated
                .shortened
                .as_ref()
                .is_none_or(|current| breakpoint.prefix_length < current.prefix_length)
            {
                updated.shortened = Some(breakpoint);
            }
            state.precast_shortcut_runtime.offer = Some(updated.clone());
            if let Some((&next, rest)) = remaining_players.split_first() {
                Ok(responder_wait(
                    next,
                    updated.epoch,
                    rest,
                    &updated.breakpoints,
                ))
            } else {
                materialize(state, &updated, updated.shortened.clone(), events)
            }
        }
        _ => Err(EngineError::InvalidAction(
            "Invalid pre-cast shortcut response".to_string(),
        )),
    }
}

/// A shortened route creates a real priority boundary that must be changed by
/// its owner before either a manual or auto pass can advance it.
pub(super) fn blocks_pass(state: &GameState, actor: PlayerId) -> bool {
    state.precast_shortcut_runtime.must_diverge == Some(actor)
}

/// A meaningful game action discharges only its actor's divergence latch.
/// Preference and protocol actions intentionally do not count.
pub(super) fn note_meaningful_action(state: &mut GameState, actor: PlayerId, action: &GameAction) {
    if state.precast_shortcut_runtime.must_diverge == Some(actor)
        && !matches!(
            action,
            GameAction::PassPriority
                | GameAction::SetAutoPass { .. }
                | GameAction::CancelAutoPass
                | GameAction::SetPhaseStops { .. }
                | GameAction::SetPriorityPassingMode { .. }
                | GameAction::SetPriorityYield { .. }
                | GameAction::SetMayTriggerAutoChoice { .. }
                | GameAction::SetTriggerOrderTemplate { .. }
                | GameAction::ReorderHand { .. }
                | GameAction::PrecastCopyShortcut { .. }
        )
    {
        state.precast_shortcut_runtime.must_diverge = None;
    }
}

/// Trusted restore/takeback boundaries call this before exposing a restored
/// state. Epoch rotation makes any pre-restore client capability stale.
pub fn rekey_after_trusted_restore(state: &mut GameState) {
    let reoffer_from = match &state.waiting_for {
        WaitingFor::PrecastCopyShortcutOffer { proposer, .. } => Some(*proposer),
        WaitingFor::RespondToPrecastCopyShortcut { .. } => state
            .precast_shortcut_runtime
            .offer
            .as_ref()
            .map(|offer| offer.caster),
        _ => None,
    };
    state.precast_shortcut_runtime.offer = None;
    state.precast_shortcut_runtime.next_epoch = state
        .precast_shortcut_runtime
        .next_epoch
        .wrapping_add(1)
        .max(1);
    state.precast_shortcut_runtime.must_diverge = None;
    if let Some(caster) = reoffer_from {
        let waiting_for = maybe_offer_after_cast_triggers(state, caster, priority_for(caster));
        crate::game::public_state::sync_waiting_for(state, &waiting_for);
    }
}

/// A raw/public state carries no private route authority. If it is decoded at
/// a local boundary, drop the unvalidated prompt and resume ordinary priority.
pub fn normalize_untrusted_restore(state: &mut GameState) {
    let semantic_owner = match &state.waiting_for {
        WaitingFor::PrecastCopyShortcutOffer { proposer, .. } => Some(*proposer),
        WaitingFor::RespondToPrecastCopyShortcut { player, .. } => Some(*player),
        _ => None,
    };
    if let Some(player) = semantic_owner {
        crate::game::public_state::sync_waiting_for(state, &priority_for(player));
    }
    state.precast_shortcut_runtime = Default::default();
}

fn priority_for(player: PlayerId) -> WaitingFor {
    WaitingFor::Priority { player }
}

fn responder_wait(
    player: PlayerId,
    epoch: u64,
    remaining_players: &[PlayerId],
    breakpoints: &[PrecastShortcutBreakpoint],
) -> WaitingFor {
    WaitingFor::RespondToPrecastCopyShortcut {
        player,
        epoch,
        breakpoint_ids: breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.owner == player)
            .map(|breakpoint| breakpoint.id)
            .collect(),
        remaining_players: remaining_players.to_vec(),
    }
}

fn suppress_current_offer(state: &mut GameState, offer: &PrecastShortcutOfferRuntime) {
    state.precast_shortcut_runtime.suppressed_cast = Some(offer.spell_id);
    state.precast_shortcut_runtime.offer = None;
}

fn issued_breakpoints(
    state: &GameState,
    caster: PlayerId,
    responders: &[PlayerId],
) -> (
    Vec<PrecastShortcutReplayStep>,
    Vec<PrecastShortcutBreakpoint>,
) {
    let mut transcript = Vec::new();
    let mut breakpoints = Vec::new();
    let mut pass_owner = caster;
    for (index, responder) in responders.iter().copied().enumerate() {
        transcript.push(PrecastShortcutReplayStep {
            actor: pass_owner,
            action: GameAction::PassPriority,
        });
        breakpoints.push(PrecastShortcutBreakpoint {
            id: state
                .precast_shortcut_runtime
                .next_epoch
                .wrapping_add(index as u64 + 1)
                .max(1),
            owner: responder,
            prefix_length: transcript.len(),
            expected_priority_holder: responder,
            expected_active_player: state.active_player,
            expected_priority_passes: state.priority_passes.clone(),
            fingerprint: state.state_revision,
        });
        pass_owner = responder;
    }
    (transcript, breakpoints)
}

fn eligible_chain_spell(
    state: &GameState,
    caster: PlayerId,
) -> Option<crate::types::identifiers::ObjectId> {
    if !state
        .players
        .iter()
        .find(|player| player.id == caster)
        .is_some_and(|player| player.hand.is_empty())
    {
        return None;
    }
    state.stack.iter().find_map(|entry| {
        (entry.controller == caster)
            .then(|| entry.ability())
            .flatten()
            .filter(|ability| {
                ability.targets.as_slice() == [crate::types::ability::TargetRef::Player(caster)]
            })
            .filter(|ability| is_chain_shape(ability))
            .map(|_| entry.id)
    })
}

/// A protocol offer is valid only when a private clone can complete the exact
/// route that it advertises. In particular, the post-cast stack must contain
/// precisely the original Chain beneath its one Witherbloom Magecraft trigger,
/// and its actual copy-count authority preserves the one-copy route. The clone
/// drive below rejects every remaining interactive branch before a public wait
/// is issued, so an accepted offer cannot strand a player in an unresolvable
/// protocol state.
fn preflight_route(
    state: &GameState,
    caster: PlayerId,
    spell_id: crate::types::identifiers::ObjectId,
) -> bool {
    if !has_exact_initial_transcript(state, caster, spell_id)
        || chain_copy_count(state, spell_id) != Some(1)
    {
        return false;
    }

    let mut trial = state.clone();
    trial.precast_shortcut_runtime.materializing = true;
    trial.precast_shortcut_runtime.offer = None;
    trial.precast_shortcut_runtime.suppressed_cast = Some(spell_id);
    crate::game::public_state::sync_waiting_for(&mut trial, &priority_for(caster));
    let offer = PrecastShortcutOfferRuntime {
        caster,
        spell_id,
        epoch: 0,
        route_id: 0,
        responders: Vec::new(),
        transcript: Vec::new(),
        breakpoints: Vec::new(),
        shortened: None,
    };

    drive_full_route(&mut trial, &offer, &mut Vec::new()).is_ok()
}

/// CR 707.10 + CR 614.1a: the shortcut's fixed transcript contains exactly
/// one copy per accepted Chain choice. Delegate applicability (including the
/// copy controller, source zone, and inert `None`/`Prevent` definitions) to the
/// normal copy-count authority rather than rejecting every CopySpell definition.
fn chain_copy_count(
    state: &GameState,
    spell_id: crate::types::identifiers::ObjectId,
) -> Option<usize> {
    let chain_copy = state
        .stack
        .iter()
        .find(|entry| entry.id == spell_id)?
        .ability()?
        .sub_ability
        .as_deref()?;
    Some(crate::game::effects::copy_spell::copy_count_with_replacements(state, chain_copy, 1))
}

/// The only initial stack accepted by the finite proof is the original Chain
/// plus its one fixed Magecraft trigger. A second observer, another cast
/// trigger, or an unrelated stack object changes the route and must remain on
/// the ordinary priority path.
fn has_exact_initial_transcript(
    state: &GameState,
    caster: PlayerId,
    spell_id: crate::types::identifiers::ObjectId,
) -> bool {
    let mut entries = state.stack.iter();
    let (Some(original_chain), Some(magecraft_trigger), None) =
        (entries.next(), entries.next(), entries.next())
    else {
        return false;
    };

    original_chain.id == spell_id
        && original_chain.controller == caster
        && matches!(
            &original_chain.kind,
            crate::types::game_state::StackEntryKind::Spell {
                ability: Some(ability),
                ..
            } if is_chain_shape(ability)
        )
        && magecraft_trigger.controller == caster
        && state
            .objects
            .get(&magecraft_trigger.source_id)
            .is_some_and(|source| {
                source.controller == caster && source.name == "Witherbloom Apprentice"
            })
        && matches!(
            &magecraft_trigger.kind,
            crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. }
                if is_fixed_magecraft_ability(ability)
        )
}

fn is_chain_shape(ability: &crate::types::ability::ResolvedAbility) -> bool {
    let Some(copy) = ability.sub_ability.as_deref() else {
        return false;
    };
    is_unmodified_chain_discard(ability) && is_unmodified_chain_copy(copy)
}

/// The offer is intentionally narrower than a text-shaped similarity. Every
/// field below either changes the discard/copy route or can introduce a player
/// decision that the fixed transcript cannot honestly make on their behalf.
fn is_unmodified_chain_discard(ability: &ResolvedAbility) -> bool {
    matches!(
        &ability.effect,
        Effect::Discard {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Player,
            selection: CardSelectionMode::Chosen,
            unless_filter: None,
            filter: None,
        }
    ) && !ability.optional
        && ability.optional_for.is_none()
        && ability.else_ability.is_none()
        && ability.duration.is_none()
        && ability.condition.is_none()
        && !ability.optional_targeting
        && ability.multi_target.is_none()
        && ability.target_constraints.is_empty()
        && ability.repeat_for.is_none()
        && ability.repeat_until.is_none()
        && ability.unless_pay.is_none()
        && ability.distribution.is_none()
        && ability.player_scope.is_none()
        && ability.starting_with.is_none()
        && !ability.forward_result
}

fn is_unmodified_chain_copy(ability: &ResolvedAbility) -> bool {
    matches!(
        &ability.effect,
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
            retarget: CopyRetargetPermission::MayChooseNewTargets,
            copier: None,
            additional_modifications,
            starting_loyalty_from_casualty_sacrifice: false,
        } if additional_modifications.is_empty()
    ) && ability.optional
        && ability.optional_for.is_none()
        && ability.sub_ability.is_none()
        && ability.else_ability.is_none()
        && ability.duration.is_none()
        && ability.condition.is_none()
        && !ability.optional_targeting
        && ability.multi_target.is_none()
        && ability.target_constraints.is_empty()
        && ability.repeat_for.is_none()
        && ability.repeat_until.is_none()
        && ability.unless_pay.is_none()
        && ability.distribution.is_none()
        && ability.player_scope.is_none()
        && ability.starting_with.is_none()
        && !ability.forward_result
}

fn has_fixed_magecraft_observer(state: &GameState, controller: PlayerId) -> bool {
    state.battlefield.iter().any(|id| {
        state.objects.get(id).is_some_and(|object| {
            object.controller == controller
                && object.name == "Witherbloom Apprentice"
                && object.trigger_definitions.iter_all().any(|entry| {
                    let trigger = entry.definition();
                    trigger.mode == TriggerMode::SpellCastOrCopy
                        && trigger
                            .execute
                            .as_deref()
                            .is_some_and(is_fixed_magecraft_definition)
                })
        })
    })
}

fn is_fixed_magecraft_definition(ability: &AbilityDefinition) -> bool {
    ability.player_scope == Some(PlayerFilter::Opponent)
        && ability.cost.is_none()
        && ability.else_ability.is_none()
        && ability.duration.is_none()
        && ability.condition.is_none()
        && !ability.optional_targeting
        && !ability.optional
        && ability.optional_for.is_none()
        && ability.multi_target.is_none()
        && ability.target_constraints.is_empty()
        && ability.distribute.is_none()
        && ability.unless_pay.is_none()
        && ability.modal.is_none()
        && ability.mode_abilities.is_empty()
        && ability.repeat_for.is_none()
        && ability.repeat_until.is_none()
        && !ability.cant_be_copied
        && !ability.forward_result
        && ability.starting_with.is_none()
        && ability.target_chooser.is_none()
        && matches!(
            ability.effect.as_ref(),
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: None,
            }
        )
        && ability.sub_ability.as_deref().is_some_and(|gain| {
            matches!(
                gain.effect.as_ref(),
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                }
            ) && gain.cost.is_none()
                && gain.sub_ability.is_none()
                && gain.else_ability.is_none()
                && gain.duration.is_none()
                && gain.condition.is_none()
                && !gain.optional_targeting
                && !gain.optional
                && gain.optional_for.is_none()
                && gain.multi_target.is_none()
                && gain.target_constraints.is_empty()
                && gain.distribute.is_none()
                && gain.unless_pay.is_none()
                && gain.modal.is_none()
                && gain.mode_abilities.is_empty()
                && gain.repeat_for.is_none()
                && gain.repeat_until.is_none()
                && !gain.cant_be_copied
                && !gain.forward_result
                && gain.player_scope.is_none()
                && gain.starting_with.is_none()
                && gain.target_chooser.is_none()
        })
}

fn materialize(
    state: &mut GameState,
    offer: &PrecastShortcutOfferRuntime,
    shortened: Option<PrecastShortcutBreakpoint>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut work = state.clone();
    work.precast_shortcut_runtime.materializing = true;
    work.precast_shortcut_runtime.offer = None;
    work.precast_shortcut_runtime.suppressed_cast = Some(offer.spell_id);
    crate::game::public_state::sync_waiting_for(&mut work, &priority_for(offer.caster));

    if let Some(breakpoint) = shortened {
        replay_prefix(&mut work, offer, &breakpoint, events)?;
        work.precast_shortcut_runtime.materializing = false;
        work.precast_shortcut_runtime.must_diverge = Some(breakpoint.owner);
        *state = work;
        return Ok(state.waiting_for.clone());
    }

    drive_full_route(&mut work, offer, events)?;
    work.precast_shortcut_runtime.materializing = false;
    work.precast_shortcut_runtime.offer = None;
    *state = work;
    Ok(state.waiting_for.clone())
}

fn replay_prefix(
    state: &mut GameState,
    offer: &PrecastShortcutOfferRuntime,
    breakpoint: &PrecastShortcutBreakpoint,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if breakpoint.expected_active_player != state.active_player
        || breakpoint.expected_priority_passes != state.priority_passes
        || breakpoint.fingerprint > state.state_revision
    {
        return Err(EngineError::InvalidAction(
            "Pre-cast shortcut breakpoint no longer matches its transcript".to_string(),
        ));
    }
    for step in offer.transcript.iter().take(breakpoint.prefix_length) {
        let result = super::engine::apply_for_simulation(state, step.actor, step.action.clone())?;
        events.extend(result.events);
    }
    if !matches!(state.waiting_for, WaitingFor::Priority { player } if player == breakpoint.expected_priority_holder)
    {
        return Err(EngineError::InvalidAction(
            "Pre-cast shortcut transcript did not reach its issued priority boundary".to_string(),
        ));
    }
    Ok(())
}

fn drive_full_route(
    state: &mut GameState,
    offer: &PrecastShortcutOfferRuntime,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let mut accepted_copies = 0usize;
    let mut declined = false;
    let mut copied_lineage = Vec::new();
    let mut expected_chain_source = offer.spell_id;
    for _ in 0..REPLAY_STEP_CAP {
        if declined
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            if copied_lineage.len() != COPY_COUNT
                || state.players.iter().any(|player| player.is_eliminated)
            {
                return Err(EngineError::InvalidAction(
                    "Pre-cast shortcut route did not prove a nonlethal copy lineage".to_string(),
                ));
            }
            return Ok(());
        }
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. }
                if stack_contains_only_chain_and_magecraft(state, offer) =>
            {
                GameAction::PassPriority
            }
            WaitingFor::OptionalEffectChoice {
                player,
                source_id,
                may_trigger_key,
                ..
            } if *player == offer.caster
                && *source_id == expected_chain_source
                && may_trigger_key.is_none()
                && pending_optional_is_chain_copy(state, offer, expected_chain_source)
                && accepted_copies < COPY_COUNT =>
            {
                accepted_copies += 1;
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::OptionalEffectChoice {
                player,
                source_id,
                may_trigger_key,
                ..
            } if *player == offer.caster
                && *source_id == expected_chain_source
                && may_trigger_key.is_none()
                && pending_optional_is_chain_copy(state, offer, expected_chain_source) =>
            {
                declined = true;
                GameAction::DecideOptionalEffect { accept: false }
            }
            WaitingFor::CopyRetarget {
                player,
                copy_id,
                target_slots,
                effect_kind,
                effect_source_id,
                ..
            } if *player == offer.caster
                && *copy_id == expected_chain_source
                && *effect_kind == crate::types::ability::EffectKind::CopySpell
                && *effect_source_id == Some(*copy_id)
                && target_slots.len() == 1
                && target_slots[0].current
                    == Some(crate::types::ability::TargetRef::Player(offer.caster)) =>
            {
                GameAction::KeepAllCopyTargets
            }
            _ => {
                return Err(EngineError::InvalidAction(
                    "Pre-cast shortcut encountered a non-deterministic reducer choice".to_string(),
                ))
            }
        };
        let result = super::engine::apply_as_current_for_simulation(state, action)?;
        for event in &result.events {
            if let GameEvent::SpellCopied {
                original_id,
                object_id,
                ..
            } = event
            {
                if *original_id != expected_chain_source {
                    return Err(EngineError::InvalidAction(
                        "Pre-cast shortcut copy lineage diverged from the self-referential Chain route"
                            .to_string(),
                    ));
                }
                copied_lineage.push((*original_id, *object_id));
                expected_chain_source = *object_id;
            }
        }
        events.extend(result.events);
    }
    Err(EngineError::InvalidAction(
        "Pre-cast shortcut reducer replay exceeded its engine cap".to_string(),
    ))
}

fn pending_optional_is_chain_copy(
    state: &GameState,
    offer: &PrecastShortcutOfferRuntime,
    expected_chain_source: ObjectId,
) -> bool {
    state
        .active_optional_effect_frame()
        .map(|frame| frame.ability.as_ref())
        .is_some_and(|ability| {
            ability.controller == offer.caster
                && ability.source_id == expected_chain_source
                && is_unmodified_chain_copy(ability)
        })
}

fn stack_contains_only_chain_and_magecraft(
    state: &GameState,
    offer: &PrecastShortcutOfferRuntime,
) -> bool {
    state.stack.iter().all(|entry| {
        entry.controller == offer.caster
            && entry.ability().is_some_and(|ability| {
                is_chain_shape(ability) || is_fixed_magecraft_ability(ability)
            })
    })
}

fn is_fixed_magecraft_ability(ability: &ResolvedAbility) -> bool {
    ability.player_scope == Some(PlayerFilter::Opponent)
        && !ability.optional
        && ability.optional_for.is_none()
        && ability.else_ability.is_none()
        && ability.duration.is_none()
        && ability.condition.is_none()
        && !ability.optional_targeting
        && ability.multi_target.is_none()
        && ability.target_constraints.is_empty()
        && ability.repeat_for.is_none()
        && ability.repeat_until.is_none()
        && ability.unless_pay.is_none()
        && ability.distribution.is_none()
        && ability.starting_with.is_none()
        && !ability.forward_result
        && matches!(
            &ability.effect,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: None,
            }
        )
        && ability.sub_ability.as_deref().is_some_and(|gain| {
            matches!(
                &gain.effect,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                }
            ) && gain.sub_ability.is_none()
                && gain.else_ability.is_none()
                && !gain.optional
                && gain.optional_for.is_none()
                && gain.duration.is_none()
                && gain.condition.is_none()
                && !gain.optional_targeting
                && gain.multi_target.is_none()
                && gain.target_constraints.is_empty()
                && gain.repeat_for.is_none()
                && gain.repeat_until.is_none()
                && gain.unless_pay.is_none()
                && gain.distribution.is_none()
                && gain.player_scope.is_none()
                && gain.starting_with.is_none()
                && !gain.forward_result
        })
}
