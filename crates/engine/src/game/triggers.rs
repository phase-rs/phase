use std::collections::HashSet;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, ModalChoice, PlayerFilter,
    ResolvedAbility, TargetFilter, TargetRef, TriggerCondition, TriggerDefinition, TypeFilter,
    TypedFilter, UnlessCost,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    DelayedTrigger, GameState, StackEntry, StackEntryKind, TargetSelectionConstraint,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::keywords::WardCost;
use crate::types::phase::Phase;
use crate::types::player::{Player, PlayerId};
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::ability_utils::build_resolved_from_def;
use super::filter::{matches_target_filter, spell_record_matches_filter};
use super::speed::{
    effective_speed, has_max_speed, mark_speed_trigger_used, speed_key_source,
    speed_trigger_available,
};
use super::stack;

// Re-export so existing paths stay valid.
pub use super::trigger_matchers::{build_trigger_registry, trigger_matcher};

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
    /// CR 700.2b: Modal trigger data for deferred mode selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<ModalChoice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mode_abilities: Vec<AbilityDefinition>,
    /// Human-readable trigger description from the Oracle text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// CR 702.21a: Convert a WardCost to an UnlessCost for the counter effect.
fn ward_cost_to_unless_cost(ward_cost: &WardCost) -> UnlessCost {
    match ward_cost {
        WardCost::Mana(mana_cost) => UnlessCost::Fixed {
            cost: mana_cost.clone(),
        },
        WardCost::PayLife(amount) => UnlessCost::PayLife { amount: *amount },
        WardCost::DiscardCard => UnlessCost::DiscardCard,
        WardCost::Sacrifice { count, filter } => UnlessCost::Sacrifice {
            count: *count,
            filter: filter.clone(),
        },
        // CR 702.21a + CR 701.67: Waterbend ward cost maps to mana payment.
        // Full tap-to-help semantics deferred to waterbend cost integration.
        WardCost::Waterbend(mana_cost) => UnlessCost::Fixed {
            cost: mana_cost.clone(),
        },
        // CR 702.21a: Compound ward cost — use the first mana component as the unless cost.
        // Full compound cost resolution deferred to ward cost payment integration.
        WardCost::Compound(costs) => {
            if let Some(first) = costs.first() {
                ward_cost_to_unless_cost(first)
            } else {
                UnlessCost::Fixed {
                    cost: crate::types::mana::ManaCost::zero(),
                }
            }
        }
    }
}

/// Check trigger definitions on an object against an event, collecting matches into `pending`.
///
/// When `zone_filter` is `Some(zone)`, only trigger definitions whose `trigger_zones`
/// contains that zone will be checked. This enables graveyard (and future exile) triggers
/// without scanning every zone unconditionally.
struct MatchedTrigger {
    trig_idx: usize,
    pending: PendingTrigger,
    batched: bool,
    constraint: Option<crate::types::ability::TriggerConstraint>,
}

#[allow(clippy::too_many_arguments)]
fn collect_matching_triggers(
    state: &GameState,
    event: &GameEvent,
    obj_id: ObjectId,
    controller: PlayerId,
    trigger_defs: &[TriggerDefinition],
    timestamp: u32,
    zone_filter: Option<Zone>,
    batched_this_pass: &mut HashSet<(ObjectId, usize)>,
) -> Vec<MatchedTrigger> {
    let mut pending = Vec::new();
    for (trig_idx, trig_def) in trigger_defs.iter().enumerate() {
        // Zone guard: only fire a trigger if its declared zones include the zone being scanned.
        // Empty trigger_zones defaults to battlefield-only (engine-internal triggers like
        // prowess/ward). Parser-created non-battlefield triggers set trigger_zones explicitly.
        if let Some(zone) = zone_filter {
            let zones_match = if trig_def.trigger_zones.is_empty() {
                zone == Zone::Battlefield
            } else {
                trig_def.trigger_zones.contains(&zone)
            };
            if !zones_match {
                continue;
            }
        }
        // CR 603.2c: "One or more" (batched) triggers fire once per batch of
        // simultaneous events, not once per individual event. Skip if already
        // fired in this process_triggers pass.
        if trig_def.batched && batched_this_pass.contains(&(obj_id, trig_idx)) {
            continue;
        }
        if let Some(matcher) = trigger_matcher(trig_def.mode.clone()) {
            if !matcher(event, trig_def, obj_id, state) {
                continue;
            }
            if !check_trigger_constraint(state, trig_def, obj_id, trig_idx, controller, event) {
                continue;
            }
            if let Some(ref condition) = trig_def.condition {
                if !check_trigger_condition(state, condition, controller, Some(obj_id), Some(event))
                {
                    continue;
                }
            }
            let ability = build_triggered_ability(state, trig_def, obj_id, controller);
            let (modal, mode_abilities) = trig_def
                .execute
                .as_ref()
                .map(|exec| (exec.modal.clone(), exec.mode_abilities.clone()))
                .unwrap_or_default();
            pending.push(MatchedTrigger {
                trig_idx,
                pending: PendingTrigger {
                    source_id: obj_id,
                    controller,
                    condition: trig_def.condition.clone(),
                    ability,
                    timestamp,
                    target_constraints: Vec::new(),
                    trigger_event: Some(event.clone()),
                    modal,
                    mode_abilities,
                    description: trig_def.description.clone(),
                },
                batched: trig_def.batched,
                constraint: trig_def.constraint.clone(),
            });
        }
    }
    pending
}

fn trigger_source_ids_for_zone(state: &GameState, zone: Zone) -> Vec<ObjectId> {
    match zone {
        Zone::Battlefield => state.battlefield.clone(),
        Zone::Graveyard => state
            .players
            .iter()
            .flat_map(|player| player.graveyard.iter().copied())
            .collect(),
        Zone::Exile => state.exile.clone(),
        Zone::Stack => state
            .stack
            .iter()
            .filter_map(|entry| match &entry.kind {
                StackEntryKind::Spell { .. } => Some(entry.id),
                StackEntryKind::ActivatedAbility { .. }
                | StackEntryKind::TriggeredAbility { .. } => None,
            })
            .collect(),
        Zone::Hand | Zone::Library | Zone::Command => Vec::new(),
    }
}

