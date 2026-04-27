use std::collections::HashSet;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChosenAttribute, ControllerRef, Effect, ModalChoice,
    PlayerFilter, ResolvedAbility, TargetFilter, TargetRef, TributeOutcome, TriggerCondition,
    TriggerDefinition, TypeFilter, TypedFilter, UnlessCost,
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
use crate::types::statics::{StaticMode, TriggerCause};
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::ability_utils::build_resolved_from_def;
use super::filter::{matches_target_filter, spell_record_matches_filter, FilterContext};
use super::game_object::GameObject;
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
    source_obj: &GameObject,
    timestamp: u32,
    zone_filter: Option<Zone>,
    batched_this_pass: &mut HashSet<(ObjectId, usize)>,
    registered_this_event: &mut HashSet<(ObjectId, usize)>,
) -> Vec<MatchedTrigger> {
    let mut pending = Vec::new();
    let obj_id = source_obj.id;
    let controller = source_obj.controller;
    // CR 702.26b + CR 114.4: `active_trigger_definitions` owns the phased-out /
    // command-zone gate. CR 603.4 intervening-if is still the two-point check
    // inside this function (condition block below) and at resolution.
    for (trig_idx, trig_def) in
        super::functioning_abilities::active_trigger_definitions(state, source_obj)
    {
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
        // CR 603.2 / CR 603.3: A single printed trigger definition fires at most
        // once per eligible event. Multiple zone-scan paths (battlefield,
        // leaves-battlefield last-known-information, and non-battlefield zones)
        // may all visit the same `(obj_id, trig_idx)` pair within a single event —
        // notably for Dies / leaves-battlefield triggers where the object is
        // simultaneously findable via the "look back" path (CR 603.10a) and the
        // graveyard scan (CR 113.6k). Per-event dedup ensures one registration
        // per physical printed trigger per event. Intra-call `trigger_events`
        // expansion (e.g., `matching_attack_events` for multi-attacker batches)
        // still produces multiple PendingTriggers below because the set is only
        // updated AFTER collection at the call site.
        if registered_this_event.contains(&(obj_id, trig_idx)) {
            continue;
        }
        if let Some(matcher) = trigger_matcher(trig_def.mode.clone()) {
            if !matcher(event, trig_def, obj_id, state) {
                continue;
            }
            let trigger_events =
                if matches!(trig_def.mode, TriggerMode::Attacks) && trig_def.condition.is_none() {
                    super::trigger_matchers::matching_attack_events(event, trig_def, obj_id, state)
                } else {
                    vec![event.clone()]
                };
            if !check_trigger_constraint(state, trig_def, obj_id, trig_idx, controller, event) {
                continue;
            }
            if let Some(ref condition) = trig_def.condition {
                if !check_trigger_condition(state, condition, controller, Some(obj_id), Some(event))
                {
                    continue;
                }
            }
            let mut ability = build_triggered_ability(state, trig_def, obj_id, controller);
            // CR 603.4: Stamp the printed-trigger index so per-turn resolution
            // tracking (`AbilityCondition::NthResolutionThisTurn`) can identify
            // "this ability" at resolution time.
            ability.ability_index = Some(trig_idx);
            let (modal, mode_abilities) = trig_def
                .execute
                .as_ref()
                .map(|exec| (exec.modal.clone(), exec.mode_abilities.clone()))
                .unwrap_or_default();
            for trigger_event in trigger_events {
                pending.push(MatchedTrigger {
                    trig_idx,
                    pending: PendingTrigger {
                        source_id: obj_id,
                        controller,
                        condition: trig_def.condition.clone(),
                        ability: ability.clone(),
                        timestamp,
                        target_constraints: Vec::new(),
                        trigger_event: Some(trigger_event),
                        modal: modal.clone(),
                        mode_abilities: mode_abilities.clone(),
                        description: trig_def.description.clone(),
                    },
                    batched: trig_def.batched,
                    constraint: trig_def.constraint.clone(),
                });
            }
        }
    }
    pending
}

fn trigger_source_ids_for_zone(state: &GameState, zone: Zone) -> Vec<ObjectId> {
    match zone {
        // CR 702.26b: Phased-out permanents don't trigger.
        Zone::Battlefield => state.battlefield_phased_in_ids(),
        Zone::Graveyard => state
            .players
            .iter()
            .flat_map(|player| player.graveyard.iter().copied())
            .collect(),
        Zone::Exile => state.exile.iter().copied().collect(),
        Zone::Stack => state
            .stack
            .iter()
            .filter_map(|entry| match &entry.kind {
                StackEntryKind::Spell { .. } => Some(entry.id),
                // CR 111.1b + CR 113.3b: Activated/triggered ability stack entries
                // (including KeywordAction) are abilities, not objects.
                StackEntryKind::ActivatedAbility { .. }
                | StackEntryKind::TriggeredAbility { .. }
                | StackEntryKind::KeywordAction { .. } => None,
            })
            .collect(),
        // CR 114.4: Abilities of emblems function in the command zone.
        // Non-emblem command-zone objects (commanders before casting, etc.)
        // do NOT have their abilities function per CR 114.4, so this filter
        // is the single authority — mirrored by `object_functions` in
        // `functioning_abilities`.
        Zone::Command => state
            .command_zone
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.is_emblem && !o.is_phased_out())
            })
            .collect(),
        Zone::Hand | Zone::Library => Vec::new(),
    }
}

/// CR 603.2g + CR 603.6a + CR 700.4: Check whether an event's trigger-firing
/// should be suppressed by any active `SuppressTriggers` static on the battlefield.
///
/// Only matches ZoneChanged events that correspond to ETB (to=Battlefield) or Dies
/// (from=Battlefield, to=Graveyard). The suppression tests the event's *subject*
/// (the entering/dying permanent) against the static's `source_filter`, matching
/// official Torpor Orb rulings: a creature entering suppresses every ETB trigger
/// in response — including observer triggers on other permanents.
///
/// CR 603.10a: Filter evaluation uses the event's `ZoneChangeRecord`
/// (last-known-information snapshot) rather than live `state.objects` — for Dies
/// events the subject has already left the battlefield and its live type data may
/// no longer reflect the pre-change state.
///
/// Replacement effects (CR 614) are unaffected — they run in a different phase.
/// Static "enters with" / "enters tapped" / "as X enters" effects (CR 603.6d) are
/// also unaffected because they are static abilities, not triggered ones.
fn event_is_suppressed_by_static_triggers(state: &GameState, event: &GameEvent) -> bool {
    use crate::types::statics::SuppressedTriggerEvent;

    // Classify the event: is it ETB, Dies, or neither?
    let (record, triggered_event) = match event {
        GameEvent::ZoneChanged {
            record,
            to: Zone::Battlefield,
            ..
        } => (record.as_ref(), SuppressedTriggerEvent::EntersBattlefield),
        GameEvent::ZoneChanged {
            record,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            ..
        } => (record.as_ref(), SuppressedTriggerEvent::Dies),
        _ => return false,
    };

    // CR 702.26b + CR 604.1: `battlefield_active_statics` owns the phased-out /
    // command-zone / condition gate so Torpor Orb phased out no longer silently
    // suppresses ETB triggers.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::SuppressTriggers {
            ref source_filter,
            ref events,
        } = def.mode
        else {
            continue;
        };
        if !events.contains(&triggered_event) {
            continue;
        }
        // CR 603.10a: Zone-change last-known information — use the record snapshot.
        let filter_ctx = super::filter::FilterContext::from_source(state, bf_obj.id);
        if super::filter::matches_target_filter_on_zone_change_record(
            state,
            record,
            source_filter,
            &filter_ctx,
        ) {
            return true;
        }
    }
    false
}

/// Process events and place triggered abilities on the stack in APNAP order.
/// CR 603.3b: Process triggered abilities waiting to be put on the stack.
pub fn process_triggers(state: &mut GameState, events: &[GameEvent]) {
    // CR 603.6a + CR 611.2e: Continuous effects (including statics that grant
    // triggered abilities to a class — sliver-lord pattern) apply the moment
    // the affected permanent is on the battlefield. The newcomers must be
    // checked for ETB triggers including any granted by their own static
    // abilities (Harmonic Sliver) and by other lords already on the
    // battlefield. Flushing pending layer evaluation here guarantees
    // `obj.trigger_definitions` and `obj.keywords` reflect all active
    // continuous effects before this pass scans for matching triggers.
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }
    let mut pending: Vec<PendingTrigger> = Vec::new();
    // CR 603.2c: Track which batched triggers (source_id, trig_idx) have already
    // fired in this pass so "one or more" triggers fire at most once per batch.
    let mut batched_this_pass: HashSet<(ObjectId, usize)> = HashSet::new();

    for event in events {
        // CR 603.2 / CR 603.3: Per-event dedup. A single printed trigger definition
        // fires at most once per eligible event, even if multiple scan paths
        // (battlefield, leaves-battlefield last-known-information, graveyard/exile/stack)
        // all reach the same `(obj_id, trig_idx)` pair for this event. Cleared
        // between events so each distinct event can still fire the trigger.
        let mut registered_this_event: HashSet<(ObjectId, usize)> = HashSet::new();
        // CR 603.2g + CR 603.6a + CR 700.4: If a SuppressTriggers static matches the
        // subject of an ETB/Dies event, skip all trigger matching for that event —
        // per CR 603.2g, an event that "won't trigger anything" because the static
        // declares its trigger registration void. Torpor Orb stops every ETB trigger
        // caused by a creature entering, including observer triggers like Soul Warden.
        // CR 603.6d: Static "enters tapped"/"enters with counters"/"as X enters"
        // effects are NOT triggered and are unaffected (they run as part of the ETB
        // event itself, not through process_triggers).
        if event_is_suppressed_by_static_triggers(state, event) {
            continue;
        }
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
                        obj,
                        obj.entered_battlefield_turn.unwrap_or(0),
                        Some(Zone::Battlefield),
                        &mut batched_this_pass,
                        &mut registered_this_event,
                    ),
                )
            };

            for matched in matched_triggers {
                record_trigger_fired(state, matched.constraint.as_ref(), obj_id, matched.trig_idx);
                if matched.batched {
                    batched_this_pass.insert((obj_id, matched.trig_idx));
                }
                registered_this_event.insert((obj_id, matched.trig_idx));
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
                                contribution: crate::types::ability::ManaContribution::Base,
                            },
                            restrictions: vec![],
                            grants: vec![],
                            expiry: Some(crate::types::mana::ManaExpiry::EndOfCombat),
                            target: None,
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
            from: Some(Zone::Battlefield),
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
                        obj,
                        obj.entered_battlefield_turn.unwrap_or(0),
                        Some(Zone::Battlefield),
                        &mut batched_this_pass,
                        &mut registered_this_event,
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
                    registered_this_event.insert((*moved_id, matched.trig_idx));
                    pending.push(matched.pending);
                }
            }
        }

        // CR 113.6k + CR 114.4: Non-battlefield trigger zones are opt-in via
        // `trigger_zones`. CR 114.4: abilities of emblems function in the
        // command zone, so emblem-hosted triggers whose `trigger_zones`
        // include `Zone::Command` are picked up here. Non-emblem command-
        // zone objects (commanders pre-cast) are filtered out by
        // `trigger_source_ids_for_zone(Zone::Command)` which applies the
        // `is_emblem` gate. Synthetic battlefield-only keyword triggers
        // (prowess / ward / firebending / exploit) deliberately do NOT run
        // in this loop — emblems have no keywords.
        for zone in [Zone::Graveyard, Zone::Exile, Zone::Stack, Zone::Command] {
            for obj_id in trigger_source_ids_for_zone(state, zone) {
                let matched_triggers = {
                    let obj = match state.objects.get(&obj_id) {
                        Some(o) => o,
                        None => continue,
                    };
                    collect_matching_triggers(
                        state,
                        event,
                        obj,
                        0,
                        Some(zone),
                        &mut batched_this_pass,
                        &mut registered_this_event,
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
                    registered_this_event.insert((obj_id, matched.trig_idx));
                    pending.push(matched.pending);
                }
            }
        }

        // CR 702.85a + CR 702.85c: Cascade — synthesized keyword trigger off
        // the just-cast spell. Unlike Prowess (battlefield-sourced, handled
        // inside the battlefield loop above), cascade's source IS the cast
        // object on the SpellCast event, so we read it directly rather than
        // scanning every stack object. Each Cascade keyword instance triggers
        // separately (CR 702.85c).
        //
        // CR 603.3b: APNAP ordering across triggers needs distinct timestamps
        // even when multiple cascade instances fire from one spell — using
        // `state.next_timestamp()` per instance gives a stable, monotonically
        // increasing order matching how every other timestamp in the engine
        // is allocated.
        if let GameEvent::SpellCast {
            object_id: cast_obj_id,
            ..
        } = event
        {
            let (instance_count, controller) = state
                .objects
                .get(cast_obj_id)
                .map(|obj| {
                    let n = obj
                        .keywords
                        .iter()
                        .filter(|k| matches!(k, Keyword::Cascade))
                        .count();
                    (n, obj.controller)
                })
                .unwrap_or((0, PlayerId(0)));
            for _ in 0..instance_count {
                // CR 702.85a: Cascade fires only when "you cast this spell" —
                // wire `WasCast` as the trigger condition so a future refactor
                // that routes synthesized triggers through `check_trigger_condition`
                // still gates the firing correctly (belt-and-suspenders alongside
                // the SpellCast event itself).
                let cascade_trig_def = TriggerDefinition::new(TriggerMode::SpellCast)
                    .description("Cascade".to_string())
                    .condition(TriggerCondition::WasCast);
                let cascade_ability =
                    ResolvedAbility::new(Effect::Cascade, Vec::new(), *cast_obj_id, controller);
                let timestamp = state.next_timestamp() as u32;
                pending.push(PendingTrigger {
                    source_id: *cast_obj_id,
                    controller,
                    condition: cascade_trig_def.condition,
                    ability: cascade_ability,
                    timestamp,
                    target_constraints: Vec::new(),
                    trigger_event: Some(event.clone()),
                    modal: None,
                    mode_abilities: vec![],
                    description: cascade_trig_def.description,
                });
            }
        }

        // CR 725.2: At the beginning of the monarch's end step, that player draws a card.
        // Synthetic game-rule trigger — not attached to any permanent.
        if let GameEvent::PhaseChanged { phase: Phase::End } = event {
            if let Some(monarch_id) = state.monarch {
                if monarch_id == state.active_player {
                    let draw_effect = Effect::Draw {
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                        up_to: false,
                    };
                    let draw_ability =
                        ResolvedAbility::new(draw_effect, Vec::new(), ObjectId(0), monarch_id);
                    let trig_def = TriggerDefinition::new(TriggerMode::Phase)
                        .description("Monarch draw (CR 725.2)".to_string());
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

        // CR 725.2: When a creature deals combat damage to the monarch, its controller
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
                            .description("Monarch steal (CR 725.2)".to_string());
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
            // CR 605.1b: Triggered mana abilities don't use the stack — they resolve
            // immediately at the moment the trigger event occurs. Classify via the
            // single-authority `is_triggered_mana_ability` (ResolvedAbility form),
            // which enforces all three CR 605.1b criteria.
            if super::mana_abilities::is_triggered_mana_ability(
                &trigger.ability,
                trigger.trigger_event.as_ref(),
            ) {
                super::mana_abilities::resolve_triggered_mana_ability_inline(
                    state,
                    &trigger.ability,
                    &mut events_out,
                );
                continue;
            }
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

    // Clear transient cast_from_zone and the cast-tally booleans/color breakdown
    // on all objects after trigger collection. These fields only need to survive
    // long enough for ETB trigger detection (CR 603.4). `mana_spent_to_cast_amount`
    // is intentionally NOT cleared: it is a historical fact about the object
    // (how much mana was spent to cast it) used by spell resolution effects
    // like "deals damage equal to the amount of mana spent to cast this spell"
    // (Molten Note) and by CR 603.4 intervening-if resolution re-checks
    // (Hungry Graffalon / Topiary Lecturer Increment). The field is initialized
    // to 0 by `GameObject::new` and set at cast finalization in
    // `casting::pay_mana_cost`; it never needs to be reset.
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        obj.cast_from_zone = None;
        obj.mana_spent_to_cast = false;
        obj.colors_spent_to_cast = crate::types::mana::ColoredManaCount::default();
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
            ability: Box::new(trigger.ability),
            condition: trigger.condition,
            trigger_event: trigger.trigger_event,
            description: trigger.description,
        },
    };
    stack::push_to_stack(state, entry, events);
}

/// CR 603.2d: Apply trigger doubling from `StaticMode::DoubleTriggers`
/// static abilities. Scans battlefield for permanents with a DoubleTriggers
/// static, then clones matching pending triggers an additional time. The
/// `TriggerCause` predicate restricts which spawning events qualify
/// (Panharmonicon: ETB; Isshin: creature attacking; Any: unrestricted).
fn apply_trigger_doubling(state: &GameState, pending: &mut Vec<PendingTrigger>) {
    // CR 702.26b + CR 604.1: `active_static_definitions` owns the gating so a
    // phased-out doubler no longer doubles triggers.
    let doublers: Vec<(PlayerId, ObjectId, TriggerCause, Option<TargetFilter>)> = state
        .battlefield
        .iter()
        .filter_map(|&obj_id| {
            let obj = state.objects.get(&obj_id)?;
            let doubler = super::functioning_abilities::active_static_definitions(state, obj)
                .find(|sd| matches!(sd.mode, StaticMode::DoubleTriggers { .. }))?;
            let cause = match &doubler.mode {
                StaticMode::DoubleTriggers { cause } => cause.clone(),
                _ => unreachable!("filter above guarantees DoubleTriggers"),
            };
            Some((obj.controller, obj_id, cause, doubler.affected.clone()))
        })
        .collect();

    if doublers.is_empty() {
        return;
    }

    let mut extra: Vec<PendingTrigger> = Vec::new();
    for (doubler_controller, doubler_id, cause, ref affected) in &doublers {
        for trigger in pending.iter() {
            // Controller match: trigger source must be controlled by the doubler's controller
            if trigger.controller != *doubler_controller {
                continue;
            }
            // Self-exclusion: don't double triggers from the doubler itself entering
            if trigger.source_id == *doubler_id {
                continue;
            }
            // CR 603.2d: Check the cause predicate against the spawning event.
            if !trigger_cause_matches(cause, trigger.trigger_event.as_ref()) {
                continue;
            }
            // CR 603.2d: If the doubler specifies an affected filter (e.g. "creature you
            // control of the chosen type"), only double triggers from matching sources.
            if let Some(filter) = affected {
                if !matches_target_filter(
                    state,
                    trigger.source_id,
                    filter,
                    &FilterContext::from_source(state, *doubler_id),
                ) {
                    continue;
                }
            }
            extra.push(trigger.clone());
        }
    }
    pending.extend(extra);
}

/// CR 603.2d: Predicate check — does a `TriggerCause` match the event that
/// spawned a pending trigger? Called once per (doubler, pending-trigger) pair.
///
/// - `TriggerCause::Any` matches any event (even absent events — some state
///   triggers carry `trigger_event = None`, and unrestricted doublers should
///   still cover them).
/// - `TriggerCause::EntersBattlefield { core_types }` matches `ZoneChanged`
///   events moving to the battlefield whose object's core types intersect
///   the predicate's `core_types`. An empty `core_types` list means "any
///   permanent" (reserved for hypothetical cards that don't narrow by type).
/// - `TriggerCause::CreatureAttacking` matches `AttackersDeclared` events.
///   CR 508.1a: every object declared as an attacker must be a creature,
///   so no further type check is required.
fn trigger_cause_matches(cause: &TriggerCause, event: Option<&GameEvent>) -> bool {
    match cause {
        TriggerCause::Any => true,
        TriggerCause::EntersBattlefield { core_types } => {
            let Some(GameEvent::ZoneChanged {
                to: Zone::Battlefield,
                record,
                ..
            }) = event
            else {
                return false;
            };
            if core_types.is_empty() {
                return true;
            }
            // CR 603.6a: The entering permanent's core types must include at
            // least one of the predicate's listed types. Panharmonicon uses
            // `[Artifact, Creature]` — either type qualifies.
            record.core_types.iter().any(|ct| core_types.contains(ct))
        }
        TriggerCause::CreatureAttacking => {
            matches!(event, Some(GameEvent::AttackersDeclared { .. }))
        }
        TriggerCause::CreatureDying => {
            // CR 603.6c + CR 700.4: "Dies" means battlefield → graveyard. Use
            // the pre-move snapshot in `record` because the object is no
            // longer on the battlefield when the trigger fires.
            let Some(GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                to: Zone::Graveyard,
                record,
                ..
            }) = event
            else {
                return false;
            };
            record.core_types.contains(&CoreType::Creature)
        }
    }
}

