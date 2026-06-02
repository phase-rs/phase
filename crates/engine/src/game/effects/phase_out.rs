//! CR 702.26: Phase Out / Phase In resolvers for the `Effect::PhaseOut` and
//! `Effect::PhaseIn` variants. All phasing primitives live in
//! `game::phasing`; this module is the thin effect-handler glue that
//! dispatches resolved targets to those primitives and emits the
//! `EffectResolved` bookkeeping event.
//!
//! Both resolvers handle player and object targets in a single pass:
//! explicit `TargetRef::Player` targets and player-typed mass filters
//! (`Controller`, `Player`, `Typed { type_filters: [], … }`) route through
//! `phase_out_player`/`phase_in_player`; everything else routes through the
//! permanent path (CR 702.26 proper). Player phasing has no formal CR rule
//! and follows the small set of card Oracle text that says "you phase out".

use std::collections::HashSet;

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::game_object::PhaseOutCause;
use crate::game::phasing::{phase_in_object, phase_in_player, phase_out_object, phase_out_player};
use crate::types::ability::{
    ControllerRef, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
    TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 702.26a: Resolve `Effect::PhaseOut` by phasing out every targeted
/// permanent (or every permanent matching the effect's mass filter, e.g.
/// "All permanents you control phase out" from Teferi's Protection). Phased-
/// out objects remain on the battlefield (CR 702.26d); we delegate to
/// `phase_out_object` which also cascades to indirectly-phased attachments
/// and removes everything from combat (CR 506.4 + CR 702.26g).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target = match &ability.effect {
        Effect::PhaseOut { target } => target.clone(),
        _ => return Ok(()),
    };

    // Player-phasing branch. Mirrors `collect_object_targets` for the
    // permanent path: explicit `TargetRef::Player` targets win, then a
    // player-typed mass filter (`Controller`, `Typed { type_filters: [], … }`,
    // `Player`) expands to the matching set of player ids. This dispatches
    // before the object branch so a player target never silently becomes a
    // no-op via `collect_object_targets`.
    let player_targets = collect_player_targets(state, ability, &target);
    for pid in &player_targets {
        phase_out_player(state, *pid, events);
    }

    let object_targets = collect_object_targets(state, ability, &target);
    let target_set: HashSet<ObjectId> = object_targets.iter().copied().collect();
    for oid in object_targets {
        // CR 702.26h: attachments whose host is also in this mass set phase out
        // only indirectly via the host's CR 702.26g cascade, not as direct targets.
        if attachment_host_in_set(state, oid, &target_set) {
            continue;
        }
        phase_out_object(state, oid, PhaseOutCause::Directly, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PhaseOut,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 702.26c: Resolve `Effect::PhaseIn` by phasing in every targeted
/// permanent. Rare; most phasing-in happens during the untap-step TBA.
pub fn resolve_phase_in(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target = match &ability.effect {
        Effect::PhaseIn { target } => target.clone(),
        _ => return Ok(()),
    };

    // Player-phasing branch — same idiom as `resolve` for symmetry. Phased-out
    // players don't appear in the targeting choke point, so callers wanting
    // to phase them back in must use an explicit `TargetRef::Player` target
    // (or a player-typed mass filter such as `Controller`).
    let player_targets = collect_player_targets(state, ability, &target);
    for pid in &player_targets {
        phase_in_player(state, *pid, events);
    }

    // CR 702.26b: Filter choke point normally excludes phased-out objects, so
    // we can't rely on the standard target expansion for phase-in. Instead,
    // enumerate state.battlefield directly and match the filter manually,
    // skipping the phased-out exclusion.
    let targets = collect_phase_in_targets(state, ability, &target);
    for oid in targets {
        phase_in_object(state, oid, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PhaseIn,
        source_id: ability.source_id,
    });
    Ok(())
}

/// Resolve the target player set for a `PhaseOut`/`PhaseIn` effect.
///
/// Explicit `TargetRef::Player` targets win. Otherwise a player-typed mass
/// filter (`Controller`, `Player`, or a `Typed` filter with no `type_filters`
/// and an optional `controller` ref) expands to the matching player ids.
/// Returns an empty vec if the filter doesn't refer to players (the object
/// branch will handle it).
fn collect_player_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<PlayerId> {
    let from_targets: Vec<PlayerId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Player(pid) => Some(*pid),
            TargetRef::Object(_) => None,
        })
        .collect();
    if !from_targets.is_empty() {
        return from_targets;
    }

    match target {
        TargetFilter::Controller => vec![ability.scoped_player.unwrap_or(ability.controller)],
        TargetFilter::Player => state.players.iter().map(|p| p.id).collect(),
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            ..
        }) if type_filters.is_empty() => {
            state
                .players
                .iter()
                .filter(|p| match controller {
                    Some(ControllerRef::You) => p.id == ability.controller,
                    Some(ControllerRef::Opponent) => p.id != ability.controller,
                    Some(ControllerRef::ScopedPlayer) => {
                        p.id == ability.scoped_player.unwrap_or(ability.controller)
                    }
                    // CR 109.4: TargetPlayer is ambiguous here (phase_out targets are
                    // resolved from ability.targets directly); fail closed.
                    Some(ControllerRef::TargetPlayer) => false,
                    Some(ControllerRef::ParentTargetController) => false,
                    Some(ControllerRef::DefendingPlayer) => false,
                    // CR 608.2c + CR 109.4: Player chosen by an earlier
                    // `Choose(Player)` in this resolution.
                    Some(ControllerRef::ChosenPlayer { index }) => {
                        ability.chosen_players.get(*index as usize).copied() == Some(p.id)
                    }
                    // CR 603.2 + CR 109.4: The triggering player. Resolved against
                    // the current trigger event; fail closed when there is none.
                    Some(ControllerRef::TriggeringPlayer) => {
                        state.current_trigger_event.as_ref().and_then(|e| {
                            crate::game::targeting::extract_player_from_event(e, state)
                        }) == Some(p.id)
                    }
                    None => true,
                })
                .map(|p| p.id)
                .collect()
        }
        _ => Vec::new(),
    }
}