/// Process events and place triggered abilities on the stack in APNAP order.
/// CR 603.3b: Process triggered abilities waiting to be put on the stack.
pub fn process_triggers(state: &mut GameState, events: &[GameEvent]) {
    let mut pending: Vec<PendingTrigger> = Vec::new();
    // CR 603.2c: Track which batched triggers (source_id, trig_idx) have already
    // fired in this pass so "one or more" triggers fire at most once per batch.
    let mut batched_this_pass: HashSet<(ObjectId, usize)> = HashSet::new();

    for event in events {
        // Scan all permanents on the battlefield for matching triggers
        for obj_id in trigger_source_ids_for_zone(state, Zone::Battlefield) {
            let (
                controller,
                timestamp,
                has_prowess,
                has_exploit,
                firebending_n,
                ward_costs,
                matched_triggers,
            ) = {
                let obj = match state.objects.get(&obj_id) {
                    Some(o) => o,
                    None => continue,
                };
                let fb_n = obj.keywords.iter().find_map(|k| {
                    if let Keyword::Firebending(n) = k {
                        Some(*n)
                    } else {
                        None
                    }
                });
                // CR 702.21a: Collect all ward costs — each instance triggers independently.
                let wards = if matches!(event, GameEvent::BecomesTarget { .. }) {
                    obj.keywords
                        .iter()
                        .filter_map(|k| {
                            if let Keyword::Ward(cost) = k {
                                Some(cost.clone())
                            } else {
                                None
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                (
                    obj.controller,
                    obj.entered_battlefield_turn.unwrap_or(0),
                    matches!(event, GameEvent::SpellCast { .. })
                        && obj.has_keyword(&Keyword::Prowess),
                    matches!(event, GameEvent::ZoneChanged { .. })
                        && obj.has_keyword(&Keyword::Exploit),
                    fb_n,
                    wards,
                    collect_matching_triggers(
                        state,
                        event,
                        obj_id,
                        obj.controller,
                        &obj.trigger_definitions,
                        obj.entered_battlefield_turn.unwrap_or(0),
                        Some(Zone::Battlefield),
                        &mut batched_this_pass,
                    ),
                )
            };

            for matched in matched_triggers {
                record_trigger_fired(state, matched.constraint.as_ref(), obj_id, matched.trig_idx);
                if matched.batched {
                    batched_this_pass.insert((obj_id, matched.trig_idx));
                }
                pending.push(matched.pending);
            }

            // CR 702.108a: Prowess triggers when controller casts a noncreature spell.
            // Cards define Prowess as K:Prowess with no explicit trigger_definition,
            // so we synthetically generate the trigger here.
            if let GameEvent::SpellCast {
                controller: caster,
                object_id: spell_obj_id,
                ..
            } = event
            {
                if has_prowess && *caster == controller {
                    // Check if the cast spell is noncreature
                    let is_noncreature = state
                        .objects
                        .get(spell_obj_id)
                        .map(|obj| !obj.card_types.core_types.contains(&CoreType::Creature))
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
                            description: None,
                        });
                    }
                }
            }

            // Keyword-based triggers: Firebending
            // Firebending N triggers when a creature with firebending is declared as attacker.
            // Produces N {R} mana with EndOfCombat expiry.
            if let GameEvent::AttackersDeclared { attacker_ids, .. } = event {
                if let Some(n) = firebending_n {
                    if attacker_ids.contains(&obj_id) && n > 0 {
                        let fb_effect = Effect::Mana {
                            produced: crate::types::ability::ManaProduction::Fixed {
                                colors: vec![crate::types::mana::ManaColor::Red; n as usize],
                            },
                            restrictions: vec![],
                            expiry: Some(crate::types::mana::ManaExpiry::EndOfCombat),
                        };
                        let fb_ability =
                            ResolvedAbility::new(fb_effect, Vec::new(), obj_id, controller);
                        let fb_trig_def = TriggerDefinition::new(TriggerMode::Firebend)
                            .description(format!("Firebending {n}"));
                        pending.push(PendingTrigger {
                            source_id: obj_id,
                            controller,
                            condition: fb_trig_def.condition,
                            ability: fb_ability,
                            timestamp,
                            target_constraints: Vec::new(),
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                            description: None,
                        });
                        // Track bending type for Avatar Aang's "if you've done all four"
                        if let Some(player) = state.players.iter_mut().find(|p| p.id == controller)
                        {
                            player
                                .bending_types_this_turn
                                .insert(crate::types::events::BendingType::Fire);
                        }
                    }
                }
            }

            // Keyword-based triggers: Exploit
            // CR 702.110a: When a creature with exploit enters, the controller may sacrifice a creature.
            if has_exploit {
                if let GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } = event
                {
                    if *object_id == obj_id {
                        let exploit_target = TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            controller: Some(ControllerRef::You),
                            ..Default::default()
                        });
                        let exploit_effect = Effect::Exploit {
                            target: exploit_target,
                        };
                        let mut exploit_ability = ResolvedAbility::new(
                            exploit_effect,
                            Vec::new(),
                            *object_id,
                            controller,
                        );
                        exploit_ability.optional = true;
                        pending.push(PendingTrigger {
                            source_id: *object_id,
                            controller,
                            condition: None,
                            ability: exploit_ability,
                            timestamp,
                            target_constraints: Vec::new(),
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                            description: None,
                        });
                    }
                }
            }

            // CR 702.21a: Ward triggers when this permanent becomes the target
            // of a spell or ability an opponent controls. Each ward instance
            // triggers independently. Only fires for permanents (battlefield scan).
            if !ward_costs.is_empty() {
                if let GameEvent::BecomesTarget {
                    object_id: targeted_id,
                    source_id: targeting_source_id,
                } = event
                {
                    if *targeted_id == obj_id {
                        // Look up source controller. For spells, StackEntry.id matches source_id.
                        // For activated abilities, StackEntry.source_id matches (the permanent),
                        // and the fallback via state.objects finds the permanent's controller.
                        let source_controller = state
                            .stack
                            .iter()
                            .find(|e| {
                                e.id == *targeting_source_id || e.source_id == *targeting_source_id
                            })
                            .map(|e| e.controller)
                            .or_else(|| {
                                state.objects.get(targeting_source_id).map(|o| o.controller)
                            });

                        if let Some(src_ctrl) = source_controller {
                            if src_ctrl != controller {
                                for ward in &ward_costs {
                                    let unless_cost = ward_cost_to_unless_cost(ward);
                                    let counter_effect = Effect::Counter {
                                        target: TargetFilter::TriggeringSource,
                                        source_static: None,
                                        unless_payment: Some(unless_cost),
                                    };
                                    let ward_ability = ResolvedAbility::new(
                                        counter_effect,
                                        Vec::new(),
                                        obj_id,
                                        controller,
                                    );
                                    pending.push(PendingTrigger {
                                        source_id: obj_id,
                                        controller,
                                        condition: None,
                                        ability: ward_ability,
                                        timestamp,
                                        target_constraints: Vec::new(),
                                        trigger_event: Some(event.clone()),
                                        modal: None,
                                        mode_abilities: vec![],
                                        description: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // CR 603.10a: Leaves-the-battlefield abilities look back in time. Objects that
        // just left the battlefield (e.g., sacrificed, destroyed, exiled) are scanned with
        // zone_filter=Battlefield so their battlefield-zone triggers can still fire. This
        // covers "dies," "leaves the battlefield," and "exiled from battlefield" triggers.
        // We use the ZoneChanged event itself to identify which objects left, then scan
        // them as if they were still on the battlefield (last-known-information).
        if let GameEvent::ZoneChanged {
            object_id: moved_id,
            from: Zone::Battlefield,
            ..
        } = event
        {
            // Only scan if the object wasn't already found by the battlefield scan
            // (it won't be — it has already moved out — but guard against double-fire).
            if state
                .objects
                .get(moved_id)
                .is_some_and(|o| o.zone != Zone::Battlefield)
            {
                let matched_triggers = {
                    let obj = &state.objects[moved_id];
                    collect_matching_triggers(
                        state,
                        event,
                        *moved_id,
                        obj.controller,
                        &obj.trigger_definitions,
                        obj.entered_battlefield_turn.unwrap_or(0),
                        Some(Zone::Battlefield),
                        &mut batched_this_pass,
                    )
                };
                for matched in matched_triggers {
                    record_trigger_fired(
                        state,
                        matched.constraint.as_ref(),
                        *moved_id,
                        matched.trig_idx,
                    );
                    if matched.batched {
                        batched_this_pass.insert((*moved_id, matched.trig_idx));
                    }
                    pending.push(matched.pending);
                }
            }
        }

        // CR 113.6k: Non-battlefield trigger zones are opt-in via trigger_zones.
        for zone in [Zone::Graveyard, Zone::Exile, Zone::Stack] {
            for obj_id in trigger_source_ids_for_zone(state, zone) {
                let matched_triggers = {
                    let obj = match state.objects.get(&obj_id) {
                        Some(o) => o,
                        None => continue,
                    };
                    collect_matching_triggers(
                        state,
                        event,
                        obj_id,
                        obj.controller,
                        &obj.trigger_definitions,
                        0,
                        Some(zone),
                        &mut batched_this_pass,
                    )
                };

                for matched in matched_triggers {
                    record_trigger_fired(
                        state,
                        matched.constraint.as_ref(),
                        obj_id,
                        matched.trig_idx,
                    );
                    if matched.batched {
                        batched_this_pass.insert((obj_id, matched.trig_idx));
                    }
                    pending.push(matched.pending);
                }
            }
        }

        // CR 724.2: At the beginning of the monarch's end step, that player draws a card.
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
                        .description("Monarch draw (CR 724.2)".to_string());
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
                        description: None,
                    });
                }
            }
        }

        // CR 725.2: At the beginning of the initiative holder's upkeep,
        // that player ventures into the Undercity. Synthetic game-rule trigger.
        if let GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        } = event
        {
            if let Some(init_holder) = state.initiative {
                if init_holder == state.active_player {
                    let venture_effect = Effect::VentureInto {
                        dungeon: crate::game::dungeon::DungeonId::Undercity,
                    };
                    let venture_ability =
                        ResolvedAbility::new(venture_effect, Vec::new(), ObjectId(0), init_holder);
                    let trig_def = TriggerDefinition::new(TriggerMode::Phase)
                        .description("Initiative upkeep venture (CR 725.2)".to_string());
                    pending.push(PendingTrigger {
                        source_id: ObjectId(0),
                        controller: init_holder,
                        condition: trig_def.condition,
                        ability: venture_ability,
                        timestamp: 0,
                        target_constraints: Vec::new(),
                        trigger_event: Some(event.clone()),
                        modal: None,
                        mode_abilities: vec![],
                        description: None,
                    });
                }
            }
        }

        // CR 724.2: When a creature deals combat damage to the monarch, its controller
        // becomes the monarch. Synthetic game-rule trigger.
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
                            .description("Monarch steal (CR 724.2)".to_string());
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
                            description: None,
                        });
                    }
                }
            }
        }

        // CR 725.2: When a creature deals combat damage to the initiative holder,
        // its controller takes the initiative. Synthetic game-rule trigger.
        if let GameEvent::DamageDealt {
            source_id,
            target: TargetRef::Player(target_player),
            is_combat: true,
            ..
        } = event
        {
            if state.initiative == Some(*target_player) {
                if let Some(attacker) = state.objects.get(source_id) {
                    let new_holder = attacker.controller;
                    if new_holder != *target_player {
                        let take_init = ResolvedAbility::new(
                            Effect::TakeTheInitiative,
                            Vec::new(),
                            *source_id,
                            new_holder,
                        );
                        let trig_def = TriggerDefinition::new(TriggerMode::DamageDone)
                            .description("Initiative steal (CR 725.2)".to_string());
                        pending.push(PendingTrigger {
                            source_id: *source_id,
                            controller: new_holder,
                            condition: trig_def.condition,
                            ability: take_init,
                            timestamp: 0,
                            target_constraints: Vec::new(),
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                            description: None,
                        });
                    }
                }
            }
        }

        // CR 702.179d: The player with speed has an inherent no-source trigger that
        // increases their speed once each turn when one or more opponents lose life
        // during that player's turn, if their speed is less than 4.
        if let GameEvent::LifeChanged { player_id, amount } = event {
            let trigger_controller = state.active_player;
            if *amount < 0
                && *player_id != trigger_controller
                && effective_speed(state, trigger_controller) > 0
                && speed_trigger_available(state, trigger_controller)
                && !has_max_speed(state, trigger_controller)
            {
                let increase_ability = ResolvedAbility::new(
                    Effect::IncreaseSpeed {
                        player_scope: PlayerFilter::Controller,
                        amount: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    },
                    Vec::new(),
                    speed_key_source(),
                    trigger_controller,
                );
                let trig_def = TriggerDefinition::new(TriggerMode::LifeLost)
                    .description("Start your engines! (CR 702.179d)".to_string());
                pending.push(PendingTrigger {
                    source_id: speed_key_source(),
                    controller: trigger_controller,
                    condition: trig_def.condition,
                    ability: increase_ability,
                    timestamp: 0,
                    target_constraints: Vec::new(),
                    trigger_event: Some(event.clone()),
                    modal: None,
                    mode_abilities: vec![],
                    description: None,
                });
                mark_speed_trigger_used(state, trigger_controller);
            }
        }
    }

    // CR 603.2d: Trigger doubling — Panharmonicon-style effects.
    // Scan battlefield for objects with StaticMode::Panharmonicon statics,
    // then clone matching pending triggers.
    apply_trigger_doubling(state, &mut pending);

    if pending.is_empty() {
        return;
    }

    // CR 603.3b: Active player's triggers are ordered before non-active player's triggers.
    // Within same controller, order by timestamp.
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
        // CR 700.2b: Modal triggered ability — stash for mode selection before pushing to stack.
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

        match super::ability_utils::auto_select_targets_for_ability(
            state,
            &trigger.ability,
            &target_slots,
            &trigger.target_constraints,
        ) {
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

    // Clear transient cast_from_zone on all objects after trigger collection.
    // This field only needs to survive long enough for ETB trigger processing.
    for obj in state.objects.values_mut() {
        obj.cast_from_zone = None;
    }
}