/// CR 603.8: Check state triggers for all permanents on the battlefield.
/// State triggers fire when a game-state condition is true, rather than in response
/// to events. A state trigger doesn't trigger again until its ability has resolved,
/// been countered, or otherwise left the stack.
///
/// CR 702.26b: Phased-out permanents are treated as though they don't exist
/// — their state triggers don't fire.
pub fn check_state_triggers(state: &mut GameState) {
    // CR 702.26b: phased-out gating is owned by `active_trigger_definitions`
    // below; we iterate the full battlefield and let the helper drop phased-
    // out permanents rather than re-filtering here.
    let source_ids: Vec<ObjectId> = state.battlefield.iter().copied().collect();

    let mut pending: Vec<PendingTrigger> = Vec::new();

    for obj_id in source_ids {
        // CR 702.26b + CR 114.4: `active_trigger_definitions` owns the
        // phased-out / command-zone gate. We clone the yielded triggers to a
        // local Vec so the mutable-state pass below (push_pending_trigger_to_stack)
        // doesn't collide with the shared borrow on `state.objects`.
        let (controller, timestamp, trigger_defs): (PlayerId, u32, Vec<TriggerDefinition>) = {
            let Some(obj) = state.objects.get(&obj_id) else {
                continue;
            };
            if obj.zone != Zone::Battlefield {
                continue;
            }
            (
                obj.controller,
                obj.entered_battlefield_turn.unwrap_or(0),
                super::functioning_abilities::active_trigger_definitions(state, obj)
                    .map(|(_, def)| def.clone())
                    .collect(),
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
    let mut to_fire: Vec<(DelayedTrigger, Option<GameEvent>)> = Vec::new();
    let mut to_remove: Vec<(usize, GameEvent)> = Vec::new();

    for (idx, delayed) in state.delayed_triggers.iter().enumerate() {
        if let Some(trigger_event) = delayed_trigger_event(
            &delayed.condition,
            events,
            state,
            delayed.source_id,
            delayed.controller,
        ) {
            if delayed.one_shot {
                to_remove.push((idx, trigger_event));
            } else {
                to_fire.push((delayed.clone(), Some(trigger_event)));
            }
        }
    }

    // Remove one-shot triggers in reverse order to preserve indices, collecting into to_fire
    for (idx, trigger_event) in to_remove.into_iter().rev() {
        let trigger = state.delayed_triggers.remove(idx);
        to_fire.push((trigger, Some(trigger_event)));
    }

    if to_fire.is_empty() {
        return vec![];
    }

    let mut new_events = Vec::new();

    // CR 603.3b: APNAP ordering — active player's triggers go on stack last (resolve first).
    // Sort so NAP triggers come first (pushed to stack bottom), AP triggers last (stack top).
    to_fire.sort_by_key(|(trigger, _)| {
        let is_nap = if trigger.controller == state.active_player {
            0
        } else {
            1
        };
        (is_nap, state.turn_number)
    });
    to_fire.reverse();

    for (trigger, trigger_event) in to_fire {
        let pending = PendingTrigger {
            source_id: trigger.source_id,
            controller: trigger.controller,
            condition: None,
            ability: trigger.ability,
            timestamp: state.turn_number,
            target_constraints: Vec::new(),
            trigger_event,
            modal: None,
            mode_abilities: vec![],
            description: None,
        };
        push_pending_trigger_to_stack(state, pending, &mut new_events);
    }

    new_events
}

/// CR 603.7: Check if a delayed trigger condition is met by recent events.
fn delayed_trigger_event(
    condition: &crate::types::ability::DelayedTriggerCondition,
    events: &[GameEvent],
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
) -> Option<GameEvent> {
    use crate::types::ability::DelayedTriggerCondition;

    match condition {
        DelayedTriggerCondition::AtNextPhase { phase } => events
            .iter()
            .find(|e| matches!(e, GameEvent::PhaseChanged { phase: p } if p == phase))
            .cloned(),
        DelayedTriggerCondition::AtNextPhaseForPlayer { phase, player } => {
            if state.active_player != *player {
                return None;
            }
            events
                .iter()
                .find(|e| matches!(e, GameEvent::PhaseChanged { phase: p } if p == phase))
                .cloned()
        }
        DelayedTriggerCondition::WhenLeavesPlay { object_id } => events
            .iter()
            .find(|e| {
                matches!(e,
                    GameEvent::ZoneChanged { object_id: id, from: Some(Zone::Battlefield), .. }
                    if *id == *object_id
                )
            })
            .cloned(),
        // CR 603.7c: "when [object] dies" — zone change to graveyard from battlefield
        DelayedTriggerCondition::WhenDies { .. } => events
            .iter()
            .find(|e| {
                matches!(
                    e,
                    GameEvent::ZoneChanged {
                        from: Some(Zone::Battlefield),
                        to: Zone::Graveyard,
                        ..
                    }
                )
            })
            .cloned(),
        // CR 603.7c: "when [object] leaves the battlefield" — any zone change from battlefield
        DelayedTriggerCondition::WhenLeavesPlayFiltered { .. } => events
            .iter()
            .find(|e| {
                matches!(
                    e,
                    GameEvent::ZoneChanged {
                        from: Some(Zone::Battlefield),
                        ..
                    }
                )
            })
            .cloned(),
        // CR 603.7c: "when [object] enters the battlefield" — zone change to battlefield
        DelayedTriggerCondition::WhenEntersBattlefield { .. } => events
            .iter()
            .find(|e| {
                matches!(
                    e,
                    GameEvent::ZoneChanged {
                        to: Zone::Battlefield,
                        ..
                    }
                )
            })
            .cloned(),
        // "when [object] dies or is exiled" — zone change to graveyard OR exile from battlefield.
        DelayedTriggerCondition::WhenDiesOrExiled { filter } => events
            .iter()
            .find(|e| {
                matches!(
                    e,
                    GameEvent::ZoneChanged {
                        from: Some(Zone::Battlefield),
                        to: Zone::Graveyard | Zone::Exile,
                        ..
                    }
                ) && matches!(
                    e,
                    GameEvent::ZoneChanged { object_id, .. }
                        if crate::game::filter::matches_target_filter(
                            state,
                            *object_id,
                            filter,
                            &FilterContext::from_source_with_controller(source_id, controller),
                        )
                )
            })
            .cloned(),
        // CR 603.7c: "Whenever [event] this turn" — delegate to trigger matcher registry.
        DelayedTriggerCondition::WheneverEvent { trigger }
        | DelayedTriggerCondition::WhenNextEvent { trigger } => {
            if let Some(matcher) = super::trigger_matchers::trigger_matcher(trigger.mode.clone()) {
                events
                    .iter()
                    .find(|event| matcher(event, trigger, source_id, state))
                    .cloned()
            } else {
                None
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
        // CR 505.1: Main phases are precombat and postcombat.
        TriggerConstraint::OnlyDuringYourMainPhase => {
            state.active_player == controller
                && matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
        }
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
                        .filter(|record| {
                            spell_record_matches_filter(
                                record,
                                filter,
                                caster,
                                &state.all_creature_types,
                            )
                        })
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
        // CR 508.1 + CR 603.2c: Count attackers in the triggering AttackersDeclared
        // batch whose controller matches `scope` relative to the trigger controller.
        TriggerCondition::AttackersDeclaredMin { scope, minimum } => {
            let Some(GameEvent::AttackersDeclared { attacker_ids, .. }) = trigger_event else {
                return false;
            };
            let count = attacker_ids
                .iter()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| match scope {
                        ControllerRef::You => obj.controller == controller,
                        ControllerRef::Opponent => obj.controller != controller,
                        // Other ControllerRef variants are not used by the attacks-with-N
                        // combinator; treat as permissive to avoid silently dropping matches.
                        _ => true,
                    })
                })
                .count();
            count >= *minimum as usize
        }
        // CR 506.2 + CR 508.1b + CR 603.4: "if none of those creatures attacked you" —
        // Iterate the attack batch's per-attacker targets; fail the condition if any
        // attacker controlled by a player other than the trigger controller targeted
        // the trigger controller directly (CR 506.2: the defending player).
        TriggerCondition::NoneOfAttackersTargetedYou => {
            let Some(GameEvent::AttackersDeclared { attacks, .. }) = trigger_event else {
                return false;
            };
            !attacks.iter().any(|(attacker_id, target)| {
                let attacker_is_other = state
                    .objects
                    .get(attacker_id)
                    .is_some_and(|obj| obj.controller != controller);
                attacker_is_other
                    && matches!(
                        target,
                        crate::game::combat::AttackTarget::Player(p) if *p == controller
                    )
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
        // CR 601.2: "if you cast it" — true when the entering/affected object was
        // cast as a spell (regardless of origin zone). For ETB-based triggers like
        // Light-Paws, Emperor's Voice ("Whenever an Aura you control enters, if you
        // cast it..."), the trigger source is the permanent with the ability, not the
        // entering Aura — so we must check the entering object from the trigger event,
        // falling back to source_id for self-referential cases (Cascade's SpellCast
        // event, Discover ETBs where source == cast spell).
        TriggerCondition::WasCast => {
            let checked_id = trigger_event
                .and_then(|e| match e {
                    GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                    _ => None,
                })
                .or(source_id);
            checked_id
                .and_then(|id| state.objects.get(&id))
                .is_some_and(|obj| obj.cast_from_zone.is_some())
        }
        // CR 601.2: "if it wasn't cast" / "if none of them were cast" — true when
        // the entering creature was NOT cast (ninjutsu, reanimation, flicker, etc.).
        // For batch-enters triggers (e.g., Satoru, the Infiltrator), the trigger source
        // is the permanent with the ability, not the entering creature. We must check
        // the entering object from the trigger event, falling back to source_id for
        // self-referential ETB triggers where source == entering creature.
        TriggerCondition::WasNotCast => {
            let entering_id = trigger_event
                .and_then(|e| match e {
                    GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                    _ => None,
                })
                .or(source_id);
            entering_id
                .and_then(|id| state.objects.get(&id))
                .is_some_and(|obj| obj.cast_from_zone.is_none())
        }
        // CR 508.1: "if it's attacking" — true when the trigger source is in combat.attackers.
        TriggerCondition::SourceIsAttacking => {
            let sid = source_id.unwrap_or(ObjectId(0));
            state
                .combat
                .as_ref()
                .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == sid))
        }
        // CR 702.49 + CR 702.190a + CR 603.4 + CR 702.138b: "if its sneak/ninjutsu
        // cost was paid this turn" / "unless it escaped". When `negated` is true,
        // the match inverts so a missing or mismatched tag satisfies the condition
        // (reanimated, hard-cast, or cast via some other variant all pass).
        TriggerCondition::CastVariantPaid { variant, negated } => {
            let matched = source_id
                .and_then(|id| state.objects.get(&id))
                .map(|obj| obj.cast_variant_paid == Some((*variant, state.turn_number)))
                .unwrap_or(false);
            matched ^ *negated
        }
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
        TriggerCondition::ControlsType { filter } => {
            let ctx = FilterContext::from_source(state, source_id.unwrap_or(ObjectId(0)));
            state
                .battlefield
                .iter()
                .any(|id| matches_target_filter(state, *id, filter, &ctx))
        }
        // CR 603.8: "when you control no [type]" — true when no permanents match the filter.
        TriggerCondition::ControlsNone { filter } => {
            let ctx = FilterContext::from_source(state, source_id.unwrap_or(ObjectId(0)));
            !state
                .battlefield
                .iter()
                .any(|id| matches_target_filter(state, *id, filter, &ctx))
        }
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
            let ctx = FilterContext::from_source(state, source_id.unwrap_or(ObjectId(0)));
            let count = state
                .battlefield
                .iter()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == controller
                            && matches_target_filter(state, **id, filter, &ctx)
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
                    spells.iter().any(|record| {
                        spell_record_matches_filter(
                            record,
                            filter,
                            controller,
                            &state.all_creature_types,
                        )
                    })
                }),
        },
        TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => {
            // CR 603.4: Intervening-if check runs at both detection and resolution.
            // At detection time `state.current_trigger_event` is not yet populated,
            // so event-scoped refs (e.g. `ManaSpentOnTriggeringSpell`) must resolve
            // against the explicit `trigger_event` parameter.
            let source_id = source_id.unwrap_or(ObjectId(0));
            let lhs = crate::game::quantity::resolve_quantity_for_trigger_check(
                state,
                lhs,
                controller,
                source_id,
                trigger_event,
            );
            let rhs = crate::game::quantity::resolve_quantity_for_trigger_check(
                state,
                rhs,
                controller,
                source_id,
                trigger_event,
            );
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
                let ctx = FilterContext::from_source(state, source_id.unwrap_or(ObjectId(0)));
                defenders.iter().all(|&def_pid| {
                    !state.battlefield.iter().any(|id| {
                        state.objects.get(id).is_some_and(|obj| {
                            obj.controller == def_pid
                                && matches_target_filter(state, *id, filter, &ctx)
                        })
                    })
                })
            } else {
                false
            }
        }
        // CR 725.1: True when the controller is the monarch.
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
        // CR 702.104b: True when the Tribute ETB replacement resolved without the
        // chosen opponent placing the +1/+1 counters. Read from the creature's
        // persisted `ChosenAttribute::TributeOutcome` — explicit `Declined` or no
        // outcome recorded (e.g., all opponents eliminated before the prompt) both
        // count as "tribute wasn't paid". An explicit `Paid` outcome suppresses the
        // trigger.
        TriggerCondition::TributeNotPaid => source_id
            .and_then(|id| state.objects.get(&id))
            .is_none_or(|obj| {
                !obj.chosen_attributes
                    .iter()
                    .any(|a| matches!(a, ChosenAttribute::TributeOutcome(TributeOutcome::Paid)))
            }),
        // CR 207.2c: Addendum — cast during main phase.
        TriggerCondition::CastDuringMainPhase => {
            matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
        }
        // CR 207.2c: Adamant — at least N mana of a specific color was spent to cast.
        // Reads the per-color tally recorded in casting::pay_mana_cost.
        TriggerCondition::ManaColorSpent { color, minimum } => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.colors_spent_to_cast.get(*color) >= *minimum),
        // CR 601.2h: "if no mana was spent to cast it/them" — check the entering object.
        TriggerCondition::ManaSpentCondition { text } => {
            let entering_id = trigger_event
                .and_then(|e| match e {
                    GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                    _ => None,
                })
                .or(source_id);
            if text.contains("no mana was spent") {
                entering_id
                    .and_then(|id| state.objects.get(&id))
                    .is_some_and(|obj| !obj.mana_spent_to_cast)
            } else {
                // Other mana-spent conditions (e.g., "if mana from a Treasure was spent")
                // remain unimplemented — default to false.
                false
            }
        }
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
        // CR 121.1 + CR 504.1 + CR 603.4: "except the first one [you|they]
        // draw in each of [your|their] draw steps" — suppress trigger when
        // the drawing player is the active player, the current phase is the
        // draw step, and the event is the first draw of the step
        // (`nth_in_step == 1`). The ordinal is set by the emitter AFTER
        // incrementing `cards_drawn_this_step`, so 1 == first draw of step.
        TriggerCondition::ExceptFirstDrawInDrawStep => match trigger_event {
            Some(GameEvent::CardDrawn {
                player_id,
                nth_in_step,
                ..
            }) => {
                let in_draw_step = state.phase == crate::types::phase::Phase::Draw;
                let drawer_is_active = *player_id == state.active_player;
                !(in_draw_step && drawer_is_active && *nth_in_step == 1)
            }
            // Defensive: a non-CardDrawn event reaching this condition is a
            // parser/wiring error. Fail-closed (don't fire) so the misattach
            // surfaces rather than silently spamming triggers.
            _ => false,
        },
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
        // CR 711.2a + CR 711.2b: Level-up creature trigger gating — check counter count on source.
        // `CounterMatch::Any` sums across every counter type; `OfType(ct)` reads a single type.
        // Mirrors `StaticCondition::HasCounters` evaluation in `layers.rs`.
        TriggerCondition::HasCounters {
            counters,
            minimum,
            maximum,
        } => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| {
                let count: u32 = match counters {
                    crate::types::counter::CounterMatch::Any => obj.counters.values().sum(),
                    crate::types::counter::CounterMatch::OfType(ct) => {
                        obj.counters.get(ct).copied().unwrap_or(0)
                    }
                };
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
                            && matches_target_filter(
                                state,
                                id,
                                filter,
                                &FilterContext::from_source(state, id),
                            )
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
        | TriggerConstraint::OnlyDuringYourMainPhase
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
    use crate::game::filter::{matches_target_filter, FilterContext};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Comparator, ControllerRef, Effect, FilterProp,
        GainLifePlayer, MultiTargetSpec, QuantityExpr, QuantityRef, TargetFilter, TriggerCondition,
        TriggerConstraint, TriggerDefinition, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{GameState, SpellCastRecord, ZoneChangeRecord};
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

    fn zone_changed_event(
        object_id: ObjectId,
        from: Zone,
        to: Zone,
        core_types: Vec<CoreType>,
        subtypes: Vec<&str>,
    ) -> GameEvent {
        GameEvent::ZoneChanged {
            object_id,
            from: Some(from),
            to,
            record: Box::new(ZoneChangeRecord {
                name: "Test Object".to_string(),
                core_types,
                subtypes: subtypes.into_iter().map(str::to_string).collect(),
                ..ZoneChangeRecord::test_minimal(object_id, Some(from), to)
            }),
        }
    }

    /// CR 111.1 + CR 603.6a: Helper for token creation events — no prior zone.
    fn token_zone_changed_event(
        object_id: ObjectId,
        core_types: Vec<CoreType>,
        subtypes: Vec<&str>,
    ) -> GameEvent {
        GameEvent::ZoneChanged {
            object_id,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                name: "Test Token".to_string(),
                core_types,
                subtypes: subtypes.into_iter().map(str::to_string).collect(),
                is_token: true,
                ..ZoneChangeRecord::test_minimal(object_id, None, Zone::Battlefield)
            }),
        }
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
                            target: TargetFilter::Controller,
                            up_to: false,
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
                            target: TargetFilter::Controller,
                            up_to: false,
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        // Trigger event
        let events = vec![zone_changed_event(
            ObjectId(99),
            Zone::Hand,
            Zone::Battlefield,
            Vec::new(),
            Vec::new(),
        )];

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
        let ctx = FilterContext::from_source(&state, ObjectId(99));
        assert!(matches_target_filter(&state, id, &creature_filter, &ctx));
        assert!(!matches_target_filter(&state, id, &land_filter, &ctx));
        assert!(matches_target_filter(&state, id, &TargetFilter::Any, &ctx));
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
        let ctx = FilterContext::from_source(&state, source);
        assert!(matches_target_filter(
            &state,
            target,
            &creature_you_ctrl,
            &ctx
        ));
        assert!(!matches_target_filter(
            &state,
            opp_target,
            &creature_you_ctrl,
            &ctx
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
            &FilterContext::from_source(&state, obj),
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
            &FilterContext::from_source(&state, other),
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
                            target: TargetFilter::Controller,
                            up_to: false,
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        // Simulate a ZoneChanged event (another creature enters)
        let events = vec![zone_changed_event(
            ObjectId(99),
            Zone::Hand,
            Zone::Battlefield,
            Vec::new(),
            Vec::new(),
        )];

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

    /// CR 603.6a + CR 107.3: "Whenever another creature enters, put X +1/+1
    /// counters on ~, where X is that creature's power" (Hamletback Goliath).
    /// The ETB trigger (CR 603.6a) fires for the entering creature; the trigger
    /// body's X is defined by the ability text (CR 107.3) as the entering
    /// creature's power, which the parser lowers to
    /// `QuantityRef::EventContextSourcePower`. At resolution the event's source
    /// is the entering creature, so that variant must read THAT creature's
    /// power, not default to 0. Covers the class of ETB triggers that scale
    /// a self-counter by the entering object's power/toughness (~20 cards:
    /// Hamletback Goliath, Kresh the Bloodbraided, Nantuko Mentor, ...).
    #[test]
    fn hamletback_etb_trigger_scales_counter_count_by_triggering_creature_power() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Source creature: has the "whenever another creature enters" trigger.
        let goliath = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hamletback-like".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&goliath).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(6);
            obj.toughness = Some(6);
            obj.entered_battlefield_turn = Some(0);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::PutCounter {
                            counter_type: "P1P1".to_string(),
                            count: QuantityExpr::Ref {
                                qty: QuantityRef::EventContextSourcePower,
                            },
                            target: TargetFilter::SelfRef,
                        },
                    ))
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::Another]),
                    )),
            );
        }

        // Entering creature: the "another creature" with power 4.
        let entering = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Entering 4/4".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&entering).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(4);
            obj.toughness = Some(4);
            obj.entered_battlefield_turn = Some(1);
        }

        // Fire the ETB event and enqueue the trigger.
        let events_in = vec![zone_changed_event(
            entering,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        )];
        process_triggers(&mut state, &events_in);
        assert_eq!(state.stack.len(), 1, "ETB trigger should be on the stack");

        // Resolve the trigger: this sets current_trigger_event and executes PutCounter.
        let mut out_events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut out_events);

        // Goliath should gain 4 (= entering creature's power) +1/+1 counters.
        let p1p1 = state.objects[&goliath]
            .counters
            .get(&crate::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            p1p1, 4,
            "EventContextSourcePower must resolve to the entering creature's power (4), \
             yielding 4 +1/+1 counters on the source (got {p1p1})"
        );
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
                            target: TargetFilter::Controller,
                            up_to: false,
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
                            target: TargetFilter::Controller,
                            up_to: false,
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![zone_changed_event(
            ObjectId(99),
            Zone::Hand,
            Zone::Battlefield,
            Vec::new(),
            Vec::new(),
        )];

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
                            target: TargetFilter::Controller,
                            up_to: false,
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
        let events = vec![zone_changed_event(
            land,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Land],
            Vec::new(),
        )];
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

        let events = vec![zone_changed_event(
            creature,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        )];
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
                    target: TargetFilter::Controller,
                    up_to: false,
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
                                enter_with_counters: vec![],
                            },
                        )
                        .duration(crate::types::ability::Duration::UntilHostLeavesPlay),
                    )
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield),
            );
        }

        // Fire an ETB event for the trigger creature
        let events = vec![zone_changed_event(
            trigger_creature,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Enchantment],
            Vec::new(),
        )];

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

    /// CR 115.1b + CR 609: Pit of Offerings — "exile up to three target cards from graveyards."
    /// The trigger carries `multi_target: { min: 0, max: 3 }` on its ChangeZone effect.
    /// `build_target_slots` must surface THREE optional slots so target selection prompts
    /// the player for 0–3 targets (not exactly 1).
    #[test]
    fn pit_of_offerings_multi_target_surfaces_three_slots() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Populate graveyards with three cards (legal "card in a graveyard" targets).
        let gy1 = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "GY Card 1".to_string(),
            Zone::Graveyard,
        );
        let gy2 = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "GY Card 2".to_string(),
            Zone::Graveyard,
        );
        let gy3 = create_object(
            &mut state,
            CardId(12),
            PlayerId(1),
            "GY Card 3".to_string(),
            Zone::Graveyard,
        );

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.entered_battlefield_turn = Some(1);
            let mut execute = AbilityDefinition::new(
                AbilityKind::Database,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![crate::types::ability::TypeFilter::Card],
                        controller: None,
                        properties: vec![FilterProp::InZone {
                            zone: Zone::Graveyard,
                        }],
                    }),
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            );
            execute.multi_target = Some(MultiTargetSpec {
                min: 0,
                max: Some(3),
            });
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(execute)
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![zone_changed_event(
            source,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Land],
            Vec::new(),
        )];

        process_triggers(&mut state, &events);

        // Trigger should be pending (not auto-resolved to 1 target).
        assert!(state.pending_trigger.is_some(), "pending_trigger set");
        let pending = state.pending_trigger.as_ref().unwrap();

        // The crux: build_target_slots must surface THREE slots when multi_target.max == 3,
        // not one. Each slot's legal_targets is the full candidate set (gy1, gy2, gy3).
        let slots = super::super::ability_utils::build_target_slots(&state, &pending.ability)
            .expect("slot build");
        assert_eq!(
            slots.len(),
            3,
            "multi_target.max = 3 must produce 3 target slots, got {}",
            slots.len()
        );
        for slot in &slots {
            assert!(slot.optional, "min = 0 → every slot is optional");
            assert_eq!(
                slot.legal_targets.len(),
                3,
                "each slot lists all three graveyard cards"
            );
        }
        // Silence unused-var warnings for the graveyard object IDs.
        let _ = (gy1, gy2, gy3);
    }

    /// CR 115.1b + CR 609: Exercise end-to-end ChooseTarget flow for Pit of Offerings.
    /// After firing the ETB trigger, the engine must accept three sequential ChooseTarget
    /// actions, then resolve by exiling all three selected cards.
    #[test]
    fn pit_of_offerings_multi_target_full_flow_exiles_three_cards() {
        use crate::types::ability::TargetRef;
        use crate::types::actions::GameAction;
        use crate::types::phase::Phase;

        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;
        state.turn_number = 2;
        state.waiting_for = crate::types::game_state::WaitingFor::Priority {
            player: PlayerId(0),
        };

        let gy1 = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "GY1".to_string(),
            Zone::Graveyard,
        );
        let gy2 = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "GY2".to_string(),
            Zone::Graveyard,
        );
        let gy3 = create_object(
            &mut state,
            CardId(12),
            PlayerId(1),
            "GY3".to_string(),
            Zone::Graveyard,
        );

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.entered_battlefield_turn = Some(2);
            let mut execute = AbilityDefinition::new(
                AbilityKind::Database,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![crate::types::ability::TypeFilter::Card],
                        controller: None,
                        properties: vec![FilterProp::InZone {
                            zone: Zone::Graveyard,
                        }],
                    }),
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            );
            execute.multi_target = Some(MultiTargetSpec {
                min: 0,
                max: Some(3),
            });
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(execute)
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield),
            );
        }

        // Fire the ETB trigger.
        let events = vec![zone_changed_event(
            source,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Land],
            Vec::new(),
        )];
        process_triggers(&mut state, &events);

        // Advance pending trigger → TriggerTargetSelection.
        let wf = crate::game::engine::begin_pending_trigger_target_selection(&mut state)
            .expect("begin selection")
            .expect("selection needed");
        state.waiting_for = wf;

        // Three ChooseTarget actions, one per slot.
        for target_id in [gy1, gy2, gy3] {
            let result = crate::game::engine::apply(
                &mut state,
                PlayerId(0),
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target_id)),
                },
            )
            .expect("ChooseTarget should succeed");
            let _ = result;
        }

        // Resolve the stack by passing priority.
        let mut safety_bound = 20;
        while !state.stack.is_empty() && safety_bound > 0 {
            let actor = state.priority_player;
            crate::game::engine::apply(&mut state, actor, GameAction::PassPriority)
                .expect("pass priority");
            safety_bound -= 1;
        }

        // All three graveyard cards must now be in exile.
        for target_id in [gy1, gy2, gy3] {
            assert_eq!(
                state.objects.get(&target_id).unwrap().zone,
                Zone::Exile,
                "object {:?} should be in exile after resolve",
                target_id
            );
        }
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
                                enter_with_counters: vec![],
                            },
                        )
                        .duration(crate::types::ability::Duration::UntilHostLeavesPlay),
                    )
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![zone_changed_event(
            trigger_creature,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Enchantment],
            Vec::new(),
        )];

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
                            enter_with_counters: vec![],
                        },
                    ))
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![zone_changed_event(
            trigger_creature,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Enchantment],
            Vec::new(),
        )];

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
                            enter_with_counters: vec![],
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

        let events = vec![zone_changed_event(
            source,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Enchantment],
            Vec::new(),
        )];

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

        let events = vec![zone_changed_event(
            ObjectId(99),
            Zone::Hand,
            Zone::Battlefield,
            Vec::new(),
            Vec::new(),
        )];

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
                            target: TargetFilter::Controller,
                            up_to: false,
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![zone_changed_event(
            ObjectId(99),
            Zone::Hand,
            Zone::Battlefield,
            Vec::new(),
            Vec::new(),
        )];

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
                    enter_with_counters: vec![],
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
                    target: TargetFilter::Controller,
                    up_to: false,
                },
            )));
            spell.trigger_definitions.push(trigger);
        }
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: Some(ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                        up_to: false,
                    },
                    vec![],
                    spell_id,
                    PlayerId(0),
                )),
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
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
                    has_x_in_cost: false,
                },
                SpellCastRecord {
                    core_types: vec![CoreType::Creature],
                    supertypes: vec![],
                    subtypes: vec!["Bird".to_string()],
                    keywords: vec![Keyword::Flying],
                    colors: vec![ManaColor::Blue],
                    mana_value: 3,
                    has_x_in_cost: false,
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
            state.stack.back().map(|entry| &entry.kind),
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
                            target: TargetFilter::Controller,
                            up_to: false,
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
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                name: "Bird".to_string(),
                core_types: vec![CoreType::Creature],
                keywords: vec![Keyword::Flying],
                ..ZoneChangeRecord::test_minimal(bird, Some(Zone::Hand), Zone::Battlefield)
            }),
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
                                    enter_with_counters: vec![],
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
        let events = vec![zone_changed_event(
            bat,
            Zone::Stack,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        )];

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
    fn skyclave_apparition_ltb_trigger_uses_zone_change_linked_exile_snapshot() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let skyclave = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Skyclave Apparition".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&skyclave).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(
                        AbilityDefinition::new(
                            AbilityKind::Database,
                            Effect::Token {
                                name: "Illusion".to_string(),
                                power: crate::types::ability::PtValue::Quantity(
                                    QuantityExpr::Ref {
                                        qty: QuantityRef::Aggregate {
                                            function: crate::types::ability::AggregateFunction::Sum,
                                            property:
                                                crate::types::ability::ObjectProperty::ManaValue,
                                            filter: TargetFilter::And {
                                                filters: vec![
                                                    TargetFilter::ExiledBySource,
                                                    TargetFilter::Typed(
                                                        TypedFilter::default().properties(vec![
                                                            FilterProp::Owned {
                                                                controller: ControllerRef::You,
                                                            },
                                                        ]),
                                                    ),
                                                ],
                                            },
                                        },
                                    },
                                ),
                                toughness: crate::types::ability::PtValue::Quantity(
                                    QuantityExpr::Ref {
                                        qty: QuantityRef::Aggregate {
                                            function: crate::types::ability::AggregateFunction::Sum,
                                            property:
                                                crate::types::ability::ObjectProperty::ManaValue,
                                            filter: TargetFilter::And {
                                                filters: vec![
                                                    TargetFilter::ExiledBySource,
                                                    TargetFilter::Typed(
                                                        TypedFilter::default().properties(vec![
                                                            FilterProp::Owned {
                                                                controller: ControllerRef::You,
                                                            },
                                                        ]),
                                                    ),
                                                ],
                                            },
                                        },
                                    },
                                ),
                                types: vec!["Creature".to_string(), "Illusion".to_string()],
                                colors: vec![ManaColor::Blue],
                                keywords: vec![],
                                tapped: false,
                                count: QuantityExpr::Fixed { value: 1 },
                                owner: TargetFilter::Controller,
                                attach_to: None,
                                enters_attacking: false,
                                supertypes: vec![],
                                static_abilities: vec![],
                                enter_with_counters: vec![],
                            },
                        )
                        .player_scope(
                            crate::types::ability::PlayerFilter::OwnersOfCardsExiledBySource,
                        ),
                    )
                    .origin(Zone::Battlefield)
                    .valid_card(TargetFilter::SelfRef)
                    .trigger_zones(vec![Zone::Battlefield]),
            );
        }

        for (card_id, owner, mv) in [
            (301, PlayerId(0), 2u32),
            (302, PlayerId(0), 3),
            (303, PlayerId(1), 4),
        ] {
            let exiled = create_object(
                &mut state,
                CardId(card_id),
                owner,
                format!("Exiled {card_id}"),
                Zone::Exile,
            );
            state.objects.get_mut(&exiled).unwrap().mana_cost =
                crate::types::mana::ManaCost::generic(mv);
            state.exile_links.push(crate::types::game_state::ExileLink {
                source_id: skyclave,
                exiled_id: exiled,
                kind: crate::types::game_state::ExileLinkKind::TrackedBySource,
            });
        }

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, skyclave, Zone::Graveyard, &mut events);

        assert!(
            state
                .exile_links
                .iter()
                .all(|link| link.source_id != skyclave),
            "precondition: tracked links should be pruned before trigger resolution"
        );

        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            1,
            "LTB trigger should be pushed to stack"
        );

        crate::game::stack::resolve_top(&mut state, &mut Vec::new());

        let mut created: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token)
            .map(|object| {
                (
                    object.owner,
                    object.controller,
                    object.power,
                    object.toughness,
                )
            })
            .collect();
        created.sort_by_key(|entry| entry.0);

        assert_eq!(
            created,
            vec![
                (PlayerId(0), PlayerId(0), Some(5), Some(5)),
                (PlayerId(1), PlayerId(1), Some(4), Some(4)),
            ]
        );
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
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(20),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
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
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(20),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
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
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(20),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
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
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(20),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
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
            nth_in_step: 1,
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
            nth_in_step: 1,
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

    // === CR 207.2c: Adamant — ManaColorSpent intervening-if ===

    fn setup_with_colored_cast(color: ManaColor, count: u32) -> (GameState, ObjectId) {
        let mut state = setup();
        let src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Adamant Source".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&src).unwrap();
        obj.colors_spent_to_cast.add(color, count);
        (state, src)
    }

    #[test]
    fn test_adamant_true_when_enough_color_spent() {
        let (state, src) = setup_with_colored_cast(ManaColor::Red, 3);
        let cond = TriggerCondition::ManaColorSpent {
            color: ManaColor::Red,
            minimum: 3,
        };
        assert!(check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));
    }

    #[test]
    fn test_adamant_false_when_not_enough() {
        let (state, src) = setup_with_colored_cast(ManaColor::Red, 3);
        let cond = TriggerCondition::ManaColorSpent {
            color: ManaColor::Red,
            minimum: 4,
        };
        assert!(!check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));
    }

    #[test]
    fn test_adamant_false_when_wrong_color() {
        let (state, src) = setup_with_colored_cast(ManaColor::Green, 3);
        let cond = TriggerCondition::ManaColorSpent {
            color: ManaColor::Red,
            minimum: 3,
        };
        assert!(!check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));
    }

    #[test]
    fn test_adamant_respects_minimum_one() {
        // minimum: 1 with one red spent → true
        let (state, src) = setup_with_colored_cast(ManaColor::Red, 1);
        let cond = TriggerCondition::ManaColorSpent {
            color: ManaColor::Red,
            minimum: 1,
        };
        assert!(check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));

        // minimum: 1 with zero red spent → false
        let (state, src) = setup_with_colored_cast(ManaColor::Green, 5);
        let cond = TriggerCondition::ManaColorSpent {
            color: ManaColor::Red,
            minimum: 1,
        };
        assert!(!check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));
    }

    // === CR 603.6a + CR 611.2b: "When ~ enters untapped/tapped" ETB gating ===
    //
    // Gingerbread Cabin class ("When this land enters untapped, create a Food
    // token.") relies on `SourceIsTapped { negated: true }` evaluating the
    // post-replacement-pipeline tapped state of the source at trigger-check
    // time. The parser already attaches the condition; these tests guard the
    // runtime evaluator so an ETB tapped via the "enters tapped unless ..."
    // replacement suppresses the Food trigger, and an ETB untapped fires it.

    #[test]
    fn source_enters_untapped_fires_when_object_untapped() {
        let mut state = setup();
        let src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gingerbread Cabin".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&src).unwrap().tapped = false;

        let cond = TriggerCondition::SourceIsTapped { negated: true };
        assert!(check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));
    }

    #[test]
    fn source_enters_untapped_suppressed_when_object_tapped() {
        // Simulates the "enters tapped unless you control three or more other
        // Forests" replacement resolving to tapped — the Food trigger must NOT
        // fire.
        let mut state = setup();
        let src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gingerbread Cabin".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&src).unwrap().tapped = true;

        let cond = TriggerCondition::SourceIsTapped { negated: true };
        assert!(!check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));
    }

    #[test]
    fn source_enters_tapped_fires_when_object_tapped() {
        // Amulet of Vigor class: "enters tapped" rider fires only for tapped
        // ETBs.
        let mut state = setup();
        let src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Amulet of Vigor".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&src).unwrap().tapped = true;

        let cond = TriggerCondition::SourceIsTapped { negated: false };
        assert!(check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));
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
        let events = vec![zone_changed_event(
            mite_id,
            Zone::Battlefield,
            Zone::Graveyard,
            vec![CoreType::Creature, CoreType::Artifact],
            Vec::new(),
        )];

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
        let events = vec![zone_changed_event(
            obj_id,
            Zone::Graveyard,
            Zone::Exile,
            vec![CoreType::Creature],
            Vec::new(),
        )];

        process_triggers(&mut state, &events);
        assert!(
            state.stack.is_empty(),
            "Trigger should not fire for non-battlefield origin zone changes"
        );
    }

    #[test]
    fn food_leaves_battlefield_trigger_uses_zone_change_snapshot() {
        let mut state = setup();
        state.turn_number = 3;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;

        let ygra_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Ygra, Eater of All".to_string(),
            Zone::Battlefield,
        );
        {
            let ygra = state.objects.get_mut(&ygra_id).unwrap();
            ygra.controller = PlayerId(0);
            ygra.card_types.core_types.push(CoreType::Creature);
            ygra.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::default().with_type(TypeFilter::Subtype("Food".to_string())),
                    ))
                    .origin(Zone::Battlefield)
                    .destination(Zone::Graveyard)
                    .trigger_zones(vec![Zone::Battlefield])
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::PutCounter {
                            counter_type: "P1P1".to_string(),
                            count: QuantityExpr::Fixed { value: 2 },
                            target: TargetFilter::SelfRef,
                        },
                    )),
            );
        }

        let events = vec![zone_changed_event(
            ObjectId(301),
            Zone::Battlefield,
            Zone::Graveyard,
            vec![CoreType::Creature, CoreType::Artifact],
            vec!["Food"],
        )];

        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 1, "Ygra trigger should be on the stack");
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
            enter_with_counters: vec![],
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
            enter_with_counters: vec![],
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_some(),
            "ChangeZone from battlefield should still extract target for stack-time targeting"
        );
    }

    // === CR 603.2g + CR 603.6a + CR 700.4: SuppressTriggers integration tests ===

    use crate::types::ability::StaticDefinition;
    use crate::types::statics::{StaticMode, SuppressedTriggerEvent};

    /// Attach a `SuppressTriggers` static to a newly-created permanent in `state.battlefield`.
    fn add_suppress_triggers_permanent(
        state: &mut GameState,
        controller: PlayerId,
        source_filter: TargetFilter,
        events: Vec<SuppressedTriggerEvent>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(0xABCDE),
            controller,
            "Suppressor".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.entered_battlefield_turn = Some(0);
        obj.static_definitions
            .push(StaticDefinition::new(StaticMode::SuppressTriggers {
                source_filter,
                events,
            }));
        id
    }

    /// Attach an ETB-trigger creature to a newly-created permanent on the battlefield.
    /// Trigger is a no-op Draw(1) keyed on "whenever any creature enters".
    fn add_etb_observer(state: &mut GameState, controller: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(0xFADE),
            controller,
            "ETB Observer".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.entered_battlefield_turn = Some(0);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                        up_to: false,
                    },
                ))
                .destination(Zone::Battlefield),
        );
        id
    }

    /// Phase out a permanent via the real `phase_out_object` path so the
    /// CR 702.26b phased-out status is authoritative (no direct `phase_status`
    /// pokes from tests). Shared with the regression tests below.
    fn phase_out_in_place(state: &mut GameState, id: ObjectId) {
        let mut events = Vec::new();
        crate::game::phasing::phase_out_object(
            state,
            id,
            crate::game::game_object::PhaseOutCause::Directly,
            &mut events,
        );
    }

    #[test]
    fn phased_out_torpor_orb_does_not_suppress_etb_triggers() {
        // CR 702.26b + CR 603.2g regression: a phased-out Torpor Orb must not
        // suppress ETB triggers. Drives `process_triggers` end-to-end — the
        // observer's ETB trigger MUST land on the stack because the Torpor
        // static is gated out by `battlefield_active_statics`.
        let mut state = setup();
        state.active_player = PlayerId(0);

        let torpor_id = add_suppress_triggers_permanent(
            &mut state,
            PlayerId(0),
            TargetFilter::Typed(TypedFilter::creature()),
            vec![SuppressedTriggerEvent::EntersBattlefield],
        );
        phase_out_in_place(&mut state, torpor_id);
        let _observer = add_etb_observer(&mut state, PlayerId(0));

        let events = vec![zone_changed_event(
            ObjectId(0xBEEF),
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        )];
        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            1,
            "Phased-out Torpor Orb must not suppress the observer's ETB trigger"
        );
    }

    #[test]
    fn commander_in_command_zone_etb_trigger_does_not_fire() {
        // CR 114.4 regression: a non-emblem object in the command zone has no
        // functioning abilities, so its ETB observer trigger must not fire
        // when some other creature enters. `process_triggers` must reach
        // through `active_trigger_definitions`, which drops command-zone
        // non-emblems.
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Put a triggered "ETB observer" in the command zone rather than on
        // the battlefield. Same trigger shape as `add_etb_observer`.
        let commander_id = create_object(
            &mut state,
            CardId(0xC0FFEE),
            PlayerId(0),
            "Commander Observer".to_string(),
            Zone::Command,
        );
        {
            let obj = state.objects.get_mut(&commander_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.is_emblem = false;
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                            up_to: false,
                        },
                    ))
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![zone_changed_event(
            ObjectId(0xBEEF),
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        )];
        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            0,
            "A non-emblem command-zone object must not fire its ETB observer trigger"
        );
    }

    #[test]
    fn suppress_triggers_torpor_blocks_creature_etb_observer() {
        // CR 603.2g + CR 603.6a: Torpor Orb-class static on battlefield suppresses
        // an observer's ETB trigger when a CREATURE enters. Soul Warden reading.
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Torpor Orb: source_filter = creatures, events = [EntersBattlefield]
        add_suppress_triggers_permanent(
            &mut state,
            PlayerId(0),
            TargetFilter::Typed(TypedFilter::creature()),
            vec![SuppressedTriggerEvent::EntersBattlefield],
        );
        let _observer = add_etb_observer(&mut state, PlayerId(0));

        // Simulate a creature entering the battlefield.
        let events = vec![zone_changed_event(
            ObjectId(0xBEEF),
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        )];

        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            0,
            "Torpor Orb should suppress the observer's ETB trigger for a creature entering"
        );
    }

    #[test]
    fn suppress_triggers_torpor_permits_non_creature_etb() {
        // CR 603.2g + CR 603.6a: Torpor Orb only filters on CREATURES. An artifact
        // entering still fires ETB triggers normally — filter correctness test.
        let mut state = setup();
        state.active_player = PlayerId(0);

        add_suppress_triggers_permanent(
            &mut state,
            PlayerId(0),
            TargetFilter::Typed(TypedFilter::creature()),
            vec![SuppressedTriggerEvent::EntersBattlefield],
        );
        let _observer = add_etb_observer(&mut state, PlayerId(0));

        // Non-creature (artifact) enters.
        let events = vec![zone_changed_event(
            ObjectId(0xBEEF),
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Artifact],
            Vec::new(),
        )];

        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            1,
            "Torpor Orb must NOT suppress ETB triggers caused by a non-creature entering"
        );
    }

    #[test]
    fn suppress_triggers_torpor_permits_dies_event() {
        // CR 700.4: Torpor Orb has `events = [EntersBattlefield]` only — death
        // triggers must still fire. Event-set correctness test.
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Torpor (ETB-only) on battlefield.
        add_suppress_triggers_permanent(
            &mut state,
            PlayerId(0),
            TargetFilter::Typed(TypedFilter::creature()),
            vec![SuppressedTriggerEvent::EntersBattlefield],
        );

        // Create a creature with a "dies" trigger and place it on the battlefield,
        // then simulate its death.
        let dying = create_object(
            &mut state,
            CardId(0xD1E),
            PlayerId(0),
            "Dying Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&dying).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(0);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                            up_to: false,
                        },
                    ))
                    .origin(Zone::Battlefield)
                    .destination(Zone::Graveyard),
            );
        }
        // Move the object out of the battlefield to mirror a real death.
        {
            let obj = state.objects.get_mut(&dying).unwrap();
            obj.zone = Zone::Graveyard;
        }
        state.battlefield.retain(|id| *id != dying);

        let events = vec![zone_changed_event(
            dying,
            Zone::Battlefield,
            Zone::Graveyard,
            vec![CoreType::Creature],
            Vec::new(),
        )];

        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            1,
            "Torpor Orb must NOT suppress dies triggers — only [EntersBattlefield] is in events"
        );
    }

    #[test]
    fn suppress_triggers_hushbringer_blocks_creature_dies() {
        // CR 700.4 + CR 603.2g: Hushbringer-class (`events = [EntersBattlefield, Dies]`)
        // suppresses death triggers on creatures. Event-set building-block test.
        let mut state = setup();
        state.active_player = PlayerId(0);

        add_suppress_triggers_permanent(
            &mut state,
            PlayerId(0),
            TargetFilter::Typed(TypedFilter::creature()),
            vec![
                SuppressedTriggerEvent::EntersBattlefield,
                SuppressedTriggerEvent::Dies,
            ],
        );

        let dying = create_object(
            &mut state,
            CardId(0xD1F),
            PlayerId(0),
            "Hushed Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&dying).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(0);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                            up_to: false,
                        },
                    ))
                    .origin(Zone::Battlefield)
                    .destination(Zone::Graveyard),
            );
        }
        {
            let obj = state.objects.get_mut(&dying).unwrap();
            obj.zone = Zone::Graveyard;
        }
        state.battlefield.retain(|id| *id != dying);

        let events = vec![zone_changed_event(
            dying,
            Zone::Battlefield,
            Zone::Graveyard,
            vec![CoreType::Creature],
            Vec::new(),
        )];

        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            0,
            "Hushbringer-class SuppressTriggers(events=[ETB, Dies]) must suppress creature death triggers"
        );
    }

    #[test]
    fn suppress_triggers_hushbringer_permits_non_creature_dies() {
        // CR 700.4: Hushbringer filters on creatures only — an artifact dying
        // must still fire its triggers. Filter + event-set combination test.
        let mut state = setup();
        state.active_player = PlayerId(0);

        add_suppress_triggers_permanent(
            &mut state,
            PlayerId(0),
            TargetFilter::Typed(TypedFilter::creature()),
            vec![
                SuppressedTriggerEvent::EntersBattlefield,
                SuppressedTriggerEvent::Dies,
            ],
        );

        let dying_artifact = create_object(
            &mut state,
            CardId(0xD20),
            PlayerId(0),
            "Dying Artifact".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&dying_artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.entered_battlefield_turn = Some(0);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                            up_to: false,
                        },
                    ))
                    .origin(Zone::Battlefield)
                    .destination(Zone::Graveyard),
            );
        }
        {
            let obj = state.objects.get_mut(&dying_artifact).unwrap();
            obj.zone = Zone::Graveyard;
        }
        state.battlefield.retain(|id| *id != dying_artifact);

        let events = vec![zone_changed_event(
            dying_artifact,
            Zone::Battlefield,
            Zone::Graveyard,
            vec![CoreType::Artifact],
            Vec::new(),
        )];

        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            1,
            "Hushbringer must NOT suppress triggers for non-creature deaths (filter is creature-only)"
        );
    }

    #[test]
    fn suppress_triggers_no_suppressor_means_trigger_fires() {
        // Baseline: without any SuppressTriggers static, creature ETB fires normally.
        let mut state = setup();
        state.active_player = PlayerId(0);
        let _observer = add_etb_observer(&mut state, PlayerId(0));

        let events = vec![zone_changed_event(
            ObjectId(0xBEEF),
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        )];

        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            1,
            "Baseline: observer ETB trigger must fire when no suppressor is active"
        );
    }

    #[test]
    fn suppress_triggers_ignores_non_zone_change_events() {
        // CR 603.2g: SuppressTriggers keys on ETB / Dies zone-change events only.
        // Other events (phase changes, spell casts) pass through untouched.
        let mut state = setup();
        state.active_player = PlayerId(0);
        add_suppress_triggers_permanent(
            &mut state,
            PlayerId(0),
            TargetFilter::Typed(TypedFilter::creature()),
            vec![
                SuppressedTriggerEvent::EntersBattlefield,
                SuppressedTriggerEvent::Dies,
            ],
        );

        // A non-zone-change event must not be suppressed.
        let event = GameEvent::PhaseChanged { phase: Phase::Draw };
        assert!(
            !event_is_suppressed_by_static_triggers(&state, &event),
            "PhaseChanged must not be suppressed by SuppressTriggers"
        );
    }

    #[test]
    fn suppress_triggers_does_not_block_transform_on_reentry() {
        // CR 603.2g + CR 701.28: SuppressTriggers only gates triggered-ability
        // registration. A permanent returning to the battlefield with
        // `enter_transformed=true` (e.g., Ajani, Nacatl Pariah's flip trigger)
        // must still transform — transform is NOT a triggered ability. Any
        // ETB-triggered abilities on Ajani's back face are legitimately suppressed,
        // but the flip itself must resolve.
        use crate::game::effects::change_zone::execute_zone_move;
        use crate::game::game_object::BackFaceData;
        use crate::types::card_type::CardType;
        use crate::types::mana::{ManaColor, ManaCost};

        let mut state = setup();
        state.active_player = PlayerId(0);

        // Opponent's Doorkeeper Thrull: SuppressTriggers on creature ETB.
        add_suppress_triggers_permanent(
            &mut state,
            PlayerId(1),
            TargetFilter::Typed(TypedFilter::creature()),
            vec![SuppressedTriggerEvent::EntersBattlefield],
        );

        // Ajani is currently in exile (mid-resolution of his flip trigger).
        // Set up as a DFC creature with a planeswalker back face.
        let ajani = create_object(
            &mut state,
            CardId(0xA1A1),
            PlayerId(0),
            "Ajani, Nacatl Pariah".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&ajani).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.back_face = Some(BackFaceData {
                name: "Ajani, Nacatl Avenger".to_string(),
                power: None,
                toughness: None,
                loyalty: Some(4),
                defense: None,
                card_types: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Planeswalker],
                    subtypes: vec!["Ajani".to_string()],
                },
                mana_cost: ManaCost::default(),
                keywords: vec![],
                abilities: vec![],
                trigger_definitions: Default::default(),
                replacement_definitions: Default::default(),
                static_definitions: Default::default(),
                color: vec![ManaColor::White],
                printed_ref: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                layout_kind: None,
            });
        }

        // Return Ajani from exile to battlefield with enter_transformed=true,
        // mirroring the sub_ability of his "Cat dies" trigger.
        let mut events = Vec::new();
        let _ = execute_zone_move(
            &mut state,
            ajani,
            Zone::Exile,
            Zone::Battlefield,
            ObjectId(0xA1A1), // self-sourced
            None,
            true,  // enter_transformed
            false, // effect_enter_tapped
            None,  // controller_override
            &[],   // effect_enter_with_counters
            &mut events,
        );

        let obj = &state.objects[&ajani];
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "Ajani must reach the battlefield"
        );
        assert!(
            obj.transformed,
            "Ajani must flip to his back face — SuppressTriggers must not block CR 701.28 transform"
        );
        assert_eq!(
            obj.name, "Ajani, Nacatl Avenger",
            "Back-face characteristics must be applied"
        );
    }

    #[test]
    fn fertile_ground_triggered_mana_ability_skips_stack_and_adds_mana() {
        // CR 605.1b: "Whenever enchanted land is tapped for mana, its controller
        // adds an additional {G}" — a triggered mana ability that must resolve
        // inline (stack-skipped) so the added mana is available immediately.
        use crate::types::ability::{ManaContribution, ManaProduction, QuantityExpr};

        let mut state = setup();
        state.active_player = PlayerId(0);

        // Enchanted Forest under P0's control.
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        // Fertile Ground attached to the Forest.
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Fertile Ground".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.attached_to = Some(forest.into());
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::AnyOneColor {
                                count: QuantityExpr::Fixed { value: 1 },
                                color_options: vec![
                                    ManaColor::White,
                                    ManaColor::Blue,
                                    ManaColor::Black,
                                    ManaColor::Red,
                                    ManaColor::Green,
                                ],
                                contribution: ManaContribution::Additional,
                            },
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::AttachedTo),
            );
        }

        // Simulate tapping the Forest for mana: ManaAdded with tapped_for_mana=true.
        let events = vec![GameEvent::ManaAdded {
            player_id: PlayerId(0),
            mana_type: crate::types::mana::ManaType::Green,
            source_id: forest,
            tapped_for_mana: true,
        }];

        process_triggers(&mut state, &events);

        // CR 605.3b: Triggered mana ability resolves without using the stack.
        assert_eq!(
            state.stack.len(),
            0,
            "Fertile Ground's mana trigger must not be placed on the stack"
        );
        assert!(
            state.pending_trigger.is_none(),
            "Fertile Ground's mana trigger must not be pending-target"
        );

        // The mana pool now contains one unit. AnyOneColor without color_override
        // resolves to the first color_option by default — the important property
        // for CR 605.1b is that mana was added immediately.
        let pool_size: usize = state
            .players
            .iter()
            .find(|p| p.id == PlayerId(0))
            .map(|p| p.mana_pool.total())
            .unwrap_or(0);
        assert_eq!(
            pool_size, 1,
            "Fertile Ground must add one mana to the pool inline"
        );
    }

    #[test]
    fn utopia_sprawl_triggered_mana_ability_resolves_chosen_color_inline() {
        // CR 603.6d + CR 605.1b: Utopia Sprawl's "As this Aura enters, choose a color"
        // replacement stores a ChosenAttribute::Color on the aura; tapping the
        // enchanted Forest then fires a triggered mana ability that resolves
        // inline, adding one mana of the chosen color to the controller's pool.
        use crate::types::ability::{
            ChosenAttribute, ManaContribution, ManaProduction, QuantityExpr,
        };

        let mut state = setup();
        state.active_player = PlayerId(0);

        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let sprawl = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Utopia Sprawl".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&sprawl).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.attached_to = Some(forest.into());
            obj.entered_battlefield_turn = Some(1);
            // CR 603.6d: The chosen color landed on the aura during ETB (Red in this test).
            obj.chosen_attributes
                .push(ChosenAttribute::Color(ManaColor::Red));
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::ChosenColor {
                                count: QuantityExpr::Fixed { value: 1 },
                                contribution: ManaContribution::Additional,
                            },
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::AttachedTo),
            );
        }

        // Tap the Forest for mana — emits ManaAdded{Green, tapped_for_mana=true}.
        let events = vec![GameEvent::ManaAdded {
            player_id: PlayerId(0),
            mana_type: crate::types::mana::ManaType::Green,
            source_id: forest,
            tapped_for_mana: true,
        }];

        process_triggers(&mut state, &events);

        // CR 605.3b: Stack is empty — the triggered mana ability resolved inline.
        assert_eq!(
            state.stack.len(),
            0,
            "Utopia Sprawl's mana trigger must not be placed on the stack"
        );
        assert!(state.pending_trigger.is_none());

        // The pool now has the chosen-color Red mana added by the trigger.
        let player = state.players.iter().find(|p| p.id == PlayerId(0)).unwrap();
        assert_eq!(
            player
                .mana_pool
                .count_color(crate::types::mana::ManaType::Red),
            1,
            "Utopia Sprawl must add one Red mana (the chosen color) to the pool"
        );
    }

    // -----------------------------------------------------------------------
    // CR 505.1: OnlyDuringYourMainPhase constraint runtime enforcement.
    // Fires only when the active player is the trigger controller AND the
    // phase is precombat or postcombat main.
    // -----------------------------------------------------------------------

    #[test]
    fn only_during_your_main_phase_fires_in_precombat_main() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;

        let mut trig_def = make_trigger(TriggerMode::SaddlesOrCrews);
        trig_def.constraint = Some(TriggerConstraint::OnlyDuringYourMainPhase);

        let event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(42),
            creatures: vec![ObjectId(5)],
        };
        assert!(check_trigger_constraint(
            &state,
            &trig_def,
            ObjectId(42),
            0,
            PlayerId(0),
            &event,
        ));
    }

    #[test]
    fn only_during_your_main_phase_fires_in_postcombat_main() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.phase = Phase::PostCombatMain;

        let mut trig_def = make_trigger(TriggerMode::SaddlesOrCrews);
        trig_def.constraint = Some(TriggerConstraint::OnlyDuringYourMainPhase);

        let event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(42),
            creatures: vec![ObjectId(5)],
        };
        assert!(check_trigger_constraint(
            &state,
            &trig_def,
            ObjectId(42),
            0,
            PlayerId(0),
            &event,
        ));
    }

    #[test]
    fn only_during_your_main_phase_blocks_outside_main_phase() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.phase = Phase::Upkeep;

        let mut trig_def = make_trigger(TriggerMode::SaddlesOrCrews);
        trig_def.constraint = Some(TriggerConstraint::OnlyDuringYourMainPhase);

        let event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(42),
            creatures: vec![ObjectId(5)],
        };
        assert!(!check_trigger_constraint(
            &state,
            &trig_def,
            ObjectId(42),
            0,
            PlayerId(0),
            &event,
        ));
    }

    #[test]
    fn only_during_your_main_phase_blocks_on_opponents_turn() {
        // Even during Player 1's precombat main, Player 0's trigger must NOT fire —
        // "your main phase" is scoped to the trigger's controller.
        let mut state = setup();
        state.active_player = PlayerId(1);
        state.phase = Phase::PreCombatMain;

        let mut trig_def = make_trigger(TriggerMode::SaddlesOrCrews);
        trig_def.constraint = Some(TriggerConstraint::OnlyDuringYourMainPhase);

        let event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(42),
            creatures: vec![ObjectId(5)],
        };
        assert!(!check_trigger_constraint(
            &state,
            &trig_def,
            ObjectId(42),
            0,
            PlayerId(0),
            &event,
        ));
    }

    /// CR 601.2h + CR 603.4: Increment intervening-if gates the counter-placement
    /// trigger on the amount of mana spent to cast the triggering spell exceeding
    /// either the source creature's power or its toughness. This is the regression
    /// gate: before the fix, the condition was silently dropped and the trigger
    /// always fired. Covers both Hungry Graffalon (P3/T4) and Topiary Lecturer
    /// (P1/T2) shapes.
    #[test]
    fn increment_intervening_if_gates_on_mana_spent_vs_self_pt() {
        let mut state = setup();

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hungry Graffalon".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.toughness = Some(4);
        }

        let spell = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Stack,
        );

        let condition = TriggerCondition::Or {
            conditions: vec![
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ManaSpentOnTriggeringSpell,
                    },
                    comparator: Comparator::GT,
                    rhs: QuantityExpr::Ref {
                        qty: QuantityRef::SelfPower,
                    },
                },
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ManaSpentOnTriggeringSpell,
                    },
                    comparator: Comparator::GT,
                    rhs: QuantityExpr::Ref {
                        qty: QuantityRef::SelfToughness,
                    },
                },
            ],
        };

        let event = GameEvent::SpellCast {
            card_id: CardId(2),
            controller: PlayerId(0),
            object_id: spell,
        };

        // 2 mana spent: 2 > 3 false, 2 > 4 false — trigger does NOT fire.
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .mana_spent_to_cast_amount = 2;
        assert!(
            !check_trigger_condition(&state, &condition, PlayerId(0), Some(source), Some(&event)),
            "Increment must not fire when mana spent (2) <= both power (3) and toughness (4)"
        );

        // 4 mana spent: 4 > 3 true — trigger fires even though 4 > 4 is false.
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .mana_spent_to_cast_amount = 4;
        assert!(
            check_trigger_condition(&state, &condition, PlayerId(0), Some(source), Some(&event)),
            "Increment must fire when mana spent (4) > power (3), regardless of toughness"
        );

        // 5 mana spent: 5 > 3 and 5 > 4 — fires.
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .mana_spent_to_cast_amount = 5;
        assert!(
            check_trigger_condition(&state, &condition, PlayerId(0), Some(source), Some(&event)),
            "Increment must fire when mana spent (5) exceeds both power and toughness"
        );

        // Topiary Lecturer shape — P1/T2.
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.power = Some(1);
            obj.toughness = Some(2);
        }
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .mana_spent_to_cast_amount = 2;
        assert!(
            check_trigger_condition(&state, &condition, PlayerId(0), Some(source), Some(&event)),
            "Topiary Lecturer: 2 mana spent > power (1) must fire Increment"
        );

        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .mana_spent_to_cast_amount = 1;
        assert!(
            !check_trigger_condition(&state, &condition, PlayerId(0), Some(source), Some(&event)),
            "Topiary Lecturer: 1 mana spent must not exceed power (1) or toughness (2)"
        );
    }

    /// CR 107.3 + CR 202.1 + CR 603.2c: "Whenever you cast your first spell with
    /// {X} in its mana cost each turn" — constraint check must:
    /// - fire on the first qualifying spell in `spells_cast_this_turn_by_player`
    ///   (count == 1 where the filter matches)
    /// - NOT fire when the current cast is a non-qualifying spell (filter
    ///   mismatches), even if it's the first spell overall
    /// - NOT fire on the second qualifying cast this turn.
    #[test]
    fn first_spell_with_x_constraint_fires_once_per_turn() {
        use crate::types::ability::{FilterProp, TriggerConstraint, TypedFilter};

        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Nev".to_string(),
            Zone::Battlefield,
        );
        let trig_def = {
            let mut d = make_trigger(TriggerMode::SpellCast);
            d.constraint = Some(TriggerConstraint::NthSpellThisTurn {
                n: 1,
                filter: Some(TargetFilter::Typed(
                    TypedFilter::default().properties(vec![FilterProp::HasXInManaCost]),
                )),
            });
            d
        };

        let spell_event = GameEvent::SpellCast {
            card_id: CardId(1),
            controller: PlayerId(0),
            object_id: ObjectId(1000),
        };

        // Case A: first qualifying spell — record has exactly one X-cost cast.
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            vec![SpellCastRecord {
                core_types: vec![CoreType::Sorcery],
                supertypes: vec![],
                subtypes: vec![],
                keywords: vec![],
                colors: vec![],
                mana_value: 3,
                has_x_in_cost: true,
            }],
        );
        assert!(
            check_trigger_constraint(&state, &trig_def, source, 0, PlayerId(0), &spell_event),
            "first qualifying X-spell must fire"
        );

        // Case B: first cast is non-qualifying (no X in cost). Constraint must NOT fire.
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            vec![SpellCastRecord {
                core_types: vec![CoreType::Instant],
                supertypes: vec![],
                subtypes: vec![],
                keywords: vec![],
                colors: vec![],
                mana_value: 1,
                has_x_in_cost: false,
            }],
        );
        assert!(
            !check_trigger_constraint(&state, &trig_def, source, 0, PlayerId(0), &spell_event),
            "non-qualifying spell (no X) must NOT match the first-X-spell constraint"
        );

        // Case C: second qualifying spell (filter count == 2). Must NOT fire.
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            vec![
                SpellCastRecord {
                    core_types: vec![CoreType::Sorcery],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 2,
                    has_x_in_cost: true,
                },
                SpellCastRecord {
                    core_types: vec![CoreType::Sorcery],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 4,
                    has_x_in_cost: true,
                },
            ],
        );
        assert!(
            !check_trigger_constraint(&state, &trig_def, source, 0, PlayerId(0), &spell_event),
            "second X-spell this turn must NOT fire the first-X-spell trigger"
        );

        // Case D: intervening non-X spell does NOT reset the count — second X-spell still fails.
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            vec![
                SpellCastRecord {
                    core_types: vec![CoreType::Sorcery],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 2,
                    has_x_in_cost: true,
                },
                SpellCastRecord {
                    core_types: vec![CoreType::Instant],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 1,
                    has_x_in_cost: false,
                },
                SpellCastRecord {
                    core_types: vec![CoreType::Sorcery],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 4,
                    has_x_in_cost: true,
                },
            ],
        );
        assert!(
            !check_trigger_constraint(&state, &trig_def, source, 0, PlayerId(0), &spell_event),
            "intervening non-X spell must not reset qualifying count"
        );
    }

    /// CR 111.1 + CR 603.6a: An ETB trigger like Elvish Vanguard's "whenever
    /// another Elf enters" MUST fire when an Elf token is created. Tokens are
    /// created in the battlefield zone with no prior zone — the engine emits
    /// `ZoneChanged { from: None, to: Battlefield }` for token creation, and
    /// the existing `ChangesZone` matcher (which requires no origin filter
    /// for pure ETB triggers) matches this event. No token-specific trigger
    /// code is required.
    #[test]
    fn etb_changes_zone_trigger_fires_on_token_creation() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Stand-in for Elvish Vanguard: ETB trigger with no origin filter,
        // destination = Battlefield, filter = "another Elf".
        let vanguard = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Elvish Vanguard".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&vanguard).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let mut trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                        up_to: false,
                    },
                ))
                .destination(Zone::Battlefield);
            trig.valid_card = Some(TargetFilter::Typed(
                TypedFilter::default()
                    .with_type(TypeFilter::Subtype("Elf".to_string()))
                    .properties(vec![crate::types::ability::FilterProp::Another]),
            ));
            obj.trigger_definitions.push(trig);
        }

        // Simulate an Elf token being created — `from: None` per CR 111.1 /
        // 603.6a. The matcher must fire because origin is unfiltered.
        let token_id = ObjectId(500);
        let events = vec![token_zone_changed_event(
            token_id,
            vec![CoreType::Creature],
            vec!["Elf"],
        )];

        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            1,
            "ETB trigger (no origin filter) must fire on token creation"
        );
    }

    /// Negative: a trigger that explicitly names an origin zone ("whenever a
    /// creature is put into a graveyard from the battlefield") must NOT fire
    /// on token creation (`from: None`) — tokens did not come from any zone.
    #[test]
    fn dies_trigger_does_not_fire_on_token_creation() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dies Source".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                            up_to: false,
                        },
                    ))
                    .origin(Zone::Battlefield)
                    .destination(Zone::Graveyard),
            );
        }

        let token_id = ObjectId(600);
        let events = vec![token_zone_changed_event(
            token_id,
            vec![CoreType::Creature],
            Vec::new(),
        )];

        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 0);
    }

    // SOC Tier 2.6: "Whenever you create one or more creature tokens" —
    // batched token-creation trigger (CR 111.1 + CR 603.2c / 603.10c).
    // Build a Staff-like source, emit 2 TokenCreated events for creature
    // tokens controlled by P0, and verify the trigger fires exactly once.
    fn make_token_created_trigger(
        type_filter: Option<TargetFilter>,
        controller_scope: Option<TargetFilter>,
    ) -> TriggerDefinition {
        let mut def = TriggerDefinition::new(TriggerMode::TokenCreated)
            .trigger_zones(vec![Zone::Battlefield])
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                    up_to: false,
                },
            ));
        def.valid_card = type_filter;
        def.valid_target = controller_scope;
        def.batched = true;
        def
    }

    fn add_token_on_battlefield(
        state: &mut GameState,
        controller: PlayerId,
        core_types: Vec<CoreType>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(500),
            controller,
            "Spirit Token".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.controller = controller;
        obj.card_types.core_types = core_types;
        obj.entered_battlefield_turn = Some(1);
        id
    }

    #[test]
    fn tokens_created_trigger_fires_once_for_two_creature_tokens() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Staff of the Storyteller".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(make_token_created_trigger(
                Some(TargetFilter::Typed(
                    TypedFilter::default().with_type(TypeFilter::Creature),
                )),
                Some(TargetFilter::Controller),
            ));
        }

        let tok1 = add_token_on_battlefield(&mut state, PlayerId(0), vec![CoreType::Creature]);
        let tok2 = add_token_on_battlefield(&mut state, PlayerId(0), vec![CoreType::Creature]);

        let events = vec![
            GameEvent::TokenCreated {
                object_id: tok1,
                name: "Spirit".to_string(),
            },
            GameEvent::TokenCreated {
                object_id: tok2,
                name: "Spirit".to_string(),
            },
        ];

        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            1,
            "batched trigger must fire once per pass even with 2 token-creation events"
        );
    }

    #[test]
    fn tokens_created_trigger_rejects_noncreature_token() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Staff of the Storyteller".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(make_token_created_trigger(
                Some(TargetFilter::Typed(
                    TypedFilter::default().with_type(TypeFilter::Creature),
                )),
                Some(TargetFilter::Controller),
            ));
        }

        // Artifact token only — "creature tokens" filter must reject.
        let tok = add_token_on_battlefield(&mut state, PlayerId(0), vec![CoreType::Artifact]);
        let events = vec![GameEvent::TokenCreated {
            object_id: tok,
            name: "Treasure".to_string(),
        }];

        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 0);
    }

    #[test]
    fn tokens_created_trigger_rejects_opponent_creator() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Staff of the Storyteller".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(make_token_created_trigger(
                Some(TargetFilter::Typed(
                    TypedFilter::default().with_type(TypeFilter::Creature),
                )),
                Some(TargetFilter::Controller),
            ));
        }

        // Opponent-controlled creature token — Controller-scope must reject.
        let tok = add_token_on_battlefield(&mut state, PlayerId(1), vec![CoreType::Creature]);
        let events = vec![GameEvent::TokenCreated {
            object_id: tok,
            name: "Zombie".to_string(),
        }];

        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 0);
    }

    // CR 508.1 + CR 603.2c: Unit tests for the `AttackersDeclaredMin` condition
    // (Firemane Commando's attack-batch-size gate).
    #[test]
    fn attackers_declared_min_counts_scope_you() {
        let mut state = setup();
        let trigger_controller = PlayerId(0);
        let a1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A1".to_string(),
            Zone::Battlefield,
        );
        let a2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "A2".to_string(),
            Zone::Battlefield,
        );
        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![a1, a2],
            defending_player: PlayerId(1),
            attacks: vec![
                (a1, crate::game::combat::AttackTarget::Player(PlayerId(1))),
                (a2, crate::game::combat::AttackTarget::Player(PlayerId(1))),
            ],
        };
        let cond = TriggerCondition::AttackersDeclaredMin {
            scope: ControllerRef::You,
            minimum: 2,
        };
        assert!(check_trigger_condition(
            &state,
            &cond,
            trigger_controller,
            None,
            Some(&event),
        ));

        // Raising the threshold to 3 → condition fails.
        let cond3 = TriggerCondition::AttackersDeclaredMin {
            scope: ControllerRef::You,
            minimum: 3,
        };
        assert!(!check_trigger_condition(
            &state,
            &cond3,
            trigger_controller,
            None,
            Some(&event),
        ));
    }

    #[test]
    fn attackers_declared_min_opponent_scope_ignores_your_attackers() {
        let mut state = setup();
        let trigger_controller = PlayerId(0);
        // Attackers controlled by the trigger controller — Opponent scope must NOT count them.
        let a1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A1".to_string(),
            Zone::Battlefield,
        );
        let a2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "A2".to_string(),
            Zone::Battlefield,
        );
        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![a1, a2],
            defending_player: PlayerId(1),
            attacks: vec![
                (a1, crate::game::combat::AttackTarget::Player(PlayerId(1))),
                (a2, crate::game::combat::AttackTarget::Player(PlayerId(1))),
            ],
        };
        let cond = TriggerCondition::AttackersDeclaredMin {
            scope: ControllerRef::Opponent,
            minimum: 2,
        };
        assert!(!check_trigger_condition(
            &state,
            &cond,
            trigger_controller,
            None,
            Some(&event),
        ));
    }

    // CR 506.2 + CR 508.1b: Unit tests for `NoneOfAttackersTargetedYou`.
    #[test]
    fn none_of_attackers_targeted_you_true_when_all_attack_elsewhere() {
        let mut state = setup();
        let trigger_controller = PlayerId(0);
        // Opponent's attackers — both attacking a third party (not the trigger controller).
        let a1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "A1".to_string(),
            Zone::Battlefield,
        );
        let a2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "A2".to_string(),
            Zone::Battlefield,
        );
        // A planeswalker controlled by the trigger controller — attackers targeting this
        // planeswalker should NOT trip the "attacked you" condition (CR 506.2a).
        let pw = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "PW".to_string(),
            Zone::Battlefield,
        );
        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![a1, a2],
            defending_player: PlayerId(0),
            attacks: vec![
                (a1, crate::game::combat::AttackTarget::Planeswalker(pw)),
                (a2, crate::game::combat::AttackTarget::Planeswalker(pw)),
            ],
        };
        let cond = TriggerCondition::NoneOfAttackersTargetedYou;
        assert!(check_trigger_condition(
            &state,
            &cond,
            trigger_controller,
            None,
            Some(&event),
        ));
    }

    #[test]
    fn none_of_attackers_targeted_you_false_when_one_attacks_you() {
        let mut state = setup();
        let trigger_controller = PlayerId(0);
        let a1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "A1".to_string(),
            Zone::Battlefield,
        );
        let a2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "A2".to_string(),
            Zone::Battlefield,
        );
        let pw = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "PW".to_string(),
            Zone::Battlefield,
        );
        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![a1, a2],
            defending_player: PlayerId(0),
            attacks: vec![
                (a1, crate::game::combat::AttackTarget::Planeswalker(pw)),
                (
                    a2,
                    crate::game::combat::AttackTarget::Player(trigger_controller),
                ),
            ],
        };
        let cond = TriggerCondition::NoneOfAttackersTargetedYou;
        assert!(!check_trigger_condition(
            &state,
            &cond,
            trigger_controller,
            None,
            Some(&event),
        ));
    }

    /// Regression tests for `TriggerCondition::WasCast` — the condition backing
    /// "if you cast it" intervening-if clauses. For ETB-based triggers whose
    /// source is a separate permanent (e.g. Light-Paws, Emperor's Voice:
    /// "Whenever an Aura you control enters, if you cast it..."), the check
    /// must inspect the entering object from the `ZoneChanged` event rather
    /// than the trigger source. CR 601.2 / CR 603.4.
    #[test]
    fn was_cast_uses_entering_object_from_zone_changed_event() {
        let mut state = setup();
        // Light-Paws is on the battlefield; the Aura is the entering object.
        let light_paws = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Light-Paws".to_string(),
            Zone::Battlefield,
        );
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Aura".to_string(),
            Zone::Battlefield,
        );
        // Aura was cast from hand — cast_from_zone is Some.
        state.objects.get_mut(&aura).unwrap().cast_from_zone = Some(Zone::Hand);
        // Light-Paws was NOT cast this ETB event (it's been in play).
        state.objects.get_mut(&light_paws).unwrap().cast_from_zone = None;

        let event = zone_changed_event(
            aura,
            Zone::Stack,
            Zone::Battlefield,
            vec![CoreType::Enchantment],
            vec!["Aura"],
        );

        // source_id = Light-Paws, but entering object in the event is the Aura.
        // WasCast must read the Aura's cast_from_zone, not Light-Paws's.
        assert!(check_trigger_condition(
            &state,
            &TriggerCondition::WasCast,
            PlayerId(0),
            Some(light_paws),
            Some(&event),
        ));
    }

    #[test]
    fn was_cast_false_when_aura_put_onto_battlefield_not_cast() {
        let mut state = setup();
        let light_paws = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Light-Paws".to_string(),
            Zone::Battlefield,
        );
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Aura".to_string(),
            Zone::Battlefield,
        );
        // Aura entered via reanimation / Academy Rector-style "put onto battlefield".
        state.objects.get_mut(&aura).unwrap().cast_from_zone = None;
        state.objects.get_mut(&light_paws).unwrap().cast_from_zone = None;

        let event = zone_changed_event(
            aura,
            Zone::Graveyard,
            Zone::Battlefield,
            vec![CoreType::Enchantment],
            vec!["Aura"],
        );

        assert!(!check_trigger_condition(
            &state,
            &TriggerCondition::WasCast,
            PlayerId(0),
            Some(light_paws),
            Some(&event),
        ));
    }

    #[test]
    fn was_cast_self_referential_falls_back_to_source_id() {
        // Cascade / Discover-style: the trigger source IS the cast spell,
        // and no ZoneChanged event is attached (SpellCast event instead).
        // WasCast should fall back to source_id.
        let mut state = setup();
        let cast_spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Cast Spell".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&cast_spell).unwrap().cast_from_zone = Some(Zone::Hand);

        assert!(check_trigger_condition(
            &state,
            &TriggerCondition::WasCast,
            PlayerId(0),
            Some(cast_spell),
            None,
        ));

        // And false when the self-referential source was not cast.
        state.objects.get_mut(&cast_spell).unwrap().cast_from_zone = None;
        assert!(!check_trigger_condition(
            &state,
            &TriggerCondition::WasCast,
            PlayerId(0),
            Some(cast_spell),
            None,
        ));
    }

    /// CR 121.1 + CR 504.1 + CR 603.4 — `ExceptFirstDrawInDrawStep` gates
    /// Orcish Bowmasters' trigger so the active player's mandatory first draw
    /// of their draw step does NOT fire it. Subsequent draws (extra draws,
    /// any draws outside the draw step, opponent draws during their own draw
    /// step's mandatory first draw, etc.) all fire normally.
    #[test]
    fn except_first_draw_in_draw_step_suppresses_only_active_first_draw() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.phase = Phase::Draw;
        let controller = PlayerId(1); // Bowmasters' controller (the opponent)
        let condition = TriggerCondition::ExceptFirstDrawInDrawStep;

        // Active player (P0) drawing their FIRST card of the draw step → suppress.
        let first_draw = GameEvent::CardDrawn {
            player_id: PlayerId(0),
            object_id: ObjectId(50),
            nth_in_step: 1,
        };
        assert!(
            !check_trigger_condition(&state, &condition, controller, None, Some(&first_draw)),
            "the mandatory first draw of the active player's draw step must NOT fire"
        );

        // Same active player drawing a SECOND card during their draw step → fire.
        let extra_draw = GameEvent::CardDrawn {
            player_id: PlayerId(0),
            object_id: ObjectId(51),
            nth_in_step: 2,
        };
        assert!(
            check_trigger_condition(&state, &condition, controller, None, Some(&extra_draw)),
            "any subsequent draw during the active player's draw step must fire"
        );

        // Outside the draw step — first draw of a different step still fires.
        state.phase = Phase::Upkeep;
        let upkeep_first = GameEvent::CardDrawn {
            player_id: PlayerId(0),
            object_id: ObjectId(52),
            nth_in_step: 1,
        };
        assert!(
            check_trigger_condition(&state, &condition, controller, None, Some(&upkeep_first)),
            "first draw outside the draw step must fire"
        );

        // Back in draw step but the NON-active player draws first (e.g., a
        // forced draw on the opponent during the active player's draw step).
        // The exception only excuses the active player's mandatory draw, so a
        // draw by anyone else still fires the trigger.
        state.phase = Phase::Draw;
        let opponent_draw = GameEvent::CardDrawn {
            player_id: PlayerId(1),
            object_id: ObjectId(53),
            nth_in_step: 1,
        };
        assert!(
            check_trigger_condition(&state, &condition, controller, None, Some(&opponent_draw)),
            "draw step draws by the non-active player must fire"
        );
    }

    // === L9-23: Sliver-lord self-static keyword/trigger grant ===
    // CR 603.6a + CR 611.2e: Static abilities that grant abilities/keywords to
    // a class of permanents apply the moment a newcomer enters the battlefield.
    // ETB-trigger gathering MUST see the granted-trigger on the entering object
    // itself (Harmonic Sliver) and the granted keyword on the entering source
    // (Venom Sliver / sliver-lord pattern). The fix flushes pending layer
    // evaluation at the top of `process_triggers`.

    /// Helper: create a battlefield Sliver creature owned by `controller` with
    /// a `Sliver` subtype tag, ready for layer evaluation.
    fn make_sliver(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(0xB1A1),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Sliver".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(0);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        id
    }

    #[test]
    fn harmonic_sliver_self_etb_trigger_via_own_static_grant() {
        // CR 603.6a: Each time an event puts one or more permanents onto the
        // battlefield, all permanents on the battlefield (INCLUDING the
        // newcomers) are checked for any ETB triggers that match the event.
        // Harmonic Sliver's printed static "All Slivers have 'When this
        // permanent enters, destroy target ...'" grants its own ETB trigger
        // back to itself. The granted trigger MUST fire on Harmonic Sliver's
        // own ETB.
        let mut state = setup();
        state.active_player = PlayerId(0);

        let harmonic = make_sliver(&mut state, PlayerId(0), "Harmonic Sliver");

        // Static: "Creature & Sliver" => GrantTrigger(ChangesZone -> Battlefield, SelfRef, Draw 1).
        // We use Draw rather than Destroy to keep the test free of target prompts.
        let granted_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                    up_to: false,
                },
            ));
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![
                    TypeFilter::Creature,
                    TypeFilter::Subtype("Sliver".to_string()),
                ],
                controller: None,
                properties: Vec::new(),
            }))
            .modifications(vec![
                crate::types::ability::ContinuousModification::GrantTrigger {
                    trigger: Box::new(granted_trigger),
                },
            ]);
        let obj = state.objects.get_mut(&harmonic).unwrap();
        obj.static_definitions.push(static_def.clone());
        std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(static_def);

        // Layers haven't run yet — granted trigger is NOT on obj.trigger_definitions
        // until we evaluate. The fix in process_triggers must flush layers first.
        state.layers_dirty = true;

        let events = vec![zone_changed_event(
            harmonic,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            vec!["Sliver"],
        )];
        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            1,
            "Harmonic Sliver's own ETB must trigger the granted ability per CR 603.6a"
        );
    }

    #[test]
    fn other_sliver_etb_triggers_via_lord_grant() {
        // Two slivers on the battlefield: Lord (with the static) is already in
        // play; a new Sliver enters. The lord's grant must apply to the
        // newcomer so that the newcomer's own ETB fires the granted trigger.
        let mut state = setup();
        state.active_player = PlayerId(0);

        let lord = make_sliver(&mut state, PlayerId(0), "Lord Sliver");
        let granted_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                    up_to: false,
                },
            ));
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![
                    TypeFilter::Creature,
                    TypeFilter::Subtype("Sliver".to_string()),
                ],
                controller: None,
                properties: Vec::new(),
            }))
            .modifications(vec![
                crate::types::ability::ContinuousModification::GrantTrigger {
                    trigger: Box::new(granted_trigger),
                },
            ]);
        let lord_obj = state.objects.get_mut(&lord).unwrap();
        lord_obj.static_definitions.push(static_def.clone());
        std::sync::Arc::make_mut(&mut lord_obj.base_static_definitions).push(static_def);

        // Newcomer Sliver enters — both lord and newcomer should get the grant
        // applied via layers, and the newcomer's ETB must fire the granted
        // trigger from the newcomer (not from the lord, which already ETB'd).
        let newcomer = make_sliver(&mut state, PlayerId(0), "Other Sliver");
        state.layers_dirty = true;

        let events = vec![zone_changed_event(
            newcomer,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            vec!["Sliver"],
        )];
        process_triggers(&mut state, &events);

        // Both slivers (lord + newcomer) have the granted trigger via layers.
        // Per CR 603.6a only the newcomer matches the ETB event with
        // valid_card=SelfRef, so exactly one trigger fires.
        assert_eq!(
            state.stack.len(),
            1,
            "newcomer Sliver's own ETB must fire the granted self-ETB trigger exactly once"
        );
    }

    #[test]
    fn non_sliver_etb_does_not_fire_lord_grant() {
        // Negative regression: the lord's grant must not extend to a
        // non-Sliver creature. Layers correctly filter the affected set; this
        // test pins that behaviour after the layer-flush change.
        let mut state = setup();
        state.active_player = PlayerId(0);

        let lord = make_sliver(&mut state, PlayerId(0), "Lord Sliver");
        let granted_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                    up_to: false,
                },
            ));
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![
                    TypeFilter::Creature,
                    TypeFilter::Subtype("Sliver".to_string()),
                ],
                controller: None,
                properties: Vec::new(),
            }))
            .modifications(vec![
                crate::types::ability::ContinuousModification::GrantTrigger {
                    trigger: Box::new(granted_trigger),
                },
            ]);
        let lord_obj = state.objects.get_mut(&lord).unwrap();
        lord_obj.static_definitions.push(static_def.clone());
        std::sync::Arc::make_mut(&mut lord_obj.base_static_definitions).push(static_def);

        // Non-Sliver creature enters.
        let bear = create_object(
            &mut state,
            CardId(0xBEA1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Bear".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.entered_battlefield_turn = Some(0);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
        state.layers_dirty = true;

        let events = vec![zone_changed_event(
            bear,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            vec!["Bear"],
        )];
        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            0,
            "non-Sliver creature must not fire the lord's grant"
        );
    }

    #[test]
    fn venom_sliver_self_grants_deathtouch_via_layer_flush_in_process_triggers() {
        // CR 611.2e: Venom Sliver pattern — a printed static "Sliver creatures
        // you control have deathtouch" must apply to the source itself once
        // layers are evaluated. Pins that calling `process_triggers` (which
        // happens immediately after a zone change in the post-action pipeline)
        // flushes pending layer evaluation so the granted keyword is on
        // `obj.keywords` for any subsequent combat-damage or trigger check
        // that reads it.
        let mut state = setup();
        state.active_player = PlayerId(0);

        let venom = make_sliver(&mut state, PlayerId(0), "Venom Sliver");

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![
                    TypeFilter::Creature,
                    TypeFilter::Subtype("Sliver".to_string()),
                ],
                controller: Some(ControllerRef::You),
                properties: Vec::new(),
            }))
            .modifications(vec![
                crate::types::ability::ContinuousModification::AddKeyword {
                    keyword: Keyword::Deathtouch,
                },
            ]);
        let obj = state.objects.get_mut(&venom).unwrap();
        obj.static_definitions.push(static_def.clone());
        std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(static_def);

        // Before any layer evaluation, the keyword is NOT on obj.keywords.
        assert!(
            !state
                .objects
                .get(&venom)
                .unwrap()
                .has_keyword(&Keyword::Deathtouch),
            "precondition: keyword absent until layers run"
        );

        // Drive the post-action trigger scan: process_triggers must flush
        // layers before scanning so granted keywords are visible.
        state.layers_dirty = true;
        process_triggers(&mut state, &[]);

        assert!(
            state
                .objects
                .get(&venom)
                .unwrap()
                .has_keyword(&Keyword::Deathtouch),
            "Venom Sliver self-grants deathtouch once layers run via process_triggers"
        );
    }
}

