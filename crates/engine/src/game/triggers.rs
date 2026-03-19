use std::collections::HashMap;

use crate::types::ability::{
    AbilityDefinition, Effect, ModalChoice, ResolvedAbility, TargetFilter, TargetRef,
    TriggerCondition, TriggerDefinition,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind, TargetSelectionConstraint};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::player::{Player, PlayerId};
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::ability_utils::build_resolved_from_def;
use super::stack;

// Re-export so existing `use crate::game::triggers::build_trigger_registry` paths still work.
pub use super::trigger_matchers::build_trigger_registry;

/// Function signature for trigger matchers: returns true if event matches the trigger.
pub type TriggerMatcher = fn(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool;

/// A trigger that matched an event and is waiting to be placed on the stack.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingTrigger {
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub condition: Option<TriggerCondition>,
    pub ability: ResolvedAbility,
    pub timestamp: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_constraints: Vec<TargetSelectionConstraint>,
    /// CR 603.7c: The event that caused this trigger to fire, for event-context resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_event: Option<GameEvent>,
    /// CR 700.2a: Modal trigger data for deferred mode selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<ModalChoice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mode_abilities: Vec<AbilityDefinition>,
}

#[allow(clippy::too_many_arguments)]
/// Check trigger definitions on an object against an event, collecting matches into `pending`.
///
/// When `zone_filter` is `Some(zone)`, only trigger definitions whose `trigger_zones`
/// contains that zone will be checked. This enables graveyard (and future exile) triggers
/// without scanning every zone unconditionally.
fn collect_matching_triggers(
    state: &mut GameState,
    registry: &HashMap<TriggerMode, TriggerMatcher>,
    event: &GameEvent,
    obj_id: ObjectId,
    controller: PlayerId,
    trigger_defs: &[TriggerDefinition],
    timestamp: u32,
    zone_filter: Option<Zone>,
    pending: &mut Vec<PendingTrigger>,
) {
    for (trig_idx, trig_def) in trigger_defs.iter().enumerate() {
        // When scanning a non-battlefield zone, only check triggers declared for that zone
        if let Some(zone) = zone_filter {
            if !trig_def.trigger_zones.contains(&zone) {
                continue;
            }
        }
        if let Some(matcher) = registry.get(&trig_def.mode) {
            if matcher(event, trig_def, obj_id, state) {
                if !check_trigger_constraint(state, trig_def, obj_id, trig_idx, controller) {
                    continue;
                }
                if let Some(ref condition) = trig_def.condition {
                    if !check_trigger_condition(state, condition, controller, Some(obj_id)) {
                        continue;
                    }
                }
                let ability = build_triggered_ability(trig_def, obj_id, controller);
                let (modal, mode_abilities) = trig_def
                    .execute
                    .as_ref()
                    .map(|exec| (exec.modal.clone(), exec.mode_abilities.clone()))
                    .unwrap_or_default();
                pending.push(PendingTrigger {
                    source_id: obj_id,
                    controller,
                    condition: trig_def.condition.clone(),
                    ability,
                    timestamp,
                    target_constraints: Vec::new(),
                    trigger_event: Some(event.clone()),
                    modal,
                    mode_abilities,
                });
                record_trigger_fired(state, trig_def, obj_id, trig_idx);
            }
        }
    }
}