/// CR 603.3: Put triggered ability on the stack.
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
            description: trigger.description,
        },
    };
    stack::push_to_stack(state, entry, events);
}

/// CR 603.2d: Apply trigger doubling from Panharmonicon-style static abilities.
/// Scans battlefield for permanents with `StaticMode::Panharmonicon` statics,
/// then clones matching pending triggers an additional time.
fn apply_trigger_doubling(state: &GameState, pending: &mut Vec<PendingTrigger>) {
    // Collect doubling sources: (controller, source_id, affected filter)
    let doublers: Vec<(PlayerId, ObjectId, Option<TargetFilter>)> = state
        .battlefield
        .iter()
        .filter_map(|&obj_id| {
            let obj = state.objects.get(&obj_id)?;
            let has_panharmonicon = obj
                .static_definitions
                .iter()
                .any(|sd| matches!(sd.mode, StaticMode::Panharmonicon));
            if has_panharmonicon {
                // Use the first Panharmonicon static's affected filter
                let affected = obj
                    .static_definitions
                    .iter()
                    .find(|sd| matches!(sd.mode, StaticMode::Panharmonicon))
                    .and_then(|sd| sd.affected.clone());
                Some((obj.controller, obj_id, affected))
            } else {
                None
            }
        })
        .collect();

    if doublers.is_empty() {
        return;
    }

    let mut extra: Vec<PendingTrigger> = Vec::new();
    for (doubler_controller, doubler_id, ref affected) in &doublers {
        for trigger in pending.iter() {
            // Controller match: trigger source must be controlled by the doubler's controller
            if trigger.controller != *doubler_controller {
                continue;
            }
            // Self-exclusion: don't double triggers from the Panharmonicon itself entering
            if trigger.source_id == *doubler_id {
                continue;
            }
            // CR 603.2d: If the doubler specifies an affected filter (e.g. "creature you
            // control of the chosen type"), only double triggers from matching sources.
            if let Some(filter) = affected {
                if !matches_target_filter(state, trigger.source_id, filter, *doubler_id) {
                    continue;
                }
            }
            extra.push(trigger.clone());
        }
    }
    pending.extend(extra);
}

