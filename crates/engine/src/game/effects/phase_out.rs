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

use crate::game::filter::{
    matches_target_filter, matches_target_filter_including_phased_out, FilterContext,
};
use crate::game::game_object::PhaseOutCause;
use crate::game::phasing::{phase_in_object, phase_in_player, phase_out_object, phase_out_player};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

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
    let player_targets =
        crate::game::ability_utils::collect_player_targets(state, ability, &target);
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
    let player_targets =
        crate::game::ability_utils::collect_player_targets(state, ability, &target);
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

    // CR 702.26b: phasing-in is one of the rare effects that specifically
    // mentions phased-out permanents, so the effect's filter must be applied to
    // the phased-out permanents themselves. `matches_target_filter_including_
    // phased_out` evaluates the filter (controller scope, type, etc.) while
    // bypassing the choke point's phased-out exclusion, so a card such as "phase
    // in each phased-out permanent you control" no longer indiscriminately
    // phases in every phased-out permanent (including an opponent's).
    let ctx = FilterContext::from_ability(ability);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let phased_out = state.objects.get(id).is_some_and(|obj| obj.is_phased_out());
            phased_out && matches_target_filter_including_phased_out(state, *id, target, &ctx)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn add_creature(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
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

    /// CR 702.26b: A mass phase-in effect specifically mentioning phased-out
    /// permanents must still honor the effect's object filter. Reverting
    /// `collect_phase_in_targets` to return every phased-out battlefield object
    /// phases in the opponent's creature and fails this regression.
    #[test]
    fn phase_in_mass_filter_only_returns_matching_phased_out_objects() {
        let mut state = GameState::new_two_player(42);
        let source = add_creature(&mut state, PlayerId(0), "Phase Source");
        let mine = add_creature(&mut state, PlayerId(0), "Mine");
        let theirs = add_creature(&mut state, PlayerId(1), "Theirs");

        let mut events = Vec::new();
        phase_out_object(&mut state, mine, PhaseOutCause::Directly, &mut events);
        phase_out_object(&mut state, theirs, PhaseOutCause::Directly, &mut events);

        let ability = ResolvedAbility::new(
            Effect::PhaseIn {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
            Vec::new(),
            source,
            PlayerId(0),
        );

        resolve_phase_in(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.objects[&mine].is_phased_out(),
            "controller's matching phased-out creature must phase in"
        );
        assert!(
            state.objects[&theirs].is_phased_out(),
            "opponent's phased-out creature must remain phased out"
        );
    }
}
