use std::collections::HashMap;

use crate::types::ability::{
    AbilityDefinition, Effect, EffectKind, ModalChoice, ResolvedAbility, TargetFilter, TargetRef,
    TriggerCondition, TriggerDefinition, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind, TargetSelectionConstraint};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::{Player, PlayerId};
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::ability_utils::build_resolved_from_def;
use super::stack;

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
        | Effect::RevealHand { target, .. } => {
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
// Trigger Registry
// ---------------------------------------------------------------------------

/// Build a registry mapping every TriggerMode to its matcher function.
pub fn build_trigger_registry() -> HashMap<TriggerMode, TriggerMatcher> {
    let mut r: HashMap<TriggerMode, TriggerMatcher> = HashMap::new();

    // Core matchers with real logic
    r.insert(TriggerMode::ChangesZone, match_changes_zone);
    r.insert(TriggerMode::ChangesZoneAll, match_changes_zone_all);
    r.insert(TriggerMode::DamageDone, match_damage_done);
    r.insert(TriggerMode::DamageDoneOnce, match_damage_done);
    r.insert(TriggerMode::DamageAll, match_damage_done);
    r.insert(TriggerMode::DamageDealtOnce, match_damage_done);
    r.insert(
        TriggerMode::DamageDoneOnceByController,
        match_damage_done_once_by_controller,
    );
    r.insert(TriggerMode::SpellCast, match_spell_cast);
    r.insert(TriggerMode::SpellCastOrCopy, match_spell_cast);
    r.insert(TriggerMode::Attacks, match_attacks);
    r.insert(TriggerMode::AttackersDeclared, match_attackers_declared);
    r.insert(
        TriggerMode::AttackersDeclaredOneTarget,
        match_attackers_declared,
    );
    r.insert(TriggerMode::Blocks, match_blocks);
    r.insert(TriggerMode::BlockersDeclared, match_blockers_declared);
    r.insert(TriggerMode::Countered, match_countered);
    r.insert(TriggerMode::CounterAdded, match_counter_added);
    r.insert(TriggerMode::CounterAddedOnce, match_counter_added);
    r.insert(TriggerMode::CounterAddedAll, match_counter_added);
    r.insert(TriggerMode::CounterRemoved, match_counter_removed);
    r.insert(TriggerMode::CounterRemovedOnce, match_counter_removed);
    r.insert(TriggerMode::Taps, match_taps);
    r.insert(TriggerMode::TapAll, match_taps);
    r.insert(TriggerMode::Untaps, match_untaps);
    r.insert(TriggerMode::UntapAll, match_untaps);
    r.insert(TriggerMode::LifeGained, match_life_gained);
    r.insert(TriggerMode::LifeLost, match_life_lost);
    r.insert(TriggerMode::LifeLostAll, match_life_lost);
    r.insert(TriggerMode::Drawn, match_drawn);
    r.insert(TriggerMode::Discarded, match_discarded);
    r.insert(TriggerMode::DiscardedAll, match_discarded);
    r.insert(TriggerMode::Sacrificed, match_sacrificed);
    r.insert(TriggerMode::SacrificedOnce, match_sacrificed);
    r.insert(TriggerMode::Destroyed, match_destroyed);
    r.insert(TriggerMode::TokenCreated, match_token_created);
    r.insert(TriggerMode::TokenCreatedOnce, match_token_created);
    r.insert(TriggerMode::TurnBegin, match_turn_begin);
    r.insert(TriggerMode::Phase, match_phase);
    r.insert(TriggerMode::BecomesTarget, match_becomes_target);
    r.insert(TriggerMode::BecomesTargetOnce, match_becomes_target);
    r.insert(TriggerMode::LandPlayed, match_land_played);
    r.insert(TriggerMode::SpellCopy, match_spell_cast);
    r.insert(TriggerMode::ManaAdded, match_mana_added);

    // Zone-based: leaves the battlefield
    r.insert(TriggerMode::LeavesBattlefield, match_leaves_battlefield);

    // Combat: becomes blocked, you attack
    r.insert(TriggerMode::BecomesBlocked, match_becomes_blocked);
    r.insert(TriggerMode::YouAttack, match_you_attack);

    // Damage: is dealt damage
    r.insert(TriggerMode::DamageReceived, match_damage_received);

    // Promoted trigger matchers -- Standard-relevant combat triggers
    r.insert(TriggerMode::AttackerBlocked, match_attacker_blocked);
    r.insert(TriggerMode::AttackerBlockedOnce, match_attacker_blocked);
    r.insert(
        TriggerMode::AttackerBlockedByCreature,
        match_attacker_blocked,
    );
    r.insert(TriggerMode::AttackerUnblocked, match_attacker_unblocked);
    r.insert(TriggerMode::AttackerUnblockedOnce, match_attacker_unblocked);

    // Promoted trigger matchers -- zone-based triggers
    r.insert(TriggerMode::Milled, match_milled);
    r.insert(TriggerMode::MilledOnce, match_milled);
    r.insert(TriggerMode::MilledAll, match_milled);
    r.insert(TriggerMode::Exiled, match_exiled);

    // Promoted trigger matchers -- attachment triggers
    r.insert(TriggerMode::Attached, match_attached);
    r.insert(TriggerMode::Unattach, match_unattach);

    // Promoted trigger matchers -- other Standard-relevant triggers
    r.insert(TriggerMode::Cycled, match_cycled);
    r.insert(TriggerMode::Shuffled, match_shuffled);
    r.insert(TriggerMode::Revealed, match_revealed);
    r.insert(TriggerMode::TapsForMana, match_taps_for_mana);
    r.insert(TriggerMode::ChangesController, match_changes_controller);
    r.insert(TriggerMode::Transformed, match_transformed);
    r.insert(TriggerMode::Fight, match_fight);
    r.insert(TriggerMode::FightOnce, match_fight);
    r.insert(TriggerMode::Immediate, match_always);
    r.insert(TriggerMode::Always, match_always);
    r.insert(TriggerMode::Explored, match_explored);

    // Promoted trigger matchers -- face-down mechanics
    r.insert(TriggerMode::TurnFaceUp, match_turn_face_up);

    // Promoted trigger matchers -- day/night
    r.insert(TriggerMode::DayTimeChanges, match_day_time_changes);

    // Promoted trigger matchers -- crime mechanic (OTJ+)
    r.insert(TriggerMode::CommitCrime, match_commit_crime);

    // Promoted trigger matchers -- Case enchantments (MKM+)
    r.insert(TriggerMode::CaseSolved, match_case_solved);

    // Promoted trigger matchers -- Class enchantments (AFR+)
    r.insert(TriggerMode::ClassLevelGained, match_class_level_gained);

    // Remaining trigger modes: recognized but not yet matched against events.
    let unimplemented_modes = [
        TriggerMode::DamagePreventedOnce,
        TriggerMode::ExcessDamage,
        TriggerMode::ExcessDamageAll,
        TriggerMode::AbilityCast,
        TriggerMode::AbilityResolves,
        TriggerMode::AbilityTriggered,
        TriggerMode::SpellAbilityCast,
        TriggerMode::SpellAbilityCopy,
        TriggerMode::CounterPlayerAddedAll,
        TriggerMode::CounterTypeAddedAll,
        TriggerMode::PayLife,
        TriggerMode::PayCumulativeUpkeep,
        TriggerMode::PayEcho,
        TriggerMode::PhaseIn,
        TriggerMode::PhaseOut,
        TriggerMode::PhaseOutAll,
        TriggerMode::NewGame,
        TriggerMode::BecomeMonarch,
        TriggerMode::TakesInitiative,
        TriggerMode::LosesGame,
        TriggerMode::Championed,
        TriggerMode::Exerted,
        TriggerMode::Crewed,
        TriggerMode::Saddled,
        TriggerMode::Evolved,
        TriggerMode::Exploited,
        TriggerMode::Enlisted,
        TriggerMode::ManaExpend,
        TriggerMode::Adapt,
        TriggerMode::Foretell,
        TriggerMode::Investigated,
        TriggerMode::DungeonCompleted,
        TriggerMode::RoomEntered,
        TriggerMode::PlanarDice,
        TriggerMode::PlaneswalkedFrom,
        TriggerMode::PlaneswalkedTo,
        TriggerMode::ChaosEnsues,
        TriggerMode::RolledDie,
        TriggerMode::RolledDieOnce,
        TriggerMode::FlippedCoin,
        TriggerMode::Clashed,
        TriggerMode::Copied,
        TriggerMode::ConjureAll,
        TriggerMode::Vote,
        TriggerMode::BecomeRenowned,
        TriggerMode::BecomeMonstrous,
        TriggerMode::Proliferate,
        TriggerMode::RingTemptsYou,
        TriggerMode::Surveil,
        TriggerMode::Scry,
        TriggerMode::Abandoned,
        TriggerMode::ClaimPrize,
        TriggerMode::CollectEvidence,
        TriggerMode::CrankContraption,
        TriggerMode::Devoured,
        TriggerMode::Discover,
        TriggerMode::Forage,
        TriggerMode::FullyUnlock,
        TriggerMode::GiveGift,
        TriggerMode::ManifestDread,
        TriggerMode::Mentored,
        TriggerMode::Mutates,
        TriggerMode::SearchedLibrary,
        TriggerMode::SeekAll,
        TriggerMode::SetInMotion,
        TriggerMode::Specializes,
        TriggerMode::Stationed,
        TriggerMode::Trains,
        TriggerMode::UnlockDoor,
        TriggerMode::VisitAttraction,
        TriggerMode::BecomesCrewed,
        TriggerMode::BecomesPlotted,
        TriggerMode::BecomesSaddled,
        TriggerMode::Airbend,
        TriggerMode::Earthbend,
        TriggerMode::Firebend,
        TriggerMode::Waterbend,
        TriggerMode::ElementalBend,
    ];

    for mode in unimplemented_modes {
        r.insert(mode, match_unimplemented);
    }

    r
}

// ---------------------------------------------------------------------------
// Helper: check ValidCard filter using either typed TargetFilter or string filter
// ---------------------------------------------------------------------------

/// Check if the trigger's valid_card filter matches the given object.
/// Uses the TargetFilter typed field if set; otherwise no filter (passes).
fn valid_card_matches(
    trigger: &TriggerDefinition,
    state: &GameState,
    object_id: ObjectId,
    source_id: ObjectId,
) -> bool {
    match &trigger.valid_card {
        None => true,
        Some(filter) => target_filter_matches_object(state, object_id, filter, source_id),
    }
}

/// Check if the trigger's valid_source filter matches the given object.
fn valid_source_matches(
    trigger: &TriggerDefinition,
    state: &GameState,
    object_id: ObjectId,
    source_id: ObjectId,
) -> bool {
    match &trigger.valid_source {
        None => true,
        Some(filter) => target_filter_matches_object(state, object_id, filter, source_id),
    }
}

fn valid_player_matches(
    trigger: &TriggerDefinition,
    state: &GameState,
    player_id: PlayerId,
    source_id: ObjectId,
) -> bool {
    let Some(filter) = &trigger.valid_target else {
        return true;
    };

    let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
    match filter {
        TargetFilter::Player => true,
        TargetFilter::Controller => trigger_controller == Some(player_id),
        TargetFilter::Typed(TypedFilter {
            controller: Some(crate::types::ability::ControllerRef::You),
            ..
        }) => trigger_controller == Some(player_id),
        TargetFilter::Typed(TypedFilter {
            controller: Some(crate::types::ability::ControllerRef::Opponent),
            ..
        }) => trigger_controller.is_some_and(|controller| controller != player_id),
        _ => true,
    }
}

/// Basic runtime matching of a TargetFilter against a game object.
/// Handles the common filter patterns used in triggers.
fn target_filter_matches_object(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> bool {
    use crate::types::ability::{ControllerRef, FilterProp, TypeFilter};

    let obj = match state.objects.get(&object_id) {
        Some(o) => o,
        None => return false,
    };

    match filter {
        TargetFilter::None => false,
        TargetFilter::Any => true,
        TargetFilter::Player => false, // Players are not objects
        TargetFilter::Controller => false,
        TargetFilter::SelfRef => object_id == source_id,
        TargetFilter::Typed(TypedFilter {
            card_type,
            subtype,
            controller,
            properties,
        }) => {
            // Check card type
            if let Some(type_filter) = card_type {
                let type_match = match type_filter {
                    TypeFilter::Creature => obj.card_types.core_types.contains(&CoreType::Creature),
                    TypeFilter::Land => obj.card_types.core_types.contains(&CoreType::Land),
                    TypeFilter::Artifact => obj.card_types.core_types.contains(&CoreType::Artifact),
                    TypeFilter::Enchantment => {
                        obj.card_types.core_types.contains(&CoreType::Enchantment)
                    }
                    TypeFilter::Instant => obj.card_types.core_types.contains(&CoreType::Instant),
                    TypeFilter::Sorcery => obj.card_types.core_types.contains(&CoreType::Sorcery),
                    TypeFilter::Planeswalker => {
                        obj.card_types.core_types.contains(&CoreType::Planeswalker)
                    }
                    TypeFilter::Permanent => obj.card_types.core_types.iter().any(|ct| {
                        matches!(
                            ct,
                            CoreType::Creature
                                | CoreType::Artifact
                                | CoreType::Enchantment
                                | CoreType::Planeswalker
                                | CoreType::Land
                        )
                    }),
                    TypeFilter::Card | TypeFilter::Any => true,
                };
                if !type_match {
                    return false;
                }
            }
            // Check subtype
            if let Some(sub) = subtype {
                if !obj.card_types.subtypes.iter().any(|s| s == sub) {
                    return false;
                }
            }
            // Check controller
            if let Some(ctrl_ref) = controller {
                let source_controller = state.objects.get(&source_id).map(|o| o.controller);
                match ctrl_ref {
                    ControllerRef::You => {
                        if source_controller != Some(obj.controller) {
                            return false;
                        }
                    }
                    ControllerRef::Opponent => {
                        if source_controller == Some(obj.controller) {
                            return false;
                        }
                    }
                }
            }
            // Check properties
            for prop in properties {
                match prop {
                    FilterProp::Token => {
                        // Token check not yet tracked on GameObject
                    }
                    FilterProp::Attacking => {
                        // Would need combat state check
                    }
                    FilterProp::Tapped if !obj.tapped => {
                        return false;
                    }
                    FilterProp::NonType { value } => {
                        let excluded_type = match value.as_str() {
                            "Creature" => Some(CoreType::Creature),
                            "Land" => Some(CoreType::Land),
                            "Artifact" => Some(CoreType::Artifact),
                            "Enchantment" => Some(CoreType::Enchantment),
                            _ => None,
                        };
                        if let Some(ct) = excluded_type {
                            if obj.card_types.core_types.contains(&ct) {
                                return false;
                            }
                        } else {
                            // Not a core type — check subtypes (e.g., "non-Human")
                            if obj
                                .card_types
                                .subtypes
                                .iter()
                                .any(|s| s.eq_ignore_ascii_case(value))
                            {
                                return false;
                            }
                        }
                    }
                    FilterProp::WithKeyword { value }
                        if !obj.keywords.iter().any(|k| format!("{:?}", k) == *value) =>
                    {
                        return false;
                    }
                    FilterProp::Another if object_id == source_id => {
                        return false;
                    }
                    FilterProp::HasColor { color } => {
                        use crate::types::mana::ManaColor;
                        let mana_color = match color.as_str() {
                            "White" => Some(ManaColor::White),
                            "Blue" => Some(ManaColor::Blue),
                            "Black" => Some(ManaColor::Black),
                            "Red" => Some(ManaColor::Red),
                            "Green" => Some(ManaColor::Green),
                            _ => None,
                        };
                        if let Some(mc) = mana_color {
                            if !obj.color.contains(&mc) {
                                return false;
                            }
                        }
                    }
                    FilterProp::PowerLE { value } if obj.power.unwrap_or(0) > *value => {
                        return false;
                    }
                    FilterProp::PowerGE { value } if obj.power.unwrap_or(0) < *value => {
                        return false;
                    }
                    FilterProp::Multicolored if obj.color.len() <= 1 => {
                        return false;
                    }
                    FilterProp::IsChosenCreatureType => {
                        let chosen = state
                            .objects
                            .get(&source_id)
                            .and_then(|src| src.chosen_creature_type());
                        match chosen {
                            Some(ct) => {
                                if !obj
                                    .card_types
                                    .subtypes
                                    .iter()
                                    .any(|s| s.eq_ignore_ascii_case(ct))
                                {
                                    return false;
                                }
                            }
                            None => return false,
                        }
                    }
                    _ => {
                        // Other filter props: pass through for now
                    }
                }
            }
            true
        }
        TargetFilter::Not { filter: inner } => {
            !target_filter_matches_object(state, object_id, inner, source_id)
        }
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|f| target_filter_matches_object(state, object_id, f, source_id)),
        TargetFilter::And { filters } => filters
            .iter()
            .all(|f| target_filter_matches_object(state, object_id, f, source_id)),
        // StackAbility/StackSpell targeting is handled directly at call sites, not via object matching
        TargetFilter::StackAbility | TargetFilter::StackSpell => false,
        TargetFilter::SpecificObject { id: target_id } => object_id == *target_id,
        TargetFilter::AttachedTo => {
            // The trigger source must have attached_to pointing at this object.
            state
                .objects
                .get(&source_id)
                .and_then(|src| src.attached_to)
                .is_some_and(|attached| attached == object_id)
        }
        TargetFilter::LastCreated => state.last_created_token_ids.contains(&object_id),
        // CR 603.7: Match objects in a tracked set from the originating effect.
        TargetFilter::TrackedSet { id } => state
            .tracked_object_sets
            .get(id)
            .is_some_and(|set| set.contains(&object_id)),
        // CR 610.3: Delegate to shared filter logic for exile-until-leaves links.
        TargetFilter::ExiledBySource => {
            super::filter::matches_target_filter(state, object_id, filter, source_id)
        }
        // CR 603.7c / CR 506.3d: Event-context references resolve to players, not objects.
        TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::DefendingPlayer => false,
        // ParentTarget resolves to parent ability's targets at resolution time.
        TargetFilter::ParentTarget => false,
    }
}

// ---------------------------------------------------------------------------
// Core Trigger Matchers (~20 with real logic)
// ---------------------------------------------------------------------------

fn match_changes_zone(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::ZoneChanged {
        object_id,
        from,
        to,
    } = event
    {
        // Check origin zone using typed field
        if let Some(origin) = &trigger.origin {
            if origin != from {
                return false;
            }
        }
        // Check destination zone using typed field
        if let Some(destination) = &trigger.destination {
            if destination != to {
                return false;
            }
        }
        // Check valid_card filter
        if !valid_card_matches(trigger, state, *object_id, source_id) {
            return false;
        }
        true
    } else {
        false
    }
}

fn match_changes_zone_all(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    // ChangesZoneAll triggers for any card changing zones, same logic
    match_changes_zone(event, trigger, source_id, state)
}

fn match_damage_done(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::DamageDealt {
        source_id: dmg_source,
        target: _,
        amount: _,
        is_combat,
    } = event
    {
        // Check if trigger requires damage from a specific source
        if !valid_source_matches(trigger, state, *dmg_source, source_id) {
            return false;
        }
        // Check combat_damage flag
        if trigger.combat_damage && !is_combat {
            return false;
        }
        true
    } else {
        false
    }
}

fn match_damage_done_once_by_controller(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::CombatDamageDealtToPlayer {
        player_id,
        source_ids,
    } = event
    else {
        return false;
    };

    if let Some(ref vt) = trigger.valid_target {
        let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
        match vt {
            TargetFilter::Controller if trigger_controller != Some(*player_id) => {
                return false;
            }
            TargetFilter::Typed(TypedFilter {
                controller: Some(crate::types::ability::ControllerRef::You),
                ..
            }) if trigger_controller != Some(*player_id) => {
                return false;
            }
            TargetFilter::Typed(TypedFilter {
                controller: Some(crate::types::ability::ControllerRef::Opponent),
                ..
            }) if trigger_controller == Some(*player_id) => {
                return false;
            }
            TargetFilter::Player => {}
            _ => {}
        }
    }

    if let Some(filter) = &trigger.valid_source {
        return source_ids
            .iter()
            .any(|source| target_filter_matches_object(state, *source, filter, source_id));
    }

    source_ids.contains(&source_id)
}

fn match_spell_cast(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::SpellCast {
        card_id,
        controller,
    } = event
    {
        // Check valid_card filter on the cast spell
        if let Some(ref _filter) = trigger.valid_card {
            // Find object by card_id
            let obj_id = state
                .objects
                .iter()
                .find(|(_, obj)| obj.card_id == *card_id)
                .map(|(id, _)| *id);
            if let Some(oid) = obj_id {
                if !valid_card_matches(trigger, state, oid, source_id) {
                    return false;
                }
            }
        }
        valid_player_matches(trigger, state, *controller, source_id)
    } else {
        false
    }
}

fn match_attacks(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::AttackersDeclared { attacker_ids, .. } = event {
        // "Attacks" triggers for the specific source creature attacking
        if trigger.valid_card.is_some() {
            attacker_ids
                .iter()
                .any(|id| valid_card_matches(trigger, state, *id, source_id))
        } else {
            // No filter: trigger if source itself is among attackers
            attacker_ids.contains(&source_id)
        }
    } else {
        false
    }
}

fn match_attackers_declared(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::AttackersDeclared { .. })
}