/// Process events and place triggered abilities on the stack in APNAP order.
pub fn process_triggers(state: &mut GameState, events: &[GameEvent]) {
    let registry = build_trigger_registry();
    let mut pending: Vec<PendingTrigger> = Vec::new();

    for event in events {
        // Scan all permanents on the battlefield for matching triggers
        let battlefield_ids: Vec<ObjectId> = state.battlefield.clone();
        for obj_id in battlefield_ids {
            let (controller, trigger_defs, timestamp, has_prowess) = {
                let obj = match state.objects.get(&obj_id) {
                    Some(o) => o,
                    None => continue,
                };
                (
                    obj.controller,
                    obj.trigger_definitions.clone(),
                    obj.entered_battlefield_turn.unwrap_or(0),
                    obj.has_keyword(&Keyword::Prowess),
                )
            };

            collect_matching_triggers(
                state,
                &registry,
                event,
                obj_id,
                controller,
                &trigger_defs,
                timestamp,
                None,
                &mut pending,
            );

            // Keyword-based triggers: Prowess
            // Prowess triggers when the controller casts a noncreature spell.
            // Cards define Prowess as K:Prowess with no explicit trigger_definition,
            // so we synthetically generate the trigger here.
            if let GameEvent::SpellCast {
                card_id,
                controller: caster,
            } = event
            {
                if has_prowess && *caster == controller {
                    // Check if the cast spell is noncreature
                    let is_noncreature = state
                        .objects
                        .iter()
                        .find(|(_, obj)| obj.card_id == *card_id)
                        .map(|(_, obj)| !obj.card_types.core_types.contains(&CoreType::Creature))
                        .unwrap_or(false);

                    if is_noncreature {
                        let prowess_effect = Effect::Pump {
                            power: crate::types::ability::PtValue::Fixed(1),
                            toughness: crate::types::ability::PtValue::Fixed(1),
                            target: TargetFilter::SelfRef,
                        };
                        let prowess_ability =
                            ResolvedAbility::new(prowess_effect, Vec::new(), obj_id, controller);
                        let prowess_trig_def = TriggerDefinition::new(TriggerMode::SpellCast)
                            .description("Prowess".to_string());
                        pending.push(PendingTrigger {
                            source_id: obj_id,
                            controller,
                            condition: prowess_trig_def.condition,
                            ability: prowess_ability,
                            timestamp,
                            target_constraints: Vec::new(),
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                        });
                    }
                }
            }
        }

        // Scan graveyard objects for triggers with trigger_zones containing Graveyard
        let graveyard_ids: Vec<ObjectId> = state
            .players
            .iter()
            .flat_map(|p| p.graveyard.iter().copied())
            .collect();
        for obj_id in graveyard_ids {
            let (controller, trigger_defs) = {
                let obj = match state.objects.get(&obj_id) {
                    Some(o) => o,
                    None => continue,
                };
                (obj.controller, obj.trigger_definitions.clone())
            };

            collect_matching_triggers(
                state,
                &registry,
                event,
                obj_id,
                controller,
                &trigger_defs,
                0,
                Some(Zone::Graveyard),
                &mut pending,
            );
        }

        // CR 722.3: At the beginning of the monarch's end step, the monarch draws a card.
        // Synthetic game-rule trigger — not attached to any permanent.
        if let GameEvent::PhaseChanged { phase: Phase::End } = event {
            if let Some(monarch_id) = state.monarch {
                if monarch_id == state.active_player {
                    let draw_effect = Effect::Draw {
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    };
                    let draw_ability =
                        ResolvedAbility::new(draw_effect, Vec::new(), ObjectId(0), monarch_id);
                    let trig_def = TriggerDefinition::new(TriggerMode::Phase)
                        .description("Monarch draw (CR 722.3)".to_string());
                    pending.push(PendingTrigger {
                        source_id: ObjectId(0),
                        controller: monarch_id,
                        condition: trig_def.condition,
                        ability: draw_ability,
                        timestamp: 0,
                        target_constraints: Vec::new(),
                        trigger_event: Some(event.clone()),
                        modal: None,
                        mode_abilities: vec![],
                    });
                }
            }
        }

        // CR 722.4: When a creature deals combat damage to the monarch, that creature's
        // controller becomes the monarch. Synthetic game-rule trigger.
        if let GameEvent::DamageDealt {
            source_id,
            target: TargetRef::Player(target_player),
            is_combat: true,
            ..
        } = event
        {
            if state.monarch == Some(*target_player) {
                // The attacking creature's controller becomes the monarch
                if let Some(attacker) = state.objects.get(source_id) {
                    let new_monarch = attacker.controller;
                    if new_monarch != *target_player {
                        let become_effect = Effect::BecomeMonarch;
                        let become_ability = ResolvedAbility::new(
                            become_effect,
                            Vec::new(),
                            *source_id,
                            new_monarch,
                        );
                        let trig_def = TriggerDefinition::new(TriggerMode::DamageDone)
                            .description("Monarch steal (CR 722.4)".to_string());
                        pending.push(PendingTrigger {
                            source_id: *source_id,
                            controller: new_monarch,
                            condition: trig_def.condition,
                            ability: become_ability,
                            timestamp: 0,
                            target_constraints: Vec::new(),
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                        });
                    }
                }
            }
        }
    }

    if pending.is_empty() {
        return;
    }

    // APNAP ordering: active player's triggers first on stack (resolve last),
    // then non-active player's. Within same controller, order by timestamp.
    pending.sort_by_key(|t| {
        let is_nap = if t.controller == state.active_player {
            0
        } else {
            1
        };
        (is_nap, t.timestamp)
    });

    // Reverse so NAP triggers are placed first (bottom of stack), AP triggers last (top).
    // CR 603.3b: LIFO means AP triggers resolve last (APNAP ordering).
    pending.reverse();

    let mut events_out = Vec::new();
    for trigger in pending {
        // CR 700.2a: Modal triggered ability — stash for mode selection before pushing to stack.
        if trigger.modal.is_some() && !trigger.mode_abilities.is_empty() {
            state.pending_trigger = Some(trigger);
            return;
        }

        let target_slots = match super::ability_utils::build_target_slots(state, &trigger.ability) {
            Ok(target_slots) => target_slots,
            Err(_) => continue,
        };

        if target_slots.is_empty() {
            push_pending_trigger_to_stack(state, trigger, &mut events_out);
            continue;
        }

        match super::ability_utils::auto_select_targets(&target_slots, &trigger.target_constraints)
        {
            Ok(Some(targets)) => {
                let mut trigger = trigger;
                if super::ability_utils::assign_targets_in_chain(&mut trigger.ability, &targets)
                    .is_err()
                {
                    continue;
                }
                super::casting::emit_targeting_events(
                    state,
                    &super::ability_utils::flatten_targets_in_chain(&trigger.ability),
                    trigger.source_id,
                    trigger.controller,
                    &mut events_out,
                );
                push_pending_trigger_to_stack(state, trigger, &mut events_out);
            }
            Ok(None) => {
                state.pending_trigger = Some(trigger);
                return;
            }
            Err(_) => continue,
        }
    }
}