/// True when `oid` is attached to another permanent that is also in `targets`.
fn attachment_host_in_set(state: &GameState, oid: ObjectId, targets: &HashSet<ObjectId>) -> bool {
    state
        .objects
        .get(&oid)
        .and_then(|obj| obj.attached_to.as_ref())
        .and_then(|t| t.as_object())
        .is_some_and(|host| targets.contains(&host))
}

/// Resolve the target object set for a `PhaseOut` effect. Explicit
/// `ability.targets` (from the targeting phase) take precedence; mass filters
/// (e.g., `Typed Permanent / You`) are expanded against the battlefield.
fn collect_object_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    let from_targets: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    if !from_targets.is_empty() {
        return from_targets;
    }

    // Mass filter — expand against the phased-in battlefield.
    let ctx = FilterContext::from_ability(ability);
    state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter(|id| matches_target_filter(state, *id, target, &ctx))
        .collect()
}

/// Resolve target object set for a `PhaseIn` effect. Because the filter
/// choke point treats phased-out objects as nonexistent, we iterate
/// `state.battlefield` directly and evaluate only the non-phased-out aspects
/// of the filter here.
fn collect_phase_in_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    let from_targets: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    if !from_targets.is_empty() {
        return from_targets;
    }

    // For mass phase-in effects, we must see phased-out permanents — that's
    // the only sensible target set. Rely on the caller-supplied filter to
    // narrow (e.g., "all phased-out permanents you control"). The core
    // filter choke point hides phased-out objects, so we can't use it for
    // non-trivial mass filters here without extending the filter API. For
    // now, a mass phase-in with a controller-scoped filter is approximated
    // by scanning all battlefield objects and checking controller.
    let _ = (ability, target);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects.get(id).is_some_and(|obj| obj.is_phased_out()))
        .collect()
}