fn match_blocks(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::BlockersDeclared { assignments } = event {
        if trigger.valid_card.is_some() {
            // valid_card filter: check if any blocker in the assignments matches.
            // For self-reference ("Whenever ~ blocks"), this fires when source_id is a blocker.
            // For typed filters ("Whenever a creature you control blocks"), check each blocker.
            assignments
                .iter()
                .any(|(blocker, _)| valid_card_matches(trigger, state, *blocker, source_id))
        } else {
            // No filter: fire if source itself is among blockers
            assignments.iter().any(|(blocker, _)| *blocker == source_id)
        }
    } else {
        false
    }
}

fn match_blockers_declared(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::BlockersDeclared { .. })
}

fn match_countered(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::SpellCountered { object_id, .. } = event {
        valid_card_matches(trigger, state, *object_id, source_id)
    } else {
        false
    }
}

fn match_counter_added(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CounterAdded {
        object_id,
        counter_type,
        count,
    } = event
    {
        if !valid_card_matches(trigger, state, *object_id, source_id) {
            return false;
        }
        // CR 714.2a: Apply counter filter (type + optional threshold crossing).
        if let Some(ref filter) = trigger.counter_filter {
            if filter.counter_type != *counter_type {
                return false;
            }
            if let Some(threshold) = filter.threshold {
                let current = state
                    .objects
                    .get(object_id)
                    .and_then(|obj| obj.counters.get(&filter.counter_type).copied())
                    .unwrap_or(0);
                let previous = current.saturating_sub(*count);
                // Fire only when the threshold is crossed: previous < threshold <= current
                if !(previous < threshold && threshold <= current) {
                    return false;
                }
            }
        }
        true
    } else {
        false
    }
}