pub fn push_pending_trigger_to_stack(
    state: &mut GameState,
    trigger: PendingTrigger,
    events: &mut Vec<GameEvent>,
) {
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let entry = StackEntry {
        id: entry_id,
        source_id: trigger.source_id,
        controller: trigger.controller,
        kind: StackEntryKind::TriggeredAbility {
            source_id: trigger.source_id,
            ability: trigger.ability,
            condition: trigger.condition,
            trigger_event: trigger.trigger_event,
        },
    };
    stack::push_to_stack(state, entry, events);
}

/// CR 603.7: Check if any delayed triggers should fire based on recent events.
/// Matching delayed triggers are removed from state and placed on the stack.
pub fn check_delayed_triggers(state: &mut GameState, events: &[GameEvent]) -> Vec<GameEvent> {
    if state.delayed_triggers.is_empty() {
        return vec![];
    }

    let mut fired_indices = Vec::new();

    for (idx, delayed) in state.delayed_triggers.iter().enumerate() {
        if delayed_trigger_matches(&delayed.condition, events, state) {
            fired_indices.push(idx);
        }
    }

    if fired_indices.is_empty() {
        return vec![];
    }

    // Remove in reverse order to preserve indices
    let mut fired = Vec::new();
    for &idx in fired_indices.iter().rev() {
        fired.push(state.delayed_triggers.remove(idx));
    }
    fired.reverse(); // Restore original order

    let mut new_events = Vec::new();

    // CR 603.3b: APNAP ordering — active player's triggers go on stack last (resolve first).
    // Sort so NAP triggers come first (pushed to stack bottom), AP triggers last (stack top).
    fired.sort_by_key(|t| {
        let is_nap = if t.controller == state.active_player {
            0
        } else {
            1
        };
        (is_nap, state.turn_number)
    });
    fired.reverse();

    for trigger in fired {
        let pending = PendingTrigger {
            source_id: trigger.source_id,
            controller: trigger.controller,
            condition: None,
            ability: trigger.ability,
            timestamp: state.turn_number,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
        };
        push_pending_trigger_to_stack(state, pending, &mut new_events);
    }

    new_events
}

/// CR 603.7: Check if a delayed trigger condition is met by recent events.
fn delayed_trigger_matches(
    condition: &crate::types::ability::DelayedTriggerCondition,
    events: &[GameEvent],
    state: &GameState,
) -> bool {
    use crate::types::ability::DelayedTriggerCondition;

    match condition {
        DelayedTriggerCondition::AtNextPhase { phase } => events
            .iter()
            .any(|e| matches!(e, GameEvent::PhaseChanged { phase: p } if p == phase)),
        DelayedTriggerCondition::AtNextPhaseForPlayer { phase, player } => {
            state.active_player == *player
                && events
                    .iter()
                    .any(|e| matches!(e, GameEvent::PhaseChanged { phase: p } if p == phase))
        }
        DelayedTriggerCondition::WhenLeavesPlay { object_id } => events.iter().any(|e| {
            matches!(e,
                GameEvent::ZoneChanged { object_id: id, from: Zone::Battlefield, .. }
                if *id == *object_id
            )
        }),
    }
}

/// Check whether a trigger's constraint allows it to fire.
fn check_trigger_constraint(
    state: &GameState,
    trig_def: &TriggerDefinition,
    obj_id: ObjectId,
    trig_idx: usize,
    controller: PlayerId,
) -> bool {
    use crate::types::ability::TriggerConstraint;

    let constraint = match &trig_def.constraint {
        Some(c) => c,
        None => return true, // No constraint — always fires
    };

    let key = (obj_id, trig_idx);

    match constraint {
        TriggerConstraint::OncePerTurn => !state.triggers_fired_this_turn.contains(&key),
        TriggerConstraint::OncePerGame => !state.triggers_fired_this_game.contains(&key),
        TriggerConstraint::OnlyDuringYourTurn => state.active_player == controller,
        TriggerConstraint::NthSpellThisTurn { n } => state.spells_cast_this_turn as u32 == *n,
        TriggerConstraint::NthDrawThisTurn { n } => state
            .players
            .iter()
            .find(|p| p.id == controller)
            .is_some_and(|p| p.cards_drawn_this_turn == *n),
        // CR 716.5: "When this Class becomes level N" — fire only at the specified level.
        TriggerConstraint::AtClassLevel { level } => state
            .objects
            .get(&obj_id)
            .and_then(|obj| obj.class_level)
            .is_some_and(|current| current == *level),
    }
}