/// CR 603.8: Check state triggers for all permanents on the battlefield.
/// State triggers fire when a game-state condition is true, rather than in response
/// to events. A state trigger doesn't trigger again until its ability has resolved,
/// been countered, or otherwise left the stack.
pub fn check_state_triggers(state: &mut GameState) {
    let source_ids: Vec<ObjectId> = state.battlefield.to_vec();

    let mut pending: Vec<PendingTrigger> = Vec::new();

    for obj_id in source_ids {
        let (controller, timestamp, trigger_defs) = {
            let Some(obj) = state.objects.get(&obj_id) else {
                continue;
            };
            if obj.zone != Zone::Battlefield {
                continue;
            }
            (
                obj.controller,
                obj.entered_battlefield_turn.unwrap_or(0),
                obj.trigger_definitions.clone(),
            )
        };

        for trigger in &trigger_defs {
            if trigger.mode != TriggerMode::StateCondition {
                continue;
            }

            // CR 603.8: Don't re-trigger if this state trigger is already on the stack.
            let already_on_stack = state.stack.iter().any(|entry| {
                entry.source_id == obj_id
                    && matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. })
            });
            if already_on_stack {
                continue;
            }

            // Evaluate the condition
            let condition_met = trigger.condition.as_ref().is_some_and(|cond| {
                check_trigger_condition(state, cond, controller, Some(obj_id), None)
            });

            if condition_met {
                let execute = trigger.execute.as_deref().cloned().unwrap_or_else(|| {
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Unimplemented {
                            name: "state trigger".to_string(),
                            description: trigger.description.clone(),
                        },
                    )
                });

                let ability = build_resolved_from_def(&execute, obj_id, controller);
                pending.push(PendingTrigger {
                    source_id: obj_id,
                    controller,
                    condition: trigger.condition.clone(),
                    ability,
                    timestamp,
                    target_constraints: Vec::new(),
                    trigger_event: None,
                    modal: None,
                    mode_abilities: vec![],
                    description: trigger.description.clone(),
                });
            }
        }
    }

    if pending.is_empty() {
        return;
    }

    // CR 603.3b: APNAP ordering for state triggers.
    pending.sort_by_key(|t| {
        let is_nap = if t.controller == state.active_player {
            0
        } else {
            1
        };
        (is_nap, t.timestamp)
    });
    pending.reverse();

    let mut events_out = Vec::new();
    for trigger in pending {
        push_pending_trigger_to_stack(state, trigger, &mut events_out);
    }
}

/// CR 603.7: Check if any delayed triggers should fire based on recent events.
/// One-shot triggers are removed after firing; multi-fire (WheneverEvent) triggers
/// persist until end-of-turn cleanup (CR 603.7c).
pub fn check_delayed_triggers(state: &mut GameState, events: &[GameEvent]) -> Vec<GameEvent> {
    if state.delayed_triggers.is_empty() {
        return vec![];
    }

    // Separate "abilities to fire" from "indices to remove".
    // One-shot triggers are removed; multi-fire triggers are cloned and left in place.
    let mut to_fire: Vec<DelayedTrigger> = Vec::new();
    let mut to_remove: Vec<usize> = Vec::new();

    for (idx, delayed) in state.delayed_triggers.iter().enumerate() {
        if delayed_trigger_matches(&delayed.condition, events, state, delayed.source_id) {
            if delayed.one_shot {
                to_remove.push(idx);
            } else {
                to_fire.push(delayed.clone());
            }
        }
    }

    // Remove one-shot triggers in reverse order to preserve indices, collecting into to_fire
    for &idx in to_remove.iter().rev() {
        to_fire.push(state.delayed_triggers.remove(idx));
    }

    if to_fire.is_empty() {
        return vec![];
    }

    let mut new_events = Vec::new();

    // CR 603.3b: APNAP ordering — active player's triggers go on stack last (resolve first).
    // Sort so NAP triggers come first (pushed to stack bottom), AP triggers last (stack top).
    to_fire.sort_by_key(|t| {
        let is_nap = if t.controller == state.active_player {
            0
        } else {
            1
        };
        (is_nap, state.turn_number)
    });
    to_fire.reverse();

    for trigger in to_fire {
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
            description: None,
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
    source_id: ObjectId,
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
        // CR 603.7c: "when [object] dies" — zone change to graveyard from battlefield
        DelayedTriggerCondition::WhenDies { .. } => events.iter().any(|e| {
            matches!(
                e,
                GameEvent::ZoneChanged {
                    from: Zone::Battlefield,
                    to: Zone::Graveyard,
                    ..
                }
            )
        }),
        // CR 603.7c: "when [object] leaves the battlefield" — any zone change from battlefield
        DelayedTriggerCondition::WhenLeavesPlayFiltered { .. } => events.iter().any(|e| {
            matches!(
                e,
                GameEvent::ZoneChanged {
                    from: Zone::Battlefield,
                    ..
                }
            )
        }),
        // CR 603.7c: "when [object] enters the battlefield" — zone change to battlefield
        DelayedTriggerCondition::WhenEntersBattlefield { .. } => events.iter().any(|e| {
            matches!(
                e,
                GameEvent::ZoneChanged {
                    to: Zone::Battlefield,
                    ..
                }
            )
        }),
        // "when [object] dies or is exiled" — zone change to graveyard OR exile from battlefield.
        // Building block for Earthbending return trigger.
        DelayedTriggerCondition::WhenDiesOrExiled { object_id } => events.iter().any(|e| {
            matches!(
                e,
                GameEvent::ZoneChanged {
                    object_id: id,
                    from: Zone::Battlefield,
                    to: Zone::Graveyard | Zone::Exile,
                }
                if *id == *object_id
            )
        }),
        // CR 603.7c: "Whenever [event] this turn" — delegate to trigger matcher registry.
        DelayedTriggerCondition::WheneverEvent { trigger } => {
            if let Some(matcher) = super::trigger_matchers::trigger_matcher(trigger.mode.clone()) {
                events
                    .iter()
                    .any(|event| matcher(event, trigger, source_id, state))
            } else {
                false
            }
        }
    }
}