fn match_counter_removed(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CounterRemoved {
        object_id,
        counter_type: _,
        ..
    } = event
    {
        if !valid_card_matches(trigger, state, *object_id, source_id) {
            return false;
        }
        true
    } else {
        false
    }
}

fn match_taps(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::PermanentTapped { object_id } = event {
        if trigger.valid_card.is_some() {
            valid_card_matches(trigger, state, *object_id, source_id)
        } else {
            *object_id == source_id
        }
    } else {
        false
    }
}

fn match_untaps(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::PermanentUntapped { object_id } = event {
        if trigger.valid_card.is_some() {
            valid_card_matches(trigger, state, *object_id, source_id)
        } else {
            *object_id == source_id
        }
    } else {
        false
    }
}

fn match_life_gained(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::LifeChanged { player_id, amount } = event {
        if *amount <= 0 {
            return false;
        }
        valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

fn match_life_lost(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::LifeChanged { player_id, amount } = event {
        if *amount >= 0 {
            return false;
        }
        valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

fn match_drawn(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CardDrawn { player_id, .. } = event {
        valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

fn match_discarded(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::Discarded {
        player_id: _,
        object_id,
    } = event
    {
        if !valid_card_matches(trigger, state, *object_id, source_id) {
            return false;
        }
        true
    } else {
        false
    }
}

fn match_sacrificed(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::PermanentSacrificed { object_id, .. } = event {
        valid_card_matches(trigger, state, *object_id, source_id)
    } else {
        false
    }
}

fn match_destroyed(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CreatureDestroyed { object_id } = event {
        valid_card_matches(trigger, state, *object_id, source_id)
    } else {
        false
    }
}

fn match_token_created(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::TokenCreated { .. })
}

fn match_turn_begin(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::TurnStarted { .. })
}

fn match_phase(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    if let GameEvent::PhaseChanged { phase } = event {
        if let Some(ref trigger_phase) = trigger.phase {
            phase == trigger_phase
        } else {
            true
        }
    } else {
        false
    }
}

// CR 114.1a / CR 603.4: Match when the trigger's source becomes the target of a spell or ability.
fn match_becomes_target(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::BecomesTarget {
        object_id,
        source_id: targeting_spell_id,
    } = event
    else {
        return false;
    };

    // CR 114.1a: Check source filter — "of a spell" restricts to StackEntryKind::Spell
    if let Some(TargetFilter::StackSpell) = &trigger.valid_source {
        let is_spell = state
            .stack
            .iter()
            .any(|e| e.id == *targeting_spell_id && matches!(e.kind, StackEntryKind::Spell { .. }));
        if !is_spell {
            return false;
        }
    }

    // Check if the targeted object matches the trigger's valid_card filter
    if trigger.valid_card.is_some() {
        valid_card_matches(trigger, state, *object_id, source_id)
    } else {
        *object_id == source_id
    }
}

/// Match CommitCrime triggers: fires when the trigger's controller commits a crime.
fn match_commit_crime(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CrimeCommitted { player_id } = event {
        // Fire when the crime was committed by the trigger source's controller
        state
            .objects
            .get(&source_id)
            .map(|obj| obj.controller == *player_id)
            .unwrap_or(false)
    } else {
        false
    }
}

/// CR 719.2: Match CaseSolved events for the trigger's source object.
fn match_case_solved(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::CaseSolved { object_id } if *object_id == source_id)
}

/// CR 716.5: "When this Class becomes level N" triggers.
fn match_class_level_gained(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::ClassLevelGained { object_id, .. } if *object_id == source_id)
}

fn match_land_played(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::LandPlayed { object_id, .. } = event {
        valid_card_matches(trigger, state, *object_id, source_id)
    } else {
        false
    }
}

fn match_mana_added(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::ManaAdded { .. })
}

// ---------------------------------------------------------------------------
// Promoted Trigger Matchers
// ---------------------------------------------------------------------------

/// AttackerBlocked: fires when the source creature is among blocked attackers.
fn match_attacker_blocked(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    if let GameEvent::BlockersDeclared { assignments } = event {
        // Check if source is among the attackers that got blocked
        assignments
            .iter()
            .any(|(_, attacker)| *attacker == source_id)
    } else {
        false
    }
}

/// AttackerUnblocked: fires when source attacked but was not assigned any blockers.
fn match_attacker_unblocked(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::BlockersDeclared { assignments } = event {
        // Source must be an attacker in the current combat
        let is_attacker = state
            .combat
            .as_ref()
            .map(|c| c.attackers.iter().any(|a| a.object_id == source_id))
            .unwrap_or(false);
        if !is_attacker {
            return false;
        }
        // Source must not be among the blocked attackers
        !assignments
            .iter()
            .any(|(_, attacker)| *attacker == source_id)
    } else {
        false
    }
}

/// Milled: fires when a card moves from Library to Graveyard.
fn match_milled(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::ZoneChanged {
        object_id,
        from,
        to,
        ..
    } = event
    {
        if *from != Zone::Library || *to != Zone::Graveyard {
            return false;
        }
        if !valid_card_matches(trigger, state, *object_id, source_id) {
            return false;
        }
        true
    } else {
        false
    }
}

/// Exiled: fires when a card moves to Exile zone.
fn match_exiled(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::ZoneChanged { object_id, to, .. } = event {
        if *to != Zone::Exile {
            return false;
        }
        if !valid_card_matches(trigger, state, *object_id, source_id) {
            return false;
        }
        true
    } else {
        false
    }
}

/// Attached: fires when source becomes attached to a permanent.
fn match_attached(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match event {
        GameEvent::EffectResolved {
            kind: EffectKind::Attach | EffectKind::AttachAll,
            ..
        } => state
            .objects
            .get(&source_id)
            .map(|obj| obj.attached_to.is_some())
            .unwrap_or(false),
        _ => false,
    }
}

/// Unattach: fires when attachment is removed from a permanent.
fn match_unattach(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match event {
        GameEvent::ZoneChanged {
            object_id, from, ..
        } if *from == Zone::Battlefield => {
            // Check if source was attached to the object that left
            state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.attached_to)
                .map(|attached| attached == *object_id)
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// Cycled: fires when a player cycles a card.
fn match_cycled(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::Cycled { object_id, .. } = event {
        valid_card_matches(trigger, state, *object_id, source_id)
    } else {
        false
    }
}

/// Shuffled: fires when a library is shuffled.
fn match_shuffled(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Shuffle,
            ..
        }
    )
}

/// Revealed: fires when a card is revealed.
fn match_revealed(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            ..
        }
    )
}