/// Check whether an intervening-if condition is satisfied.
/// Used both at fire-time and resolution-time.
///
/// Predicates check player/game state directly.
/// Combinators (`And`/`Or`) recurse into their children.
///
/// `source_id` is required for conditions like `SolveConditionMet` that need
/// to inspect the trigger's source object (e.g., the Case's solve condition).
pub(crate) fn check_trigger_condition(
    state: &GameState,
    condition: &TriggerCondition,
    controller: PlayerId,
    source_id: Option<ObjectId>,
) -> bool {
    match condition {
        TriggerCondition::GainedLife { minimum } => {
            player_field(state, controller, |p| p.life_gained_this_turn >= *minimum)
        }
        TriggerCondition::LostLife => {
            player_field(state, controller, |p| p.life_lost_this_turn > 0)
        }
        TriggerCondition::Descended => player_field(state, controller, |p| p.descended_this_turn),
        TriggerCondition::ControlCreatures { minimum } => {
            let count = state
                .battlefield
                .iter()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == controller
                            && obj.card_types.core_types.contains(&CoreType::Creature)
                    })
                })
                .count();
            count >= *minimum as usize
        }
        // CR 719.2: True when the source Case is unsolved and its solve condition is met.
        TriggerCondition::SolveConditionMet => source_id
            .and_then(|id| state.objects.get(&id))
            .and_then(|obj| obj.case_state.as_ref())
            .is_some_and(|cs| !cs.is_solved && evaluate_solve_condition(state, cs, controller)),
        // CR 716.6: True when the source Class is at or above the specified level.
        TriggerCondition::ClassLevelGE { level } => source_id
            .and_then(|id| state.objects.get(&id))
            .and_then(|obj| obj.class_level)
            .is_some_and(|current| current >= *level),
        TriggerCondition::And { conditions } => conditions
            .iter()
            .all(|c| check_trigger_condition(state, c, controller, source_id)),
        TriggerCondition::Or { conditions } => conditions
            .iter()
            .any(|c| check_trigger_condition(state, c, controller, source_id)),
    }
}

/// CR 719.2: Evaluate a Case's solve condition against the current game state.
/// Returns true when the Case is unsolved and its condition is currently met.
fn evaluate_solve_condition(
    state: &GameState,
    cs: &crate::game::game_object::CaseState,
    controller: PlayerId,
) -> bool {
    use crate::types::ability::SolveCondition;

    match &cs.solve_condition {
        SolveCondition::ObjectCount {
            filter,
            comparator,
            threshold,
        } => {
            let count = state
                .battlefield
                .iter()
                .filter(|&&id| {
                    state.objects.get(&id).is_some_and(|obj| {
                        obj.controller == controller
                            && super::filter::matches_target_filter(state, id, filter, id)
                    })
                })
                .count() as i32;
            comparator.clone().evaluate(count, *threshold as i32)
        }
        SolveCondition::Text { .. } => false, // Undecomposed conditions never auto-solve
    }
}

/// Helper to check a predicate against the controller's player state.
fn player_field(state: &GameState, controller: PlayerId, f: impl Fn(&Player) -> bool) -> bool {
    state
        .players
        .iter()
        .find(|p| p.id == controller)
        .map(f)
        .unwrap_or(false)
}

/// Record that a constrained trigger has fired.
fn record_trigger_fired(
    state: &mut GameState,
    trig_def: &TriggerDefinition,
    obj_id: ObjectId,
    trig_idx: usize,
) {
    use crate::types::ability::TriggerConstraint;

    let constraint = match &trig_def.constraint {
        Some(c) => c,
        None => return, // No constraint — nothing to track
    };

    let key = (obj_id, trig_idx);

    match constraint {
        TriggerConstraint::OncePerTurn => {
            state.triggers_fired_this_turn.insert(key);
        }
        TriggerConstraint::OncePerGame => {
            state.triggers_fired_this_game.insert(key);
        }
        TriggerConstraint::OnlyDuringYourTurn
        | TriggerConstraint::NthSpellThisTurn { .. }
        | TriggerConstraint::NthDrawThisTurn { .. }
        | TriggerConstraint::AtClassLevel { .. } => {
            // No tracking needed — checked at fire time via game/object state
        }
    }
}