/// Check whether a trigger's constraint allows it to fire.
///
/// `event` is the triggering event — needed by `NthSpellThisTurn` to identify
/// the caster and count their per-player spell total (not the global count).
fn check_trigger_constraint(
    state: &GameState,
    trig_def: &TriggerDefinition,
    obj_id: ObjectId,
    trig_idx: usize,
    controller: PlayerId,
    event: &GameEvent,
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
        TriggerConstraint::OnlyDuringOpponentsTurn => state.active_player != controller,
        // CR 603.2: Per-caster spell count. The caster is extracted from the SpellCast
        // event; the count comes from the per-player map (not the global counter).
        // When `filter` contains `TypeFilter::Non(Creature)`, use the noncreature counter.
        TriggerConstraint::NthSpellThisTurn { n, filter } => {
            let caster = match event {
                GameEvent::SpellCast { controller: c, .. } => *c,
                _ => return false,
            };
            let count = state
                .spells_cast_this_turn_by_player
                .get(&caster)
                .map_or(0, |spells| match filter {
                    None => spells.len() as u32,
                    Some(filter) => spells
                        .iter()
                        .filter(|record| spell_record_matches_filter(record, filter, caster))
                        .count() as u32,
                });
            count == *n
        }
        // CR 121.2: Extract the drawing player from the event (not the controller).
        // Matches the NthSpellThisTurn pattern which extracts the caster from SpellCast.
        TriggerConstraint::NthDrawThisTurn { n } => {
            let drawer = match event {
                GameEvent::CardDrawn { player_id, .. } => *player_id,
                _ => return false,
            };
            state
                .players
                .iter()
                .find(|p| p.id == drawer)
                .is_some_and(|p| p.cards_drawn_this_turn == *n)
        }
        // CR 716.2a: "When this Class becomes level N" — fire only at the specified level.
        TriggerConstraint::AtClassLevel { level } => state
            .objects
            .get(&obj_id)
            .and_then(|obj| obj.class_level)
            .is_some_and(|current| current == *level),
        // CR 603.4: "This ability triggers only the first N times each turn."
        TriggerConstraint::MaxTimesPerTurn { max } => {
            let count = state
                .trigger_fire_counts_this_turn
                .get(&key)
                .copied()
                .unwrap_or(0);
            count < *max
        }
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
    trigger_event: Option<&GameEvent>,
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
        // CR 508.1a: Count co-attackers excluding the source creature.
        TriggerCondition::MinCoAttackers { minimum } => {
            state.combat.as_ref().is_some_and(|combat| {
                let co_attacker_count = combat
                    .attackers
                    .iter()
                    .filter(|a| {
                        a.object_id != source_id.unwrap_or(ObjectId(0))
                            && state
                                .objects
                                .get(&a.object_id)
                                .is_some_and(|obj| obj.controller == controller)
                    })
                    .count();
                co_attacker_count >= *minimum as usize
            })
        }
        // CR 719.2: True when the source Case is unsolved and its solve condition is met.
        TriggerCondition::SolveConditionMet => source_id
            .and_then(|id| state.objects.get(&id))
            .and_then(|obj| obj.case_state.as_ref())
            .is_some_and(|cs| !cs.is_solved && evaluate_solve_condition(state, cs, controller)),
        // CR 716.2a: True when the source Class is at or above the specified level.
        TriggerCondition::ClassLevelGE { level } => source_id
            .and_then(|id| state.objects.get(&id))
            .and_then(|obj| obj.class_level)
            .is_some_and(|current| current >= *level),
        // "if you cast it" — true when the source was cast (regardless of zone).
        TriggerCondition::WasCast => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.cast_from_zone.is_some()),
        // CR 508.1: "if it's attacking" — true when the trigger source is in combat.attackers.
        TriggerCondition::SourceIsAttacking => {
            let sid = source_id.unwrap_or(ObjectId(0));
            state
                .combat
                .as_ref()
                .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == sid))
        }
        // CR 702.49 + CR 603.4: "if its sneak/ninjutsu cost was paid this turn"
        TriggerCondition::NinjutsuVariantPaid { variant } => source_id
            .and_then(|id| state.objects.get(&id))
            .map(|obj| obj.ninjutsu_variant_paid == Some((variant.clone(), state.turn_number)))
            .unwrap_or(false),
        // CR 601.2: True when the current turn's active player is an opponent.
        TriggerCondition::DuringOpponentsTurn => state.active_player != controller,
        // CR 700.4 + CR 120.1: True when the dying creature was dealt damage by the
        // trigger source this turn.
        TriggerCondition::DealtDamageBySourceThisTurn => {
            // Extract the dying creature's ID from the trigger event. Only
            // CreatureDestroyed and ZoneChanged (dies = battlefield→graveyard)
            // carry the dying creature — other event shapes are not valid here.
            let dying_creature = trigger_event.and_then(|e| match e {
                GameEvent::CreatureDestroyed { object_id } => Some(*object_id),
                GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                _ => None,
            });
            match (source_id, dying_creature) {
                (Some(src), Some(subj)) => state
                    .damage_dealt_this_turn
                    .iter()
                    .any(|r| r.source_id == src && r.target == TargetRef::Object(subj)),
                _ => false,
            }
        }
        // CR 400.7 + CR 603.10: "if it was a [type]" — check LKI for the source's
        // core types at the time it left the battlefield.
        TriggerCondition::WasType { card_type } => source_id
            .and_then(|id| state.lki_cache.get(&id))
            .is_some_and(|lki| lki.card_types.contains(card_type)),
        // "if you control a [type]" — check for presence of matching permanent.
        TriggerCondition::ControlsType { filter } => state.battlefield.iter().any(|id| {
            crate::game::filter::matches_target_filter(
                state,
                *id,
                filter,
                source_id.unwrap_or(ObjectId(0)),
            )
        }),
        // CR 603.8: "when you control no [type]" — true when no permanents match the filter.
        TriggerCondition::ControlsNone { filter } => !state.battlefield.iter().any(|id| {
            crate::game::filter::matches_target_filter(
                state,
                *id,
                filter,
                source_id.unwrap_or(ObjectId(0)),
            )
        }),
        // CR 603.4: "if no spells were cast last turn" — check previous turn spell count.
        TriggerCondition::NoSpellsCastLastTurn => state.spells_cast_last_turn.unwrap_or(0) == 0,
        // CR 603.4: "if two or more spells were cast last turn"
        TriggerCondition::TwoOrMoreSpellsCastLastTurn => {
            state.spells_cast_last_turn.unwrap_or(0) >= 2
        }
        // CR 603.4: "if you have N or more life" — compare controller's life total.
        TriggerCondition::LifeTotalGE { minimum } => {
            player_field(state, controller, |p| p.life >= *minimum)
        }
        // CR 603.4: "if it's your turn"
        TriggerCondition::DuringYourTurn => state.active_player == controller,
        // CR 603.4: "if it's not your turn"
        TriggerCondition::NotYourTurn => state.active_player != controller,
        // CR 603.4: "if you control N or more [type]" — generalized control count.
        TriggerCondition::ControlCount { minimum, filter } => {
            let count = state
                .battlefield
                .iter()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == controller
                            && crate::game::filter::matches_target_filter(
                                state,
                                **id,
                                filter,
                                source_id.unwrap_or(ObjectId(0)),
                            )
                    })
                })
                .count();
            count >= *minimum as usize
        }
        // CR 508.1a: "if you attacked this turn" — true if controller declared attackers.
        TriggerCondition::AttackedThisTurn => {
            state.players_attacked_this_turn.contains(&controller)
        }
        // CR 603.4: "if you cast a [type] spell this turn" — check per-player cast history.
        TriggerCondition::CastSpellThisTurn { filter } => match filter {
            None => state
                .spells_cast_this_turn_by_player
                .get(&controller)
                .is_some_and(|spells| !spells.is_empty()),
            Some(filter) => state
                .spells_cast_this_turn_by_player
                .get(&controller)
                .is_some_and(|spells| {
                    spells
                        .iter()
                        .any(|record| spell_record_matches_filter(record, filter, controller))
                }),
        },
        TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => {
            let source_id = source_id.unwrap_or(ObjectId(0));
            let lhs = crate::game::quantity::resolve_quantity(state, lhs, controller, source_id);
            let rhs = crate::game::quantity::resolve_quantity(state, rhs, controller, source_id);
            comparator.evaluate(lhs, rhs)
        }
        TriggerCondition::HasMaxSpeed => has_max_speed(state, controller),
        // CR 122.1: "if you put a counter on a permanent this turn"
        TriggerCondition::CounterAddedThisTurn => state
            .players_who_added_counter_this_turn
            .contains(&controller),
        // CR 603.4: "if an opponent lost life during their last turn" — check the opponent's
        // snapshotted life_lost_last_turn. True if any opponent lost life during the previous turn.
        TriggerCondition::LostLifeLastTurn => state
            .players
            .iter()
            .any(|p| p.id != controller && p.life_lost_last_turn > 0),
        // CR 509.1a + CR 603.4: "if defending player controls no [type]" — check if the
        // defending player in combat controls no permanents matching the filter.
        TriggerCondition::DefendingPlayerControlsNone { filter } => {
            if let Some(combat) = &state.combat {
                let defenders: std::collections::HashSet<PlayerId> = combat
                    .attackers
                    .iter()
                    .map(|a| a.defending_player)
                    .collect();
                defenders.iter().all(|&def_pid| {
                    !state.battlefield.iter().any(|id| {
                        state.objects.get(id).is_some_and(|obj| {
                            obj.controller == def_pid
                                && crate::game::filter::matches_target_filter(
                                    state,
                                    *id,
                                    filter,
                                    source_id.unwrap_or(ObjectId(0)),
                                )
                        })
                    })
                })
            } else {
                false
            }
        }
        // CR 724.1: True when the controller is the monarch.
        TriggerCondition::IsMonarch => state.monarch == Some(controller),
        // CR 702.131a: True when the controller has the city's blessing.
        TriggerCondition::HasCityBlessing => state.city_blessing.contains(&controller),
        // CR 611.2b: True when the trigger source is tapped (or untapped when negated).
        TriggerCondition::SourceIsTapped { negated } => {
            let is_tapped = source_id
                .and_then(|id| state.objects.get(&id))
                .is_some_and(|obj| obj.tapped);
            is_tapped != *negated
        }
        // CR 113.6b: True when the trigger source is in the specified zone.
        TriggerCondition::SourceInZone { zone } => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.zone == *zone),
        // CR 702.104b: Tribute — the opponent didn't add counters.
        TriggerCondition::TributeNotPaid => {
            // TODO: Track tribute payment via replacement effect. Stub: always true
            // (conservative — the trigger fires, but its effect may not apply correctly).
            true
        }
        // CR 207.2c: Addendum — cast during main phase.
        TriggerCondition::CastDuringMainPhase => {
            matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
        }
        // CR 207.2c: Adamant — at least N mana of a specific color was spent.
        TriggerCondition::ManaColorSpent { .. } => {
            // TODO: Track mana color spending during casting. Stub: false.
            false
        }
        // CR 601.2b: General mana spending condition (text-based fallback).
        TriggerCondition::ManaSpentCondition { .. } => false,
        // CR 400.7: "if it had counters on it" — check LKI for counters.
        TriggerCondition::HadCounters { counter_type } => source_id
            .and_then(|id| state.lki_cache.get(&id))
            .is_some_and(|lki| match counter_type {
                // Specific counter type: parse to CounterType for canonical comparison.
                Some(ct) => {
                    let target = crate::types::counter::parse_counter_type(ct);
                    lki.counters.get(&target).is_some_and(|&v| v > 0)
                }
                // Any counter: check if any counter was present.
                None => lki.counters.values().any(|&v| v > 0),
            }),
        TriggerCondition::And { conditions } => conditions
            .iter()
            .all(|c| check_trigger_condition(state, c, controller, source_id, trigger_event)),
        TriggerCondition::Or { conditions } => conditions
            .iter()
            .any(|c| check_trigger_condition(state, c, controller, source_id, trigger_event)),
        // CR 309.7: True when the controller has completed at least one dungeon.
        TriggerCondition::CompletedADungeon => state
            .dungeon_progress
            .get(&controller)
            .is_some_and(|p| !p.completed.is_empty()),
        // CR 309.7: True when the controller has NOT completed a specific dungeon.
        TriggerCondition::NotCompletedDungeon { dungeon } => !state
            .dungeon_progress
            .get(&controller)
            .is_some_and(|p| p.completed.contains(dungeon)),
        // CR 903.3: True when the controller controls at least one of their commander(s).
        TriggerCondition::ControlsCommander => {
            // Commander designation is stored per-player. Check if any permanent on the
            // battlefield owned by and controlled by this player is a commander.
            state.battlefield.iter().any(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.controller == controller && obj.is_commander)
            })
        }
        // CR 702.112a: True when the source permanent has been made renowned.
        TriggerCondition::SourceIsRenowned => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.is_renowned),
        // CR 711.2a + CR 711.2b: Level-up creature trigger gating — check level counter count on source.
        TriggerCondition::HasCounters {
            counter_type,
            minimum,
            maximum,
        } => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| {
                let ct = crate::types::counter::parse_counter_type(counter_type);
                let count = obj.counters.get(&ct).copied().unwrap_or(0);
                count >= *minimum && maximum.is_none_or(|max| count <= max)
            }),
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
            comparator.evaluate(count, *threshold as i32)
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
    constraint: Option<&crate::types::ability::TriggerConstraint>,
    obj_id: ObjectId,
    trig_idx: usize,
) {
    use crate::types::ability::TriggerConstraint;

    let constraint = match constraint {
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
        | TriggerConstraint::OnlyDuringOpponentsTurn
        | TriggerConstraint::NthSpellThisTurn { .. }
        | TriggerConstraint::NthDrawThisTurn { .. }
        | TriggerConstraint::AtClassLevel { .. } => {
            // No tracking needed — checked at fire time via game/object state
        }
        // CR 603.4: Increment fire count for MaxTimesPerTurn tracking.
        TriggerConstraint::MaxTimesPerTurn { .. } => {
            *state.trigger_fire_counts_this_turn.entry(key).or_insert(0) += 1;
        }
    }
}