/// TapsForMana: fires when source taps and produces mana.
fn match_taps_for_mana(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::ManaAdded {
        player_id,
        source_id: mana_source,
        ..
    } = event
    {
        if trigger.valid_card.is_some() {
            if !valid_card_matches(trigger, state, *mana_source, source_id) {
                return false;
            }
        } else if *mana_source != source_id {
            return false;
        }

        valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

/// ChangesController: fires when an object changes controller.
fn match_changes_controller(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::GainControl,
            ..
        }
    )
}

/// Transformed: fires when an object transforms.
fn match_transformed(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Transform,
            ..
        }
    )
}

/// Fight: fires when creatures fight.
fn match_fight(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Fight,
            ..
        }
    )
}

/// Always/Immediate: matches any event.
fn match_always(
    _event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    true
}

/// Explored: fires when a creature explores.
fn match_explored(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Explore,
            ..
        }
    )
}

/// TurnFaceUp: fires when a face-down creature is turned face up.
fn match_turn_face_up(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::TurnFaceUp,
            ..
        }
    )
}

/// DayTimeChanges: fires when day/night changes.
fn match_day_time_changes(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::DayTimeChange,
            ..
        }
    )
}

/// LeavesBattlefield: fires when the source (or filtered object) leaves the battlefield
/// to any zone. Uses ZoneChanged event with origin = Battlefield.
fn match_leaves_battlefield(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::ZoneChanged {
        object_id,
        from,
        to: _,
    } = event
    {
        if *from != Zone::Battlefield {
            return false;
        }
        valid_card_matches(trigger, state, *object_id, source_id)
    } else {
        false
    }
}