/// Build a ResolvedAbility from a TriggerDefinition using typed fields.
fn build_triggered_ability(
    trig_def: &TriggerDefinition,
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    if let Some(execute) = &trig_def.execute {
        // Pre-resolved ability definition -- direct typed access
        build_resolved_from_def(execute, source_id, controller)
    } else {
        // Trigger with no execute -- use Unimplemented as no-op marker
        ResolvedAbility::new(
            Effect::Unimplemented {
                name: "TriggerNoExecute".to_string(),
                description: None,
            },
            Vec::new(),
            source_id,
            controller,
        )
    }
}

/// Extract the TargetFilter from an effect, if it has targeting requirements.
/// Returns None for effects with no targeting (Draw, GainLife, etc.) or
/// effects targeting self/controller (which don't need player selection).
///
/// Note: TriggeringSpellController, TriggeringSpellOwner, TriggeringPlayer,
/// and TriggeringSource auto-resolve from event context at resolution time
/// (via `state.current_trigger_event`), so they do not require player selection.
pub(crate) fn extract_target_filter_from_effect(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::ChangeZone { target, .. }
        | Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::Destroy { target, .. }
        | Effect::Counter { target, .. }
        | Effect::Tap { target, .. }
        | Effect::Untap { target, .. }
        | Effect::Sacrifice { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::Attach { target, .. }
        | Effect::Fight { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::CopySpell { target, .. }
        | Effect::AddCounter { target, .. }
        | Effect::RemoveCounter { target, .. }
        | Effect::PutCounter { target, .. }
        | Effect::Transform { target, .. }
        | Effect::RevealHand { target, .. }
        | Effect::PreventDamage { target, .. } => {
            if matches!(
                target,
                TargetFilter::None
                    | TargetFilter::SelfRef
                    | TargetFilter::Controller
                    | TargetFilter::TriggeringSpellController
                    | TargetFilter::TriggeringSpellOwner
                    | TargetFilter::TriggeringPlayer
                    | TargetFilter::TriggeringSource
                    | TargetFilter::DefendingPlayer
                    | TargetFilter::ParentTarget
            ) {
                None
            } else {
                Some(target)
            }
        }
        Effect::GenericEffect {
            target: Some(target),
            ..
        } => {
            if matches!(
                target,
                TargetFilter::None
                    | TargetFilter::SelfRef
                    | TargetFilter::Controller
                    | TargetFilter::TriggeringSpellController
                    | TargetFilter::TriggeringSpellOwner
                    | TargetFilter::TriggeringPlayer
                    | TargetFilter::TriggeringSource
                    | TargetFilter::DefendingPlayer
                    | TargetFilter::ParentTarget
            ) {
                None
            } else {
                Some(target)
            }
        }
        _ => None,
    }
}
// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::game::filter::matches_target_filter;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, FilterProp, GainLifePlayer, QuantityExpr,
        TargetFilter, TriggerDefinition, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    /// Helper to create a minimal TriggerDefinition with typed fields.
    fn make_trigger(mode: TriggerMode) -> TriggerDefinition {
        TriggerDefinition::new(mode)
    }

    #[test]
    fn apnap_ordering() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create two creatures with triggers on battlefield
        let p0_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&p0_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        let p1_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&p1_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.controller = PlayerId(1);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        // Trigger event
        let events = vec![GameEvent::ZoneChanged {
            object_id: ObjectId(99),
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        // Both triggers should be on the stack
        assert_eq!(state.stack.len(), 2);

        // AP (P0) triggers should be on top of stack (resolve last = placed last)
        // NAP (P1) triggers should be on bottom (resolve first = placed first)
        let top = &state.stack[state.stack.len() - 1];
        let bottom = &state.stack[0];
        assert_eq!(top.controller, PlayerId(0), "AP trigger should be on top");
        assert_eq!(
            bottom.controller,
            PlayerId(1),
            "NAP trigger should be on bottom"
        );
    }

    #[test]
    fn card_matches_filter_creature() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let creature_filter = TargetFilter::Typed(TypedFilter::creature());
        let land_filter = TargetFilter::Typed(TypedFilter::land());
        assert!(matches_target_filter(
            &state,
            id,
            &creature_filter,
            ObjectId(99)
        ));
        assert!(!matches_target_filter(
            &state,
            id,
            &land_filter,
            ObjectId(99)
        ));
        assert!(matches_target_filter(
            &state,
            id,
            &TargetFilter::Any,
            ObjectId(99)
        ));
    }

    #[test]
    fn card_matches_filter_you_ctrl() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let opp_target = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opp Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let creature_you_ctrl =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));
        assert!(matches_target_filter(
            &state,
            target,
            &creature_you_ctrl,
            source
        ));
        assert!(!matches_target_filter(
            &state,
            opp_target,
            &creature_you_ctrl,
            source
        ));
    }

    #[test]
    fn card_matches_filter_self() {
        let mut state = setup();
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );
        assert!(matches_target_filter(
            &state,
            obj,
            &TargetFilter::SelfRef,
            obj
        ));
        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other".to_string(),
            Zone::Battlefield,
        );
        assert!(!matches_target_filter(
            &state,
            obj,
            &TargetFilter::SelfRef,
            other
        ));
    }

    // === Integration tests for engine trigger processing ===

    #[test]
    fn etb_trigger_places_ability_on_stack() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create a permanent with an ETB trigger on battlefield
        let trigger_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "ETB Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        // Simulate a ZoneChanged event (another creature enters)
        let events = vec![GameEvent::ZoneChanged {
            object_id: ObjectId(99),
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        // Trigger should be on the stack
        assert_eq!(state.stack.len(), 1);
        let entry = &state.stack[0];
        assert_eq!(entry.source_id, trigger_creature);
        assert_eq!(entry.controller, PlayerId(0));
        match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id, ability, ..
            } => {
                assert_eq!(*source_id, trigger_creature);
                assert_eq!(
                    crate::types::ability::effect_variant_name(&ability.effect),
                    "Draw"
                );
            }
            _ => panic!("Expected TriggeredAbility on stack"),
        }
    }

    #[test]
    fn multiple_triggers_from_same_event() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create two creatures with ETB triggers, different controllers
        let c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 ETB".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c1).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        let c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 ETB".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c2).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.controller = PlayerId(1);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![GameEvent::ZoneChanged {
            object_id: ObjectId(99),
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        assert_eq!(state.stack.len(), 2);
        // APNAP: AP (P0) on top, NAP (P1) on bottom
        assert_eq!(state.stack[state.stack.len() - 1].controller, PlayerId(0));
        assert_eq!(state.stack[0].controller, PlayerId(1));
    }

    #[test]
    fn trigger_with_condition_only_matches_when_met() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create a trigger that only fires for creature zone changes
        let trigger_src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Trigger Source".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_src).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ))
                    .valid_card(TargetFilter::Typed(TypedFilter::creature()))
                    .destination(Zone::Battlefield),
            );
        }

        // Create a non-creature that enters
        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        // Land enters -- should NOT trigger (valid_card = Creature)
        let events = vec![GameEvent::ZoneChanged {
            object_id: land,
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];
        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            0,
            "Land entering should not trigger creature-only ETB"
        );

        // Now a creature enters -- should trigger
        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let events = vec![GameEvent::ZoneChanged {
            object_id: creature,
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];
        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            1,
            "Creature entering should trigger creature ETB"
        );
    }

    #[test]
    fn prowess_triggers_on_noncreature_spell_cast() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create a creature with Prowess keyword on the battlefield
        let prowess_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monastery Swiftspear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&prowess_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.keywords.push(Keyword::Prowess);
        }

        // Create a noncreature spell object (Instant) on stack for the SpellCast event
        let spell = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        // Simulate SpellCast event by controller
        let events = vec![GameEvent::SpellCast {
            card_id: CardId(10),
            controller: PlayerId(0),
        }];

        process_triggers(&mut state, &events);

        // Prowess should have placed a triggered ability on the stack
        assert_eq!(
            state.stack.len(),
            1,
            "Prowess should trigger on noncreature spell"
        );
    }

    #[test]
    fn prowess_does_not_trigger_on_creature_spell() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        state.active_player = PlayerId(0);

        let prowess_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monastery Swiftspear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&prowess_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.keywords.push(Keyword::Prowess);
        }

        // Create a creature spell
        let creature_spell = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Bear Cub".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&creature_spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let events = vec![GameEvent::SpellCast {
            card_id: CardId(10),
            controller: PlayerId(0),
        }];

        process_triggers(&mut state, &events);

        // Prowess should NOT trigger on creature spells
        assert_eq!(
            state.stack.len(),
            0,
            "Prowess should not trigger on creature spell"
        );
    }

    #[test]
    fn prowess_does_not_trigger_on_opponent_spell() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        state.active_player = PlayerId(0);

        let prowess_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monastery Swiftspear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&prowess_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.keywords.push(Keyword::Prowess);
        }

        // Opponent casts a noncreature spell
        let spell = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let events = vec![GameEvent::SpellCast {
            card_id: CardId(10),
            controller: PlayerId(1),
        }];

        process_triggers(&mut state, &events);

        // Prowess should NOT trigger on opponent's spells
        assert_eq!(
            state.stack.len(),
            0,
            "Prowess should not trigger on opponent's spell"
        );
    }

    #[test]
    fn build_triggered_ability_from_typed_execute() {
        let trig_def = TriggerDefinition::new(TriggerMode::ChangesZone).execute(
            AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: GainLifePlayer::Controller,
                },
            )),
        );

        let ability = build_triggered_ability(&trig_def, ObjectId(1), PlayerId(0));
        assert_eq!(
            crate::types::ability::effect_variant_name(&ability.effect),
            "Draw"
        );
        assert!(ability.sub_ability.is_some());
        let sub = ability.sub_ability.unwrap();
        assert_eq!(
            crate::types::ability::effect_variant_name(&sub.effect),
            "GainLife"
        );
    }

    #[test]
    fn build_triggered_ability_no_execute() {
        let trig_def = make_trigger(TriggerMode::ChangesZone);
        let ability = build_triggered_ability(&trig_def, ObjectId(1), PlayerId(0));
        assert!(matches!(ability.effect, Effect::Unimplemented { .. }));
    }

    // === Triggered ability target selection tests ===

    #[test]
    fn trigger_target_multi_targets_sets_pending() {
        // Trigger with targeting + multiple legal targets -> sets pending_trigger
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create two opponent creatures as legal targets
        let target1 = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opp Creature 1".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&target1).unwrap().controller = PlayerId(1);

        let target2 = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Opp Creature 2".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&target2).unwrap().controller = PlayerId(1);

        // Create a creature with ETB exile trigger targeting a creature opponent controls
        let trigger_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(
                        AbilityDefinition::new(
                            AbilityKind::Database,
                            Effect::ChangeZone {
                                origin: Some(Zone::Battlefield),
                                destination: Zone::Exile,
                                target: TargetFilter::Typed(
                                    TypedFilter::creature().controller(ControllerRef::Opponent),
                                ),
                                owner_library: false,
                            },
                        )
                        .duration(crate::types::ability::Duration::UntilHostLeavesPlay),
                    )
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield),
            );
        }

        // Fire an ETB event for the trigger creature
        let events = vec![GameEvent::ZoneChanged {
            object_id: trigger_creature,
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        // Multiple legal targets -> should set pending_trigger, NOT push to stack
        assert!(
            state.pending_trigger.is_some(),
            "Should have pending trigger"
        );
        assert_eq!(state.stack.len(), 0, "Should NOT be on stack yet");
        let pending = state.pending_trigger.as_ref().unwrap();
        assert_eq!(pending.source_id, trigger_creature);
        assert_eq!(pending.controller, PlayerId(0));
    }

    #[test]
    fn trigger_target_single_target_auto_selects() {
        // Trigger with targeting + exactly 1 legal target -> auto-targets and pushes to stack
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create only ONE opponent creature as legal target
        let target1 = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opp Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&target1).unwrap().controller = PlayerId(1);

        // Create trigger creature
        let trigger_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(
                        AbilityDefinition::new(
                            AbilityKind::Database,
                            Effect::ChangeZone {
                                origin: Some(Zone::Battlefield),
                                destination: Zone::Exile,
                                target: TargetFilter::Typed(
                                    TypedFilter::creature().controller(ControllerRef::Opponent),
                                ),
                                owner_library: false,
                            },
                        )
                        .duration(crate::types::ability::Duration::UntilHostLeavesPlay),
                    )
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![GameEvent::ZoneChanged {
            object_id: trigger_creature,
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        // Single legal target -> auto-target and push to stack
        assert!(
            state.pending_trigger.is_none(),
            "Should NOT have pending trigger"
        );
        assert_eq!(state.stack.len(), 1, "Should be on stack");
        let entry = &state.stack[0];
        match &entry.kind {
            StackEntryKind::TriggeredAbility { ability, .. } => {
                assert_eq!(ability.targets.len(), 1);
                assert_eq!(
                    ability.targets[0],
                    crate::types::ability::TargetRef::Object(target1)
                );
            }
            _ => panic!("Expected TriggeredAbility on stack"),
        }
    }

    #[test]
    fn trigger_target_zero_targets_skips() {
        // Trigger with targeting + 0 legal targets -> skipped entirely
        let mut state = setup();
        state.active_player = PlayerId(0);

        // No opponent creatures on battlefield (no legal targets)

        let trigger_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::ChangeZone {
                            origin: Some(Zone::Battlefield),
                            destination: Zone::Exile,
                            target: TargetFilter::Typed(
                                TypedFilter::creature().controller(ControllerRef::Opponent),
                            ),
                            owner_library: false,
                        },
                    ))
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![GameEvent::ZoneChanged {
            object_id: trigger_creature,
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        // Zero legal targets -> trigger is skipped
        assert!(
            state.pending_trigger.is_none(),
            "Should NOT have pending trigger"
        );
        assert_eq!(state.stack.len(), 0, "Should NOT be on stack");
    }

    #[test]
    fn banishing_light_trigger_skips_without_opponent_nonlands() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Exile,
                            target: TargetFilter::Typed(
                                TypedFilter::permanent()
                                    .controller(ControllerRef::Opponent)
                                    .properties(vec![FilterProp::NonType {
                                        value: "land".to_string(),
                                    }]),
                            ),
                            owner_library: false,
                        },
                    ))
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield),
            );
        }

        let opponent_land = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opponent_land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let events = vec![GameEvent::ZoneChanged {
            object_id: source,
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        assert!(
            state.pending_trigger.is_none(),
            "Should NOT present trigger target selection"
        );
        assert_eq!(state.stack.len(), 0, "Should skip the ETB trigger");
    }

    #[test]
    fn trigger_no_execute_goes_on_stack_without_targeting() {
        // Trigger with no execute (Effect::Unimplemented) goes on stack without targeting attempt
        let mut state = setup();
        state.active_player = PlayerId(0);

        let trigger_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Simple Trigger".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone).destination(Zone::Battlefield),
            );
        }

        let events = vec![GameEvent::ZoneChanged {
            object_id: ObjectId(99),
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        // Should go on stack as before (Unimplemented ability), no targeting
        assert_eq!(state.stack.len(), 1);
        assert!(state.pending_trigger.is_none());
    }

    #[test]
    fn trigger_no_targeting_effect_goes_on_stack() {
        // Trigger with execute but no targeting (e.g., Draw) goes on stack immediately
        let mut state = setup();
        state.active_player = PlayerId(0);

        let trigger_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Draw Trigger".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![GameEvent::ZoneChanged {
            object_id: ObjectId(99),
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        // No targeting needed -> should be on stack immediately
        assert_eq!(state.stack.len(), 1);
        assert!(state.pending_trigger.is_none());
    }

    #[test]
    fn graveyard_trigger_fires_on_matching_event() {
        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forsaken Miner".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            let mut trigger = make_trigger(TriggerMode::CommitCrime);
            trigger.trigger_zones = vec![Zone::Graveyard];
            trigger.execute = Some(Box::new(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                },
            )));
            obj.trigger_definitions.push(trigger);
        }

        let events = vec![GameEvent::CrimeCommitted {
            player_id: PlayerId(0),
        }];

        process_triggers(&mut state, &events);

        // Trigger should be on the stack
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn graveyard_trigger_ignored_without_trigger_zone() {
        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "No Graveyard Trigger".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            // trigger_zones is empty — should NOT fire from graveyard
            let trigger = make_trigger(TriggerMode::CommitCrime);
            obj.trigger_definitions.push(trigger);
        }

        let events = vec![GameEvent::CrimeCommitted {
            player_id: PlayerId(0),
        }];

        process_triggers(&mut state, &events);

        // Should NOT be on the stack
        assert_eq!(state.stack.len(), 0);
    }

    #[test]
    fn deep_cavern_bat_etb_trigger_fires() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create Deep-Cavern Bat on battlefield with RevealHand ETB trigger
        let bat = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Deep-Cavern Bat".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bat).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(
                        AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::RevealHand {
                                target: TargetFilter::Typed(
                                    TypedFilter::default().controller(ControllerRef::Opponent),
                                ),
                                card_filter: TargetFilter::Typed(
                                    TypedFilter::permanent().properties(vec![
                                        FilterProp::NonType {
                                            value: "Land".to_string(),
                                        },
                                    ]),
                                ),
                            },
                        )
                        .sub_ability(
                            AbilityDefinition::new(
                                AbilityKind::Spell,
                                Effect::ChangeZone {
                                    origin: None,
                                    destination: Zone::Exile,
                                    target: TargetFilter::Any,
                                    owner_library: false,
                                },
                            )
                            .duration(crate::types::ability::Duration::UntilHostLeavesPlay),
                        ),
                    )
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield)
                    .trigger_zones(vec![Zone::Battlefield]),
            );
        }

        // Simulate bat entering battlefield
        let events = vec![GameEvent::ZoneChanged {
            object_id: bat,
            from: Zone::Stack,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        // In 2-player game, one opponent → auto-target → push to stack
        assert!(
            state.pending_trigger.is_none(),
            "Should auto-target single opponent, not set pending"
        );
        assert_eq!(state.stack.len(), 1, "Trigger should be on the stack");

        let entry = &state.stack[0];
        assert_eq!(entry.source_id, bat);
        match &entry.kind {
            StackEntryKind::TriggeredAbility { ability, .. } => {
                assert_eq!(ability.targets.len(), 1);
                assert_eq!(
                    ability.targets[0],
                    crate::types::ability::TargetRef::Player(PlayerId(1))
                );
                assert!(matches!(ability.effect, Effect::RevealHand { .. }));
            }
            _ => panic!("Expected TriggeredAbility on stack"),
        }
    }
}