/// Build a ResolvedAbility from a TriggerDefinition using typed fields.
fn build_triggered_ability(
    state: &GameState,
    trig_def: &TriggerDefinition,
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    if let Some(execute) = &trig_def.execute {
        // Pre-resolved ability definition -- direct typed access
        let mut resolved = build_resolved_from_def(execute, source_id, controller);
        // Carry the trigger's description if the execute doesn't have its own.
        if resolved.description.is_none() {
            resolved.description = trig_def.description.clone();
        }
        // Propagate cast_from_zone from the source object so sub_ability
        // conditions like "if you cast it from your hand" can evaluate.
        if let Some(zone) = state.objects.get(&source_id).and_then(|o| o.cast_from_zone) {
            resolved.context.cast_from_zone = Some(zone);
        }
        // CR 118.12: Carry unless_pay modifier from trigger definition.
        if trig_def.unless_pay.is_some() {
            resolved.unless_pay = trig_def.unless_pay.clone();
        }
        resolved
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
/// CR 115.1: Only objects on the battlefield, stack, graveyard, exile, and
/// command zone can be targeted. Selections from private zones (hand, library)
/// are resolution-time choices, not targeting. ChangeZone effects with a
/// hand or library origin are therefore excluded — the resolution path
/// handles them via WaitingFor::EffectZoneChoice.
///
/// Note: TriggeringSpellController, TriggeringSpellOwner, TriggeringPlayer,
/// and TriggeringSource auto-resolve from event context at resolution time
/// (via `state.current_trigger_event`), so they do not require player selection.
pub(crate) fn extract_target_filter_from_effect(effect: &Effect) -> Option<&TargetFilter> {
    // CR 115.1: ChangeZone from private zones (hand/library) uses resolution-time
    // selection, not stack-push-time targeting.
    if let Effect::ChangeZone { origin, target, .. } = effect {
        if matches!(origin, Some(Zone::Hand) | Some(Zone::Library)) {
            return None;
        }
        // Also check InZone property when origin is None but the filter specifies a private zone
        if origin.is_none() {
            if let Some(zone) = target.extract_in_zone() {
                if matches!(zone, Zone::Hand | Zone::Library) {
                    return None;
                }
            }
        }
    }
    effect.target_filter().filter(|t| !t.is_context_ref())
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
        AbilityDefinition, AbilityKind, Comparator, ControllerRef, Effect, FilterProp,
        GainLifePlayer, QuantityExpr, QuantityRef, TargetFilter, TriggerCondition,
        TriggerConstraint, TriggerDefinition, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{GameState, SpellCastRecord};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::phase::Phase;
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
            object_id: spell,
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
            object_id: creature_spell,
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
            object_id: spell,
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

        let state = setup();
        let ability = build_triggered_ability(&state, &trig_def, ObjectId(1), PlayerId(0));
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
        let state = setup();
        let ability = build_triggered_ability(&state, &trig_def, ObjectId(1), PlayerId(0));
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
                                enter_transformed: false,
                                under_your_control: false,
                                enter_tapped: false,
                                enters_attacking: false,
                                up_to: false,
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
                                enter_transformed: false,
                                under_your_control: false,
                                enter_tapped: false,
                                enters_attacking: false,
                                up_to: false,
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
                            enter_transformed: false,
                            under_your_control: false,
                            enter_tapped: false,
                            enters_attacking: false,
                            up_to: false,
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
                                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
                            ),
                            owner_library: false,
                            enter_transformed: false,
                            under_your_control: false,
                            enter_tapped: false,
                            enters_attacking: false,
                            up_to: false,
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
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
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
    fn stack_zone_spell_cast_trigger_fires_from_stack() {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sage".to_string(),
            Zone::Stack,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Creature);
            spell.keywords.push(Keyword::Flying);
            let mut trigger = make_trigger(TriggerMode::SpellCast);
            trigger.valid_card = Some(TargetFilter::SelfRef);
            trigger.trigger_zones = vec![Zone::Stack];
            trigger.condition = Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn { filter: None },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            });
            trigger.execute = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            )));
            spell.trigger_definitions.push(trigger);
        }
        state.stack.push(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    vec![],
                    spell_id,
                    PlayerId(0),
                ),
                casting_variant: crate::types::game_state::CastingVariant::Normal,
            },
        });
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            vec![
                SpellCastRecord {
                    core_types: vec![CoreType::Instant],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![ManaColor::Blue],
                    mana_value: 1,
                },
                SpellCastRecord {
                    core_types: vec![CoreType::Creature],
                    supertypes: vec![],
                    subtypes: vec!["Bird".to_string()],
                    keywords: vec![Keyword::Flying],
                    colors: vec![ManaColor::Blue],
                    mana_value: 3,
                },
            ],
        );

        let events = vec![GameEvent::SpellCast {
            card_id: CardId(1),
            controller: PlayerId(0),
            object_id: spell_id,
        }];

        process_triggers(&mut state, &events);

        assert_eq!(state.stack.len(), 2);
        assert!(matches!(
            state.stack.last().map(|entry| &entry.kind),
            Some(StackEntryKind::TriggeredAbility { .. })
        ));
    }

    #[test]
    fn enters_trigger_matches_lowercase_with_keyword_filter() {
        let mut state = setup();
        let momo = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Momo".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&momo).unwrap();
            source.card_types.core_types.push(CoreType::Creature);
            source.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ))
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![
                                crate::types::ability::FilterProp::Another,
                                crate::types::ability::FilterProp::WithKeyword {
                                    value: Keyword::Flying,
                                },
                            ]),
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        let bird = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bird".to_string(),
            Zone::Battlefield,
        );
        {
            let creature = state.objects.get_mut(&bird).unwrap();
            creature.card_types.core_types.push(CoreType::Creature);
            creature.keywords.push(Keyword::Flying);
        }

        let events = vec![GameEvent::ZoneChanged {
            object_id: bird,
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        assert_eq!(state.stack.len(), 1);
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
                                    TypedFilter::permanent()
                                        .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
                                ),
                                count: None,
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
                                    enter_transformed: false,
                                    under_your_control: false,
                                    enter_tapped: false,
                                    enters_attacking: false,
                                    up_to: false,
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

    // ── Ward trigger tests ──────────────────────────────────────────────

    #[test]
    fn ward_trigger_fires_on_opponent_targeting() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create a creature with Ward {2} controlled by player 0
        let creature = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Ward Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.keywords.push(Keyword::Ward(WardCost::Mana(
                crate::types::mana::ManaCost::generic(2),
            )));
        }

        // Put an opponent spell on the stack targeting the creature
        let spell = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Opponent Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push(crate::types::game_state::StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(20),
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    spell,
                    PlayerId(1),
                ),
                casting_variant: crate::types::game_state::CastingVariant::Normal,
            },
        });

        // Fire BecomesTarget event
        let events = vec![GameEvent::BecomesTarget {
            object_id: creature,
            source_id: spell,
        }];

        process_triggers(&mut state, &events);

        // Ward trigger should be on the stack
        assert_eq!(
            state.stack.len(),
            2,
            "Ward trigger should be added to stack"
        );
        let ward_entry = &state.stack[1];
        assert_eq!(ward_entry.source_id, creature);
        match &ward_entry.kind {
            crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } => {
                assert!(matches!(
                    ability.effect,
                    Effect::Counter {
                        ref unless_payment,
                        ..
                    } if unless_payment.is_some()
                ));
            }
            _ => panic!("Expected TriggeredAbility on stack"),
        }
    }

    #[test]
    fn ward_trigger_does_not_fire_on_own_targeting() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let creature = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Ward Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.keywords.push(Keyword::Ward(WardCost::Mana(
                crate::types::mana::ManaCost::generic(2),
            )));
        }

        // Own spell targeting the creature
        let spell = create_object(
            &mut state,
            CardId(20),
            PlayerId(0), // Same controller!
            "Own Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push(crate::types::game_state::StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(20),
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    spell,
                    PlayerId(0),
                ),
                casting_variant: crate::types::game_state::CastingVariant::Normal,
            },
        });

        let events = vec![GameEvent::BecomesTarget {
            object_id: creature,
            source_id: spell,
        }];

        process_triggers(&mut state, &events);

        // No ward trigger — own spells don't trigger ward
        assert_eq!(
            state.stack.len(),
            1,
            "No ward trigger should fire for own spells"
        );
    }

    #[test]
    fn ward_trigger_does_not_fire_without_ward() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Creature WITHOUT ward
        let creature = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Normal Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
        }

        let spell = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Opponent Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push(crate::types::game_state::StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(20),
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    spell,
                    PlayerId(1),
                ),
                casting_variant: crate::types::game_state::CastingVariant::Normal,
            },
        });

        let events = vec![GameEvent::BecomesTarget {
            object_id: creature,
            source_id: spell,
        }];

        process_triggers(&mut state, &events);

        assert_eq!(state.stack.len(), 1, "No ward trigger without ward keyword");
    }

    #[test]
    fn multiple_ward_instances_fire_independently() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let creature = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Double Ward Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            // Two ward instances
            obj.keywords.push(Keyword::Ward(WardCost::Mana(
                crate::types::mana::ManaCost::generic(1),
            )));
            obj.keywords.push(Keyword::Ward(WardCost::PayLife(2)));
        }

        let spell = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Opponent Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push(crate::types::game_state::StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(20),
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    spell,
                    PlayerId(1),
                ),
                casting_variant: crate::types::game_state::CastingVariant::Normal,
            },
        });

        let events = vec![GameEvent::BecomesTarget {
            object_id: creature,
            source_id: spell,
        }];

        process_triggers(&mut state, &events);

        // Two ward triggers + original spell = 3
        assert_eq!(
            state.stack.len(),
            3,
            "Two ward triggers should fire independently"
        );
    }

    #[test]
    fn ward_cost_to_unless_cost_all_variants() {
        use crate::types::keywords::WardCost;
        use crate::types::mana::ManaCost;

        // Mana cost
        let mana = WardCost::Mana(ManaCost::generic(3));
        let result = ward_cost_to_unless_cost(&mana);
        assert!(matches!(result, UnlessCost::Fixed { cost } if cost == ManaCost::generic(3)));

        // Pay life
        let life = WardCost::PayLife(2);
        let result = ward_cost_to_unless_cost(&life);
        assert!(matches!(result, UnlessCost::PayLife { amount: 2 }));

        // Discard
        let discard = WardCost::DiscardCard;
        let result = ward_cost_to_unless_cost(&discard);
        assert!(matches!(result, UnlessCost::DiscardCard));

        // Sacrifice
        let sacrifice = WardCost::Sacrifice {
            count: 1,
            filter: TargetFilter::Any,
        };
        let result = ward_cost_to_unless_cost(&sacrifice);
        assert!(matches!(result, UnlessCost::Sacrifice { count: 1, .. }));

        // Waterbend
        let waterbend = WardCost::Waterbend(ManaCost::generic(4));
        let result = ward_cost_to_unless_cost(&waterbend);
        assert!(matches!(result, UnlessCost::Fixed { cost } if cost == ManaCost::generic(4)));
    }

    #[test]
    fn nth_draw_constraint_uses_drawing_player_not_controller() {
        let mut state = setup();
        // Player 1 (opponent) has drawn 2 cards this turn
        state.players[1].cards_drawn_this_turn = 2;
        // Player 0 (controller) has drawn 0 cards
        state.players[0].cards_drawn_this_turn = 0;

        let mut trig_def = make_trigger(TriggerMode::Drawn);
        trig_def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n: 2 });

        let controller = PlayerId(0);
        let event = GameEvent::CardDrawn {
            player_id: PlayerId(1),
            object_id: ObjectId(99),
        };

        // Should fire: opponent (player 1) drew their 2nd card
        assert!(check_trigger_constraint(
            &state,
            &trig_def,
            ObjectId(1),
            0,
            controller,
            &event,
        ));

        // Should NOT fire when controller drew (count is 0)
        let controller_draw = GameEvent::CardDrawn {
            player_id: PlayerId(0),
            object_id: ObjectId(100),
        };
        assert!(!check_trigger_constraint(
            &state,
            &trig_def,
            ObjectId(1),
            0,
            controller,
            &controller_draw,
        ));
    }

    #[test]
    fn test_dealt_damage_by_source_condition() {
        use crate::types::game_state::DamageRecord;

        let mut state = setup();
        let source = ObjectId(10); // The permanent with the trigger
        let dying_creature = ObjectId(20); // The creature that died

        // Record damage: source dealt 3 damage to dying_creature
        state.damage_dealt_this_turn.push(DamageRecord {
            source_id: source,
            target: TargetRef::Object(dying_creature),
            amount: 3,
            is_combat: false,
        });

        let condition = TriggerCondition::DealtDamageBySourceThisTurn;
        let event = GameEvent::CreatureDestroyed {
            object_id: dying_creature,
        };

        // Matching source + matching dying creature → true
        assert!(check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(source),
            Some(&event),
        ));

        // Non-matching source → false
        let wrong_source = ObjectId(99);
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(wrong_source),
            Some(&event),
        ));

        // Non-matching dying creature → false
        let wrong_event = GameEvent::CreatureDestroyed {
            object_id: ObjectId(88),
        };
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(source),
            Some(&wrong_event),
        ));

        // No trigger event → false
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(source),
            None,
        ));
    }

    #[test]
    fn test_damage_dealt_this_turn_cleared_on_turn() {
        use crate::types::game_state::DamageRecord;

        let mut state = setup();
        state.damage_dealt_this_turn.push(DamageRecord {
            source_id: ObjectId(1),
            target: TargetRef::Object(ObjectId(2)),
            amount: 2,
            is_combat: true,
        });
        assert!(!state.damage_dealt_this_turn.is_empty());

        // Call the actual turn-start function to verify the real code path clears it
        let mut events = Vec::new();
        crate::game::turns::start_next_turn(&mut state, &mut events);
        assert!(state.damage_dealt_this_turn.is_empty());
    }

    // === CR 603.10a: Leaves-the-battlefield trigger LKI tests ===

    #[test]
    fn dies_trigger_fires_after_sacrifice_as_cost() {
        // CR 603.10a: "When this creature dies" triggers should fire even when the
        // creature was sacrificed as a cost (already in graveyard when triggers check).

        let mut state = setup();
        state.turn_number = 3;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;

        // Create a creature with a "dies" trigger (like Haywire Mite)
        let mite_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Haywire Mite".to_string(),
            Zone::Graveyard, // Already in graveyard (sacrificed as cost)
        );
        {
            let mite = state.objects.get_mut(&mite_id).unwrap();
            mite.controller = PlayerId(0);
            mite.card_types.core_types.push(CoreType::Creature);
            mite.card_types.core_types.push(CoreType::Artifact);
            // Dies trigger: "When this creature dies, you gain 2 life"
            mite.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .valid_card(TargetFilter::SelfRef)
                    .origin(Zone::Battlefield)
                    .destination(Zone::Graveyard)
                    .trigger_zones(vec![Zone::Battlefield])
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 2 },
                            player: GainLifePlayer::Controller,
                        },
                    ))
                    .description("When this creature dies, you gain 2 life.".to_string()),
            );
        }

        // Simulate the ZoneChanged event from sacrifice
        let events = vec![GameEvent::ZoneChanged {
            object_id: mite_id,
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        }];

        process_triggers(&mut state, &events);

        // The dies trigger should have been pushed to the stack (GainLife has no targeting)
        assert!(
            !state.stack.is_empty(),
            "Dies trigger should fire via LKI even when creature is already in graveyard"
        );
        assert_eq!(state.stack.len(), 1);
        let entry = &state.stack[0];
        assert_eq!(entry.source_id, mite_id);
        if let crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } =
            &entry.kind
        {
            assert!(
                matches!(ability.effect, Effect::GainLife { .. }),
                "Triggered ability should be GainLife"
            );
        } else {
            panic!("Expected TriggeredAbility on stack");
        }
    }

    #[test]
    fn lki_trigger_does_not_fire_for_non_battlefield_origin() {
        // A creature in graveyard with a battlefield-zone trigger should NOT fire
        // for zone changes that aren't from the battlefield.
        let mut state = setup();
        state.turn_number = 3;
        state.active_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;

        let obj_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Test Card".to_string(),
            Zone::Exile, // In exile, not graveyard
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.controller = PlayerId(0);
            obj.card_types.core_types.push(CoreType::Creature);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .valid_card(TargetFilter::SelfRef)
                    .origin(Zone::Battlefield)
                    .destination(Zone::Graveyard)
                    .trigger_zones(vec![Zone::Battlefield]),
            );
        }

        // Event is from graveyard to exile, not from battlefield
        let events = vec![GameEvent::ZoneChanged {
            object_id: obj_id,
            from: Zone::Graveyard,
            to: Zone::Exile,
        }];

        process_triggers(&mut state, &events);
        assert!(
            state.stack.is_empty(),
            "Trigger should not fire for non-battlefield origin zone changes"
        );
    }

    // === extract_target_filter_from_effect private zone tests ===

    #[test]
    fn extract_target_skips_change_zone_from_hand() {
        // CR 115.1: "Put a land from your hand" doesn't target — selection at resolution.

        let effect = Effect::ChangeZone {
            origin: Some(Zone::Hand),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(
                TypedFilter::default()
                    .with_type(crate::types::ability::TypeFilter::Land)
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
            ),
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: true,
            enters_attacking: false,
            up_to: false,
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_none(),
            "ChangeZone from Hand should not extract a target (resolution-time selection)"
        );
    }

    #[test]
    fn extract_target_keeps_change_zone_from_battlefield() {
        // "Exile target creature" should still extract the target filter

        let effect = Effect::ChangeZone {
            origin: None,
            destination: Zone::Exile,
            target: TargetFilter::Typed(TypedFilter::creature()),
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_some(),
            "ChangeZone from battlefield should still extract target for stack-time targeting"
        );
    }
}