/// Regression tests for the foundational trigger double-fire defect
/// (CR 603.2 / CR 603.3 per-event registration dedup). Every trigger
/// category must register at most once per `(source_id, trig_idx, event)`
/// tuple, even when multiple zone-scan paths visit the same object.
#[cfg(test)]
mod dedup_regression_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TargetRef,
        TriggerDefinition,
    };
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{GameState, ZoneChangeRecord};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// Build a minimal `Draw 1` triggered ability that matches a given mode.
    fn draw_one_trigger(mode: TriggerMode) -> TriggerDefinition {
        TriggerDefinition::new(mode)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                    up_to: false,
                },
            ))
    }

    fn setup_with_observer(mode: TriggerMode) -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let observer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Observer".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            // Self-ref-only valid_card would restrict to ETB of self; for observer
            // triggers we want to match any qualifying event. Swap to TargetFilter::Any.
            let mut trigger = draw_one_trigger(mode);
            trigger.valid_card = Some(TargetFilter::Any);
            obj.trigger_definitions.push(trigger);
        }
        (state, observer)
    }

    /// ETB observer trigger: one creature entering produces exactly one trigger.
    /// Regression: Mischievous Mystic's ETB trigger used to double-register when
    /// synthesis ran twice, producing two tokens from one ETB.
    #[test]
    fn etb_observer_fires_once_per_event() {
        let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
        state
            .objects
            .get_mut(&observer)
            .unwrap()
            .trigger_definitions[0]
            .destination = Some(Zone::Battlefield);

        let new_etb = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Newcomer".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&new_etb)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let event = GameEvent::ZoneChanged {
            object_id: new_etb,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                new_etb,
                Some(Zone::Hand),
                Zone::Battlefield,
            )),
        };

        process_triggers(&mut state, &[event]);
        assert_eq!(
            state.stack.len(),
            1,
            "ETB observer should register exactly one trigger per ETB event"
        );
    }

    /// Attacks observer: a non-batched "whenever a creature attacks" trigger
    /// registers once per AttackersDeclared event. Regression: Najeela-style
    /// triggers registered multiply when zone scanners double-visited.
    #[test]
    fn attacks_observer_fires_once_per_event() {
        let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
        let attacker = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker],
            defending_player: PlayerId(1),
            attacks: vec![(
                attacker,
                crate::game::combat::AttackTarget::Player(PlayerId(1)),
            )],
        };

        process_triggers(&mut state, &[event]);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 1,
            "Attack observer should register exactly one trigger per AttackersDeclared"
        );
    }

    /// SpellCast observer: spell-cast triggers register once per SpellCast event.
    #[test]
    fn spell_cast_observer_fires_once_per_event() {
        let (mut state, observer) = setup_with_observer(TriggerMode::SpellCast);
        let spell = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let event = GameEvent::SpellCast {
            card_id: CardId(4),
            controller: PlayerId(0),
            object_id: spell,
        };

        process_triggers(&mut state, &[event]);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 1,
            "SpellCast observer should register exactly one trigger per SpellCast event"
        );
    }

    /// DamageDealt observer: damage-event triggers register once per DamageDealt.
    /// Regression: Mana Cannons damage fired 4-6× due to multi-path zone scans.
    #[test]
    fn damage_observer_fires_once_per_event() {
        let (mut state, observer) = setup_with_observer(TriggerMode::DamageDone);
        let source = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Damage Source".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let event = GameEvent::DamageDealt {
            source_id: source,
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            excess: 0,
        };

        process_triggers(&mut state, &[event]);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 1,
            "DamageDone observer should register exactly one trigger per DamageDealt event"
        );
    }

    /// Sacrifice observer: "whenever a permanent is sacrificed" fires once per
    /// PermanentSacrificed event, not once per zone scan.
    #[test]
    fn sacrifice_observer_fires_once_per_event() {
        let (mut state, observer) = setup_with_observer(TriggerMode::Sacrificed);
        let victim = create_object(
            &mut state,
            CardId(6),
            PlayerId(0),
            "Victim".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&victim)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let event = GameEvent::PermanentSacrificed {
            object_id: victim,
            player_id: PlayerId(0),
        };

        process_triggers(&mut state, &[event]);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 1,
            "Sacrifice observer should register exactly one trigger per PermanentSacrificed"
        );
    }

    /// Landfall: "whenever a land enters the battlefield under your control"
    /// fires once per land ETB. Regression: Icetill Explorer's landfall fired
    /// multiple times when multi-zone scans visited the same trigger_def.
    #[test]
    fn landfall_fires_once_per_land_etb() {
        let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
        state
            .objects
            .get_mut(&observer)
            .unwrap()
            .trigger_definitions[0]
            .destination = Some(Zone::Battlefield);
        // Narrow the valid_card to lands to mimic landfall's filter.
        state
            .objects
            .get_mut(&observer)
            .unwrap()
            .trigger_definitions[0]
            .valid_card = Some(TargetFilter::Typed(
            crate::types::ability::TypedFilter::land(),
        ));

        let land = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let event = GameEvent::ZoneChanged {
            object_id: land,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                name: "Mountain".to_string(),
                core_types: vec![CoreType::Land],
                subtypes: vec!["Mountain".to_string()],
                ..ZoneChangeRecord::test_minimal(land, Some(Zone::Hand), Zone::Battlefield)
            }),
        };

        process_triggers(&mut state, &[event]);
        assert_eq!(
            state.stack.len(),
            1,
            "Landfall should register exactly one trigger per land ETB"
        );
    }

    /// Panharmonicon-style trigger doubling must still produce exactly 2 stack
    /// instances from 1 matching event — the per-event dedup applies to
    /// *registration* of trigger definitions, not to the post-registration
    /// `apply_trigger_doubling` cloning pass.
    #[test]
    fn panharmonicon_still_doubles_after_dedup() {
        use crate::types::ability::ControllerRef;
        use crate::types::statics::{StaticMode, TriggerCause};

        let (mut state, _observer) = setup_with_observer(TriggerMode::ChangesZone);
        // Scope the observer trigger to ETB.
        // Find the first battlefield object (our observer) to seed.
        let observer_id = *state.battlefield.iter().next().unwrap();
        state
            .objects
            .get_mut(&observer_id)
            .unwrap()
            .trigger_definitions[0]
            .destination = Some(Zone::Battlefield);

        // Put a Panharmonicon on the battlefield with its static.
        let panh = create_object(
            &mut state,
            CardId(8),
            PlayerId(0),
            "Panharmonicon".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&panh).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.static_definitions.push(
                crate::types::ability::StaticDefinition::new(StaticMode::DoubleTriggers {
                    cause: TriggerCause::EntersBattlefield {
                        core_types: vec![CoreType::Artifact, CoreType::Creature],
                    },
                })
                .affected(TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature().controller(ControllerRef::You),
                )),
            );
        }

        // A creature enters.
        let new_etb = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Entering Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&new_etb)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let event = GameEvent::ZoneChanged {
            object_id: new_etb,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                name: "Entering Creature".to_string(),
                core_types: vec![CoreType::Creature],
                ..ZoneChangeRecord::test_minimal(new_etb, Some(Zone::Hand), Zone::Battlefield)
            }),
        };

        process_triggers(&mut state, &[event]);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer_id)
            .count();
        assert_eq!(
            observer_triggers, 2,
            "Panharmonicon must still double the observer's ETB trigger to 2 instances"
        );
    }

    /// Helper: install a `DoubleTriggers` static on a new battlefield object
    /// with the supplied cause, controlled by PlayerId(0).
    fn install_doubler(state: &mut GameState, cause: TriggerCause) -> ObjectId {
        use crate::types::statics::StaticMode;
        let id = create_object(
            state,
            CardId(100),
            PlayerId(0),
            "Doubler".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.static_definitions
            .push(crate::types::ability::StaticDefinition::new(
                StaticMode::DoubleTriggers { cause },
            ));
        id
    }

    /// CR 603.2d: Isshin (CreatureAttacking cause) doubles attack triggers
    /// of a permanent the controller owns.
    #[test]
    fn isshin_doubles_attack_triggers() {
        use crate::types::statics::TriggerCause;

        let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
        let _isshin = install_doubler(&mut state, TriggerCause::CreatureAttacking);

        // Ensure observer is a creature so it can attack and its trigger is for ITS attack.
        state
            .objects
            .get_mut(&observer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![observer],
            defending_player: PlayerId(1),
            attacks: vec![(
                observer,
                crate::game::combat::AttackTarget::Player(PlayerId(1)),
            )],
        };

        process_triggers(&mut state, &[event]);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 2,
            "Isshin must double the observer's attack trigger to 2 instances"
        );
    }

    /// CR 603.2d: Isshin does NOT double ETB triggers — the cause predicate
    /// is `CreatureAttacking`, not `EntersBattlefield`.
    #[test]
    fn isshin_does_not_double_etb_triggers() {
        use crate::types::statics::TriggerCause;

        let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
        state
            .objects
            .get_mut(&observer)
            .unwrap()
            .trigger_definitions[0]
            .destination = Some(Zone::Battlefield);
        let _isshin = install_doubler(&mut state, TriggerCause::CreatureAttacking);

        let new_etb = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Entering Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&new_etb)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let event = GameEvent::ZoneChanged {
            object_id: new_etb,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                name: "Entering Creature".to_string(),
                core_types: vec![CoreType::Creature],
                ..ZoneChangeRecord::test_minimal(new_etb, Some(Zone::Hand), Zone::Battlefield)
            }),
        };

        process_triggers(&mut state, &[event]);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 1,
            "Isshin must NOT double ETB triggers — cause is CreatureAttacking"
        );
    }

    /// CR 603.2d: Panharmonicon (EntersBattlefield cause) does NOT double
    /// attack triggers — the cause predicate filters to ETB only.
    #[test]
    fn panharmonicon_does_not_double_attack_triggers() {
        use crate::types::statics::TriggerCause;

        let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
        state
            .objects
            .get_mut(&observer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let _panh = install_doubler(
            &mut state,
            TriggerCause::EntersBattlefield {
                core_types: vec![CoreType::Artifact, CoreType::Creature],
            },
        );

        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![observer],
            defending_player: PlayerId(1),
            attacks: vec![(
                observer,
                crate::game::combat::AttackTarget::Player(PlayerId(1)),
            )],
        };

        process_triggers(&mut state, &[event]);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 1,
            "Panharmonicon must NOT double attack triggers — cause is EntersBattlefield"
        );
    }

    /// CR 603.2d: Isshin + Panharmonicon — only Isshin matches an attack
    /// event, so the total is 2 (original + 1 from Isshin).
    #[test]
    fn isshin_and_panharmonicon_only_isshin_matches_attack_event() {
        use crate::types::statics::TriggerCause;

        let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
        state
            .objects
            .get_mut(&observer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let _isshin = install_doubler(&mut state, TriggerCause::CreatureAttacking);
        let _panh = install_doubler(
            &mut state,
            TriggerCause::EntersBattlefield {
                core_types: vec![CoreType::Artifact, CoreType::Creature],
            },
        );

        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![observer],
            defending_player: PlayerId(1),
            attacks: vec![(
                observer,
                crate::game::combat::AttackTarget::Player(PlayerId(1)),
            )],
        };

        process_triggers(&mut state, &[event]);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 2,
            "Only Isshin's cause matches the attack event — total should be 2 (original + 1 clone)"
        );
    }

    /// CR 603.2d + CR 603.6c: Drivnod (CreatureDying cause) doubles a
    /// dies-triggered ability of a permanent the controller owns.
    #[test]
    fn drivnod_doubles_dies_triggers() {
        use crate::types::statics::TriggerCause;

        let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
        state
            .objects
            .get_mut(&observer)
            .unwrap()
            .trigger_definitions[0]
            .destination = Some(Zone::Graveyard);
        let _drivnod = install_doubler(&mut state, TriggerCause::CreatureDying);

        let dying = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Dying Creature".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&dying)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let event = GameEvent::ZoneChanged {
            object_id: dying,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                name: "Dying Creature".to_string(),
                core_types: vec![CoreType::Creature],
                ..ZoneChangeRecord::test_minimal(dying, Some(Zone::Battlefield), Zone::Graveyard)
            }),
        };

        process_triggers(&mut state, &[event]);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 2,
            "Drivnod must double the observer's dies trigger to 2 instances"
        );
    }

    /// CR 603.4 + CR 701.9: Intervening-if "if an opponent discarded a card this
    /// turn" evaluates against the per-turn discard set. Verifies both the
    /// positive (opponent discarded → condition met) and negative (no opponent
    /// discarded → condition unmet, as well as only-controller-discarded →
    /// condition unmet) paths for Tinybones, Trinket Thief.
    #[test]
    fn intervening_if_opponent_discarded_this_turn_gates_trigger() {
        use crate::types::ability::{Comparator, QuantityExpr, QuantityRef, TriggerCondition};

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let opponent = PlayerId(1);

        let condition = TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::OpponentDiscardedCardThisTurn,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        };

        // No one has discarded yet → condition not met.
        assert!(
            !check_trigger_condition(&state, &condition, controller, None, None),
            "empty discard set must fail the intervening-if"
        );

        // Only the controller discarded → still no opponent discard → condition unmet.
        state
            .players_who_discarded_card_this_turn
            .insert(controller);
        assert!(
            !check_trigger_condition(&state, &condition, controller, None, None),
            "self-discard must not satisfy 'an opponent discarded a card this turn'"
        );

        // Opponent discarded → condition met.
        state.players_who_discarded_card_this_turn.insert(opponent);
        assert!(
            check_trigger_condition(&state, &condition, controller, None, None),
            "opponent-discard must satisfy 'an opponent discarded a card this turn'"
        );
    }

    /// CR 603.4 + CR 109.3: Valakut-style "if you control at least five other
    /// Mountains" must exclude the triggering (newly-entered) Mountain from the
    /// count. With exactly 5 Mountains on the battlefield where one of them is
    /// the trigger object, the condition is *not* met (only 4 "other" Mountains).
    /// With 6 Mountains (5 others + triggering), the condition *is* met.
    #[test]
    fn intervening_if_other_than_trigger_object_excludes_triggering_mountain() {
        use crate::types::ability::{
            Comparator, ControllerRef, FilterProp, QuantityExpr, QuantityRef, TargetFilter,
            TriggerCondition, TypeFilter, TypedFilter,
        };

        // Helper: create a Mountain on the battlefield under `player`.
        fn make_mountain(state: &mut GameState, player: PlayerId, n: usize) -> ObjectId {
            let id = create_object(
                state,
                CardId(0),
                player,
                format!("Mountain {n}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Mountain".to_string());
            obj.base_card_types = obj.card_types.clone();
            id
        }

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);

        // Valakut source (not a Mountain subtype).
        let valakut_id = create_object(
            &mut state,
            CardId(1),
            controller,
            "Valakut".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&valakut_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.base_card_types = obj.card_types.clone();
        }
        // 4 pre-existing Mountains.
        for n in 0..4 {
            make_mountain(&mut state, controller, n);
        }
        // The triggering (newly-entered) Mountain — 5th Mountain total.
        let trigger_id = make_mountain(&mut state, controller, 100);

        let condition = TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Subtype("Mountain".to_string())],
                        controller: Some(ControllerRef::You),
                        properties: vec![FilterProp::OtherThanTriggerObject],
                    }),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 5 },
        };

        let event = GameEvent::ZoneChanged {
            object_id: trigger_id,
            from: Some(Zone::Library),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                trigger_id,
                Some(Zone::Library),
                Zone::Battlefield,
            )),
        };

        // 4 other Mountains + 1 triggering = 5 total. Excluding the triggering
        // Mountain leaves 4, which is NOT ≥ 5 — the trigger condition must fail.
        assert!(
            !check_trigger_condition(
                &state,
                &condition,
                controller,
                Some(valakut_id),
                Some(&event)
            ),
            "with only 4 other Mountains, the condition must fail"
        );

        // Add a 5th non-triggering Mountain → 5 others + 1 triggering = 6 total.
        make_mountain(&mut state, controller, 200);
        assert!(
            check_trigger_condition(
                &state,
                &condition,
                controller,
                Some(valakut_id),
                Some(&event)
            ),
            "with 5 other Mountains, the condition must pass"
        );
    }
}
