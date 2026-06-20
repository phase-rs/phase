use serde::{Deserialize, Serialize};

use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::log::GameLogEntry;
use crate::types::player::PlayerId;

use super::engine::{apply_action_boundary_with_stack_limit, PublicFinalizeMode};
use super::public_state::finalize_display_state;
use super::{players, turn_control};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveAllFastForwardResult {
    pub events: Vec<GameEvent>,
    pub waiting_for: WaitingFor,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_entries: Vec<GameLogEntry>,
    pub items_resolved: u32,
    /// Stack depth at this chunk's entry. The frontend latches the first
    /// chunk's `total` as the storm-origin denominator for progress display.
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolveAllCallbackDecision {
    Action(GameAction),
    Stop,
}

enum PriorityCycleFastForward {
    Seeded,
    CannotSeed,
    Stop,
}

pub fn resolve_all_fast_forward<F>(
    state: &mut GameState,
    requester: PlayerId,
    max_resolutions: u32,
    mut choose_non_requester_action: F,
) -> ResolveAllFastForwardResult
where
    F: FnMut(&GameState, PlayerId) -> ResolveAllCallbackDecision,
{
    let total = state.stack.len();
    let resolution_cap = if max_resolutions == 0 {
        u32::MAX
    } else {
        max_resolutions
    };
    // CR 117.4: fast-forwarding priority is only a shortcut over repeated
    // passes. The guard is not progress accounting; StackResolved events are.
    let max_iterations = total
        .saturating_mul(state.players.len())
        .saturating_mul(4)
        .clamp(100, 20_000);

    let mut events = Vec::new();
    let mut log_entries = Vec::new();
    let mut items_resolved = 0u32;
    let mut deferred_display_pending = false;

    for _ in 0..max_iterations {
        let semantic_priority_seat = match &state.waiting_for {
            WaitingFor::Priority { player } => *player,
            WaitingFor::GameOver { .. } => break,
            _ => break,
        };

        if state.stack.is_empty() || state.stack.len() > total {
            break;
        }

        let actor = turn_control::authorized_submitter_for_player(state, semantic_priority_seat);
        let (action, mode, stop_after_boundary) = if actor == requester {
            (
                GameAction::PassPriority,
                PublicFinalizeMode::DeferredDisplay,
                false,
            )
        } else {
            if deferred_display_pending {
                finalize_display_state(state);
                deferred_display_pending = false;
            }
            match choose_non_requester_action(state, actor) {
                ResolveAllCallbackDecision::Action(GameAction::PassPriority) => (
                    GameAction::PassPriority,
                    PublicFinalizeMode::DeferredDisplay,
                    false,
                ),
                ResolveAllCallbackDecision::Action(action) => {
                    (action, PublicFinalizeMode::Immediate, true)
                }
                ResolveAllCallbackDecision::Stop => break,
            }
        };

        if matches!(action, GameAction::PassPriority) && !state.stack.is_empty() {
            match seed_remaining_priority_cycle_passes(
                state,
                semantic_priority_seat,
                requester,
                &mut choose_non_requester_action,
            ) {
                PriorityCycleFastForward::Seeded | PriorityCycleFastForward::CannotSeed => {}
                PriorityCycleFastForward::Stop => break,
            }
        }

        let remaining_resolution_cap = resolution_cap.saturating_sub(items_resolved).max(1);
        let stack_resolution_limit =
            matches!(action, GameAction::PassPriority).then_some(remaining_resolution_cap);
        let Ok(boundary) = apply_action_boundary_with_stack_limit(
            state,
            actor,
            action,
            mode,
            stack_resolution_limit,
        ) else {
            break;
        };

        if matches!(mode, PublicFinalizeMode::DeferredDisplay) {
            deferred_display_pending = true;
        }

        let resolved_this_boundary = stack_resolved_count(&boundary.events);
        let halted = has_resolution_halted(&boundary.events);
        events.extend(boundary.events);
        log_entries.extend(boundary.log_entries);

        if resolved_this_boundary > 0 {
            items_resolved = items_resolved.saturating_add(resolved_this_boundary);
            if items_resolved >= resolution_cap {
                break;
            }
        }
        if halted || stop_after_boundary {
            break;
        }
    }

    if deferred_display_pending {
        finalize_display_state(state);
    }

    ResolveAllFastForwardResult {
        events,
        waiting_for: state.waiting_for.clone(),
        log_entries,
        items_resolved,
        total: total as u32,
    }
}

fn seed_remaining_priority_cycle_passes<F>(
    state: &mut GameState,
    current_seat: PlayerId,
    requester: PlayerId,
    choose_non_requester_action: &mut F,
) -> PriorityCycleFastForward
where
    F: FnMut(&GameState, PlayerId) -> ResolveAllCallbackDecision,
{
    let mut seat = players::next_player(state, current_seat);
    let mut seeded = Vec::new();

    while seat != current_seat {
        if !state.priority_passes.contains(&seat) {
            let actor = turn_control::authorized_submitter_for_player(state, seat);
            if actor != requester {
                match choose_non_requester_action(state, actor) {
                    ResolveAllCallbackDecision::Action(GameAction::PassPriority) => {}
                    ResolveAllCallbackDecision::Action(_) => {
                        return PriorityCycleFastForward::CannotSeed;
                    }
                    ResolveAllCallbackDecision::Stop => return PriorityCycleFastForward::Stop,
                }
            }
            seeded.push(seat);
        }

        let next = players::next_player(state, seat);
        if next == seat {
            break;
        }
        seat = next;
    }

    for seat in seeded {
        state.priority_passes.insert(seat);
    }

    PriorityCycleFastForward::Seeded
}

fn stack_resolved_count(events: &[GameEvent]) -> u32 {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
        .count() as u32
}

fn has_resolution_halted(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::ResolutionHalted { .. }))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, CopyRetargetPermission, Effect,
        ManaContribution, ManaProduction, ResolvedAbility, TargetFilter,
    };
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::game_state::{PublicStateDirty, StackEntry, StackEntryKind};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaColor;
    use crate::types::phase::Phase;
    use crate::types::zones::Zone;

    use super::super::public_state::{finalize_public_state, mark_public_state_all_dirty};
    use super::*;

    fn no_op_entry(id: u64, controller: PlayerId) -> StackEntry {
        let object_id = ObjectId(id);
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller,
            kind: StackEntryKind::ActivatedAbility {
                source_id: object_id,
                ability: ResolvedAbility::new(Effect::NoOp, vec![], object_id, controller),
            },
        }
    }

    fn self_copy_entry(id: u64, controller: PlayerId) -> StackEntry {
        let object_id = ObjectId(id);
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller,
            kind: StackEntryKind::ActivatedAbility {
                source_id: object_id,
                ability: ResolvedAbility::new(
                    Effect::CopySpell {
                        target: TargetFilter::SelfRef,
                        retarget: CopyRetargetPermission::KeepOriginalTargets,
                        copier: None,
                        additional_modifications: Vec::new(),
                        starting_loyalty_from_casualty_sacrifice: false,
                    },
                    vec![],
                    object_id,
                    controller,
                ),
            },
        }
    }

    fn priority_state(semantic_seat: PlayerId, stack: Vec<StackEntry>) -> GameState {
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: semantic_seat,
        };
        state.priority_player = semantic_seat;
        state.stack = stack.into_iter().collect();
        state
    }

    fn stop_callback(_: &GameState, _: PlayerId) -> ResolveAllCallbackDecision {
        ResolveAllCallbackDecision::Stop
    }

    fn make_mana_land(state: &mut GameState) -> ObjectId {
        let land_id = create_object(
            state,
            CardId(2),
            PlayerId(0),
            "Gemstone Mine".to_string(),
            Zone::Battlefield,
        );
        let land = state.objects.get_mut(&land_id).unwrap();
        land.base_card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        };
        land.card_types = land.base_card_types.clone();
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        land.abilities = std::sync::Arc::new(vec![ability]);
        land_id
    }

    #[test]
    fn counts_net_flat_stack_resolution() {
        let mut state = priority_state(PlayerId(0), vec![self_copy_entry(1, PlayerId(0))]);
        state.priority_passes.insert(PlayerId(1));

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 1, stop_callback);

        assert_eq!(result.items_resolved, 1);
        assert_eq!(result.total, 1);
        assert_eq!(state.stack.len(), 1);
        assert!(
            result.events.iter().any(|event| {
                matches!(
                    event,
                    GameEvent::StackResolved {
                        object_id: ObjectId(1)
                    }
                )
            }),
            "net-flat resolution must count StackResolved even when stack depth remains unchanged"
        );
    }

    #[test]
    fn requester_last_pass_resolves_top_stack_entry() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        state.priority_passes.insert(PlayerId(1));

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, stop_callback);

        assert_eq!(result.items_resolved, 1);
        assert!(state.stack.is_empty());
    }

    #[test]
    fn all_pass_cycle_resolves_without_intermediate_priority_events() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        let calls = Cell::new(0);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, |_, _| {
            calls.set(calls.get() + 1);
            ResolveAllCallbackDecision::Action(GameAction::PassPriority)
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(result.items_resolved, 1);
        assert!(state.stack.is_empty());
        assert!(
            !result
                .events
                .iter()
                .any(|event| matches!(event, GameEvent::PriorityPassed { .. })),
            "Resolve All seeds accepted priority passes instead of emitting every intermediate pass"
        );
    }

    #[test]
    fn future_non_pass_callback_prevents_priority_cycle_seeding() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        let calls = Cell::new(0);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, |_, _| {
            calls.set(calls.get() + 1);
            ResolveAllCallbackDecision::Action(GameAction::SetPhaseStops {
                stops: vec![Phase::PreCombatMain],
            })
        });

        assert_eq!(calls.get(), 2);
        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 1);
        assert_eq!(
            state.phase_stops.get(&PlayerId(1)),
            Some(&vec![Phase::PreCombatMain])
        );
    }

    #[test]
    fn soft_cap_stops_after_counted_stack_resolution() {
        let mut state = priority_state(
            PlayerId(0),
            vec![no_op_entry(1, PlayerId(0)), no_op_entry(2, PlayerId(0))],
        );
        state.priority_passes.insert(PlayerId(1));

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 1, stop_callback);

        assert_eq!(result.items_resolved, 1);
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn routes_controlled_turn_priority_to_authorized_requester() {
        let mut state = priority_state(PlayerId(1), vec![no_op_entry(1, PlayerId(1))]);
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.priority_player = PlayerId(0);
        state.priority_passes.insert(PlayerId(0));

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, stop_callback);

        assert_eq!(result.items_resolved, 1);
        assert!(state.stack.is_empty());
    }

    #[test]
    fn stops_when_callback_stops_for_non_requester() {
        let mut state = priority_state(PlayerId(1), vec![no_op_entry(1, PlayerId(1))]);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, stop_callback);

        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 1);
        assert!(result.events.is_empty());
        assert_eq!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        );
    }

    #[test]
    fn non_pass_callback_action_applies_once_and_stops() {
        let mut state = priority_state(PlayerId(1), vec![no_op_entry(1, PlayerId(1))]);
        let calls = Cell::new(0);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, |_, _| {
            calls.set(calls.get() + 1);
            ResolveAllCallbackDecision::Action(GameAction::SetPhaseStops {
                stops: vec![Phase::PreCombatMain],
            })
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 1);
        assert_eq!(
            state.phase_stops.get(&PlayerId(1)),
            Some(&vec![Phase::PreCombatMain])
        );
    }

    #[test]
    fn callback_sees_display_finalized_after_deferred_boundary() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        state.active_player = PlayerId(1);
        state.priority_passes.insert(PlayerId(1));
        let land_id = make_mana_land(&mut state);
        mark_public_state_all_dirty(&mut state);
        finalize_public_state(&mut state);
        assert!(state.objects[&land_id].has_mana_ability);
        state.objects.get_mut(&land_id).unwrap().tapped = true;
        mark_public_state_all_dirty(&mut state);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, |callback_state, _| {
            assert_eq!(
                callback_state.public_state_dirty,
                PublicStateDirty::default()
            );
            assert!(!callback_state.objects[&land_id].has_mana_ability);
            ResolveAllCallbackDecision::Stop
        });

        assert_eq!(result.items_resolved, 1);
        assert!(!state.objects[&land_id].has_mana_ability);
    }

    #[test]
    fn final_deferred_boundary_flushes_display_before_return() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        state.priority_passes.insert(PlayerId(1));
        let land_id = make_mana_land(&mut state);
        mark_public_state_all_dirty(&mut state);
        finalize_public_state(&mut state);
        assert!(state.objects[&land_id].has_mana_ability);
        state.objects.get_mut(&land_id).unwrap().tapped = true;
        mark_public_state_all_dirty(&mut state);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, stop_callback);

        assert_eq!(result.items_resolved, 1);
        assert_eq!(state.public_state_dirty, PublicStateDirty::default());
        assert!(!state.objects[&land_id].has_mana_ability);
    }
}