/// BecomesBlocked: fires when the source creature is assigned at least one blocker.
/// Reuses BlockersDeclared event — the attacker "becomes blocked" when blockers are declared.
fn match_becomes_blocked(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::BlockersDeclared { assignments } = event {
        if trigger.valid_card.is_some() {
            // Filter: check if any blocked attacker matches the valid_card filter
            assignments
                .iter()
                .any(|(_, attacker)| valid_card_matches(trigger, state, *attacker, source_id))
        } else {
            // Default: source itself must be among blocked attackers
            assignments
                .iter()
                .any(|(_, attacker)| *attacker == source_id)
        }
    } else {
        false
    }
}

/// DamageReceived: fires when the source creature is dealt damage.
/// Uses DamageDealt event but checks the *target* (not source) against the trigger source.
fn match_damage_received(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    if let GameEvent::DamageDealt {
        target, is_combat, ..
    } = event
    {
        if trigger.combat_damage && !is_combat {
            return false;
        }
        match target {
            TargetRef::Object(target_id) => {
                if trigger.valid_card.is_some() {
                    // Would need valid_card_matches on the target — for now,
                    // self-reference is the dominant pattern ("Whenever ~ is dealt damage")
                    *target_id == source_id
                } else {
                    *target_id == source_id
                }
            }
            TargetRef::Player(_) => false,
        }
    } else {
        false
    }
}

/// YouAttack: fires once when the trigger source's controller declares attackers.
/// Player-centric — fires regardless of which creatures attack.
fn match_you_attack(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::AttackersDeclared {
        attacker_ids,
        defending_player: _,
    } = event
    {
        if attacker_ids.is_empty() {
            return false;
        }
        // Fire if any attacker is controlled by the source's controller
        let source_controller = state.objects.get(&source_id).map(|o| o.controller);
        attacker_ids.iter().any(|id| {
            state
                .objects
                .get(id)
                .map(|o| Some(o.controller) == source_controller)
                .unwrap_or(false)
        })
    } else {
        false
    }
}

/// Unimplemented: matches nothing. Placeholder for trigger modes not yet supported.
fn match_unimplemented(
    _event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    false
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
        TargetFilter, TriggerDefinition, TypeFilter,
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
    fn changes_zone_etb_matches() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        // Origin: any (None means any), Destination: Battlefield
        trigger.destination = Some(Zone::Battlefield);

        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Hand,
            to: Zone::Battlefield,
        };
        assert!(match_changes_zone(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn changes_zone_dies_matches() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);

        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        };
        assert!(match_changes_zone(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn changes_zone_wrong_destination_no_match() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.destination = Some(Zone::Battlefield);

        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Hand,
            to: Zone::Graveyard,
        };
        assert!(!match_changes_zone(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn damage_done_matches() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::DamageDone);

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: crate::types::ability::TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
        };
        assert!(match_damage_done(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn damage_done_once_by_controller_matches_aggregated_combat_damage_event() {
        let mut state = setup();
        let trigger_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Professional Face-Breaker".to_string(),
            Zone::Battlefield,
        );
        let source_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Attacker A".to_string(),
            Zone::Battlefield,
        );
        let source_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Attacker B".to_string(),
            Zone::Battlefield,
        );
        for source in [source_a, source_b] {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        let mut trigger = make_trigger(TriggerMode::DamageDoneOnceByController);
        trigger.valid_source = Some(TargetFilter::Typed(
            TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
        ));
        trigger.valid_target = Some(TargetFilter::Player);

        let event = GameEvent::CombatDamageDealtToPlayer {
            player_id: PlayerId(1),
            source_ids: vec![source_a, source_b],
        };
        assert!(match_damage_done_once_by_controller(
            &event,
            &trigger,
            trigger_source,
            &state
        ));
    }

    #[test]
    fn spell_cast_matches() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::SpellCast);

        let event = GameEvent::SpellCast {
            card_id: CardId(10),
            controller: PlayerId(0),
        };
        assert!(match_spell_cast(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn unknown_trigger_mode_doesnt_crash() {
        let registry = build_trigger_registry();
        let unknown = TriggerMode::Unknown("FakeMode".to_string());
        // Unknown modes are not in the registry
        assert!(!registry.contains_key(&unknown));
    }

    #[test]
    fn registry_has_all_137_modes() {
        let registry = build_trigger_registry();
        // Count all registered modes (should be 137+)
        assert!(
            registry.len() >= 137,
            "Expected 137+ registered trigger modes, got {}",
            registry.len()
        );
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

    #[test]
    fn life_gained_matches_positive() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::LifeGained);
        let event = GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 3,
        };
        assert!(match_life_gained(&event, &trigger, ObjectId(1), &state));

        let loss_event = GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: -3,
        };
        assert!(!match_life_gained(
            &loss_event,
            &trigger,
            ObjectId(1),
            &state
        ));
    }

    #[test]
    fn life_lost_matches_negative() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::LifeLost);
        let event = GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: -3,
        };
        assert!(match_life_lost(&event, &trigger, ObjectId(1), &state));

        let gain_event = GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 3,
        };
        assert!(!match_life_lost(&gain_event, &trigger, ObjectId(1), &state));
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
    fn attacker_blocked_matches_when_source_is_blocked() {
        let mut state = setup();
        let attacker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        let blocker = ObjectId(99);

        let event = GameEvent::BlockersDeclared {
            assignments: vec![(blocker, attacker)],
        };
        let trigger = make_trigger(TriggerMode::AttackerBlocked);
        assert!(match_attacker_blocked(&event, &trigger, attacker, &state));
    }

    #[test]
    fn attacker_blocked_does_not_match_other_attacker() {
        let state = setup();
        let other = ObjectId(50);
        let blocker = ObjectId(99);

        let event = GameEvent::BlockersDeclared {
            assignments: vec![(blocker, other)],
        };
        let trigger = make_trigger(TriggerMode::AttackerBlocked);
        assert!(!match_attacker_blocked(
            &event,
            &trigger,
            ObjectId(1),
            &state
        ));
    }

    #[test]
    fn attacker_unblocked_matches_when_source_is_not_blocked() {
        let mut state = setup();
        let attacker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        // Set up combat state with our attacker
        state.combat = Some(crate::game::combat::CombatState {
            attackers: vec![crate::game::combat::AttackerInfo {
                object_id: attacker,
                defending_player: PlayerId(1),
            }],
            ..Default::default()
        });

        // No blockers assigned to attacker
        let event = GameEvent::BlockersDeclared {
            assignments: vec![],
        };
        let trigger = make_trigger(TriggerMode::AttackerUnblocked);
        assert!(match_attacker_unblocked(&event, &trigger, attacker, &state));
    }

    #[test]
    fn exiled_matches_zone_change_to_exile() {
        let state = setup();
        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Battlefield,
            to: Zone::Exile,
        };
        let trigger = make_trigger(TriggerMode::Exiled);
        assert!(match_exiled(&event, &trigger, ObjectId(5), &state));
    }

    #[test]
    fn exiled_does_not_match_other_zones() {
        let state = setup();
        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        };
        let trigger = make_trigger(TriggerMode::Exiled);
        assert!(!match_exiled(&event, &trigger, ObjectId(5), &state));
    }

    #[test]
    fn milled_matches_library_to_graveyard() {
        let state = setup();
        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Library,
            to: Zone::Graveyard,
        };
        let trigger = make_trigger(TriggerMode::Milled);
        assert!(match_milled(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn milled_does_not_match_hand_to_graveyard() {
        let state = setup();
        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Hand,
            to: Zone::Graveyard,
        };
        let trigger = make_trigger(TriggerMode::Milled);
        assert!(!match_milled(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn always_matcher_returns_true() {
        let state = setup();
        let event = GameEvent::GameStarted;
        let trigger = make_trigger(TriggerMode::Always);
        assert!(match_always(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn taps_for_mana_matches_mana_added() {
        let state = setup();
        let source = ObjectId(5);
        let event = GameEvent::ManaAdded {
            player_id: PlayerId(0),
            mana_type: crate::types::mana::ManaType::Green,
            source_id: source,
        };
        let trigger = make_trigger(TriggerMode::TapsForMana);
        assert!(match_taps_for_mana(&event, &trigger, source, &state));
    }

    #[test]
    fn taps_for_mana_matches_valid_card_filter() {
        let mut state = setup();
        let aura = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Wild Growth".to_string(),
            Zone::Battlefield,
        );
        let enchanted_land = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&aura).unwrap().attached_to = Some(enchanted_land);

        let event = GameEvent::ManaAdded {
            player_id: PlayerId(0),
            mana_type: crate::types::mana::ManaType::Green,
            source_id: enchanted_land,
        };

        let mut trigger = make_trigger(TriggerMode::TapsForMana);
        trigger.valid_card = Some(TargetFilter::AttachedTo);
        assert!(match_taps_for_mana(&event, &trigger, aura, &state));
    }

    #[test]
    fn taps_for_mana_respects_player_filter() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Mana Flare".to_string(),
            Zone::Battlefield,
        );
        let tapped_land = create_object(
            &mut state,
            CardId(7),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&tapped_land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let event = GameEvent::ManaAdded {
            player_id: PlayerId(1),
            mana_type: crate::types::mana::ManaType::Green,
            source_id: tapped_land,
        };

        let mut trigger = make_trigger(TriggerMode::TapsForMana);
        trigger.valid_target = Some(TargetFilter::Controller);
        trigger.valid_card = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)));
        assert!(!match_taps_for_mana(&event, &trigger, source, &state));
    }

    #[test]
    fn drawn_respects_opponent_filter() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Underworld Dreams".to_string(),
            Zone::Battlefield,
        );

        let mut trigger = make_trigger(TriggerMode::Drawn);
        trigger.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(crate::types::ability::ControllerRef::Opponent),
        ));

        let opponent_event = GameEvent::CardDrawn {
            player_id: PlayerId(1),
            object_id: ObjectId(20),
        };
        assert!(match_drawn(&opponent_event, &trigger, source, &state));

        let controller_event = GameEvent::CardDrawn {
            player_id: PlayerId(0),
            object_id: ObjectId(21),
        };
        assert!(!match_drawn(&controller_event, &trigger, source, &state));
    }

    #[test]
    fn shuffled_matches_shuffled_event() {
        let state = setup();
        let event = GameEvent::EffectResolved {
            kind: EffectKind::Shuffle,
            source_id: ObjectId(1),
        };
        let trigger = make_trigger(TriggerMode::Shuffled);
        assert!(match_shuffled(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn phase_trigger_matches_correct_phase() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::Phase);
        trigger.phase = Some(crate::types::phase::Phase::Upkeep);

        let event = GameEvent::PhaseChanged {
            phase: crate::types::phase::Phase::Upkeep,
        };
        assert!(match_phase(&event, &trigger, ObjectId(1), &state));

        let wrong_phase_event = GameEvent::PhaseChanged {
            phase: crate::types::phase::Phase::Draw,
        };
        assert!(!match_phase(
            &wrong_phase_event,
            &trigger,
            ObjectId(1),
            &state
        ));
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

    #[test]
    fn target_filter_matches_creature() {
        let mut state = setup();
        let creature = create_object(
            &mut state,
            CardId(1),
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

        let filter = TargetFilter::Typed(TypedFilter::creature());
        assert!(target_filter_matches_object(
            &state,
            creature,
            &filter,
            ObjectId(99)
        ));

        let land_filter = TargetFilter::Typed(TypedFilter::land());
        assert!(!target_filter_matches_object(
            &state,
            creature,
            &land_filter,
            ObjectId(99)
        ));
    }

    #[test]
    fn target_filter_self_ref() {
        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Self Card".to_string(),
            Zone::Battlefield,
        );
        let filter = TargetFilter::SelfRef;
        // SelfRef matches when object_id == source_id
        assert!(target_filter_matches_object(
            &state, obj_id, &filter, obj_id
        ));
        // Does not match when source is different
        assert!(!target_filter_matches_object(
            &state,
            obj_id,
            &filter,
            ObjectId(999)
        ));
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
    fn commit_crime_matcher_fires_for_controller() {
        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Criminal".to_string(),
            Zone::Battlefield,
        );

        let event = GameEvent::CrimeCommitted {
            player_id: PlayerId(0),
        };
        let trigger = make_trigger(TriggerMode::CommitCrime);

        assert!(match_commit_crime(&event, &trigger, obj_id, &state));
    }

    #[test]
    fn commit_crime_matcher_ignores_opponent_crime() {
        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Criminal".to_string(),
            Zone::Battlefield,
        );

        // Opponent committed the crime, not us
        let event = GameEvent::CrimeCommitted {
            player_id: PlayerId(1),
        };
        let trigger = make_trigger(TriggerMode::CommitCrime);

        assert!(!match_commit_crime(&event, &trigger, obj_id, &state));
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

    #[test]
    fn is_chosen_creature_type_filter_matches() {
        let mut state = setup();

        // Metallic Mimic on battlefield with chosen type "Elf"
        let mimic = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Metallic Mimic".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&mimic)
            .unwrap()
            .chosen_attributes
            .push(crate::types::ability::ChosenAttribute::CreatureType(
                "Elf".to_string(),
            ));

        // Elf creature entering
        let elf = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&elf).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
        }

        // Non-elf creature
        let goblin = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Goblin Guide".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&goblin).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.card_types.subtypes.push("Goblin".to_string());
        }

        let filter = TargetFilter::Typed(
            TypedFilter::creature()
                .properties(vec![FilterProp::Another, FilterProp::IsChosenCreatureType]),
        );

        // Elf matches (is chosen type and is another creature)
        assert!(target_filter_matches_object(&state, elf, &filter, mimic));

        // Goblin doesn't match (wrong creature type)
        assert!(!target_filter_matches_object(
            &state, goblin, &filter, mimic
        ));

        // Mimic doesn't match itself (Another filter)
        assert!(!target_filter_matches_object(&state, mimic, &filter, mimic));
    }

    #[test]
    fn is_chosen_creature_type_no_choice_rejects() {
        let mut state = setup();

        // Source with no chosen creature type
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "No Choice".to_string(),
            Zone::Battlefield,
        );

        let elf = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&elf).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
        }

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::IsChosenCreatureType]),
        );

        // No chosen type → always rejects
        assert!(!target_filter_matches_object(&state, elf, &filter, source));
    }

    // --- Counter filter tests ---

    #[test]
    fn counter_filter_threshold_crossing() {
        use crate::types::ability::CounterTriggerFilter;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let saga_id = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Saga".to_string(),
            Zone::Battlefield,
        );
        // Saga now has 1 lore counter (counter was just added: 0 → 1)
        state
            .objects
            .get_mut(&saga_id)
            .unwrap()
            .counters
            .insert(crate::game::game_object::CounterType::Lore, 1);

        let event = GameEvent::CounterAdded {
            object_id: saga_id,
            counter_type: crate::game::game_object::CounterType::Lore,
            count: 1,
        };

        // Trigger for chapter 1 (threshold=1) should fire: 0 < 1 <= 1
        let trigger_ch1 = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::game::game_object::CounterType::Lore,
                threshold: Some(1),
            });
        assert!(match_counter_added(&event, &trigger_ch1, saga_id, &state));

        // Trigger for chapter 2 (threshold=2) should NOT fire: 0 < 2, but 2 > 1
        let trigger_ch2 = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::game::game_object::CounterType::Lore,
                threshold: Some(2),
            });
        assert!(!match_counter_added(&event, &trigger_ch2, saga_id, &state));
    }

    #[test]
    fn counter_filter_double_addition() {
        use crate::types::ability::CounterTriggerFilter;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let saga_id = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Saga".to_string(),
            Zone::Battlefield,
        );
        // Saga now has 2 lore counters (added 2 at once, e.g., Vorinclex)
        state
            .objects
            .get_mut(&saga_id)
            .unwrap()
            .counters
            .insert(crate::game::game_object::CounterType::Lore, 2);

        let event = GameEvent::CounterAdded {
            object_id: saga_id,
            counter_type: crate::game::game_object::CounterType::Lore,
            count: 2, // Added 2 at once
        };

        // Both chapter 1 (threshold=1) and chapter 2 (threshold=2) should fire
        // because previous=0, current=2, so 0 < 1 <= 2 and 0 < 2 <= 2
        let trigger_ch1 = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::game::game_object::CounterType::Lore,
                threshold: Some(1),
            });
        assert!(match_counter_added(&event, &trigger_ch1, saga_id, &state));

        let trigger_ch2 = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::game::game_object::CounterType::Lore,
                threshold: Some(2),
            });
        assert!(match_counter_added(&event, &trigger_ch2, saga_id, &state));

        // Chapter 3 should NOT fire: 0 < 3 but 3 > 2
        let trigger_ch3 = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::game::game_object::CounterType::Lore,
                threshold: Some(3),
            });
        assert!(!match_counter_added(&event, &trigger_ch3, saga_id, &state));
    }

    #[test]
    fn counter_filter_ignores_wrong_type() {
        use crate::types::ability::CounterTriggerFilter;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let saga_id = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Saga".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&saga_id)
            .unwrap()
            .counters
            .insert(crate::game::game_object::CounterType::Plus1Plus1, 1);

        // +1/+1 counter added, but trigger filters for lore
        let event = GameEvent::CounterAdded {
            object_id: saga_id,
            counter_type: crate::game::game_object::CounterType::Plus1Plus1,
            count: 1,
        };

        let trigger = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::game::game_object::CounterType::Lore,
                threshold: Some(1),
            });
        assert!(!match_counter_added(&event, &trigger, saga_id, &state));
    }

    #[test]
    fn counter_filter_no_threshold() {
        use crate::types::ability::CounterTriggerFilter;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let saga_id = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Saga".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&saga_id)
            .unwrap()
            .counters
            .insert(crate::game::game_object::CounterType::Lore, 1);

        let event = GameEvent::CounterAdded {
            object_id: saga_id,
            counter_type: crate::game::game_object::CounterType::Lore,
            count: 1,
        };

        // Filter with no threshold fires on any addition of the matching type
        let trigger = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::game::game_object::CounterType::Lore,
                threshold: None,
            });
        assert!(match_counter_added(&event, &trigger, saga_id, &state));
    }

    // -----------------------------------------------------------------------
    // BecomesTarget + valid_source (spell-only filtering)
    // -----------------------------------------------------------------------

    fn setup_with_spell_on_stack() -> (GameState, ObjectId) {
        let mut state = setup();
        let spell_id = ObjectId(50);
        state.stack.push(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(100),
                ability: ResolvedAbility::new(
                    crate::types::ability::Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    vec![],
                    spell_id,
                    PlayerId(0),
                ),
                cast_as_adventure: false,
            },
        });
        (state, spell_id)
    }

    fn setup_with_ability_on_stack() -> (GameState, ObjectId) {
        let mut state = setup();
        let ability_id = ObjectId(60);
        state.stack.push(StackEntry {
            id: ability_id,
            source_id: ObjectId(10),
            controller: PlayerId(1),
            kind: StackEntryKind::ActivatedAbility {
                source_id: ObjectId(10),
                ability: ResolvedAbility::new(
                    crate::types::ability::Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    vec![],
                    ObjectId(10),
                    PlayerId(1),
                ),
            },
        });
        (state, ability_id)
    }

    #[test]
    fn becomes_target_spell_only_matches_spell() {
        let (state, spell_id) = setup_with_spell_on_stack();
        // trigger_owner is the permanent with the trigger (e.g. Bonecrusher Giant)
        let trigger_owner = ObjectId(5);
        let mut trigger = make_trigger(TriggerMode::BecomesTarget);
        trigger.valid_source = Some(TargetFilter::StackSpell);

        // Event: trigger_owner becomes the target of spell_id
        let event = GameEvent::BecomesTarget {
            object_id: trigger_owner,
            source_id: spell_id,
        };
        // No valid_card, so fallback: event.object_id == source_id param
        assert!(match_becomes_target(
            &event,
            &trigger,
            trigger_owner,
            &state
        ));
    }

    #[test]
    fn becomes_target_spell_only_rejects_ability() {
        let (state, ability_id) = setup_with_ability_on_stack();
        let trigger_owner = ObjectId(5);
        let mut trigger = make_trigger(TriggerMode::BecomesTarget);
        trigger.valid_source = Some(TargetFilter::StackSpell);

        // Event: trigger_owner becomes the target of an activated ability
        let event = GameEvent::BecomesTarget {
            object_id: trigger_owner,
            source_id: ability_id,
        };
        assert!(!match_becomes_target(
            &event,
            &trigger,
            trigger_owner,
            &state
        ));
    }

    #[test]
    fn becomes_target_no_source_filter_matches_ability() {
        let (state, ability_id) = setup_with_ability_on_stack();
        let trigger_owner = ObjectId(5);
        let trigger = make_trigger(TriggerMode::BecomesTarget);
        // valid_source = None means "spell or ability"

        // Event: trigger_owner becomes the target of an activated ability — should still fire
        let event = GameEvent::BecomesTarget {
            object_id: trigger_owner,
            source_id: ability_id,
        };
        assert!(match_becomes_target(
            &event,
            &trigger,
            trigger_owner,
            &state
        ));
    }
}
