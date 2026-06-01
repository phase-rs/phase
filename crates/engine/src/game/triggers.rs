use std::collections::HashSet;

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, BounceSelection, ChosenAttribute,
    CommanderOwnership, ControllerRef, CopyRetargetPermission, DelayedTriggerCondition, Effect,
    ModalChoice, PlayerFilter, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
    TributeOutcome, TriggerCondition, TriggerDefinition, TypeFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::events::{GameEvent, ManaTapState};
use crate::types::game_state::{
    DelayedTrigger, DistributionUnit, GameState, MayTriggerOrigin, StackEntry, StackEntryKind,
    TargetSelectionConstraint,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::WardCost;
use crate::types::keywords::{Keyword, KeywordKind};
use crate::types::phase::Phase;
use crate::types::player::{Player, PlayerCounterKind, PlayerId};
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
pub use super::trigger_matchers::{build_trigger_registry, trigger_matcher, trigger_registry};

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
    /// CR 601.2d + CR 603.3d: Trigger controllers divide distributed effects
    /// while putting the triggered ability on the stack, after targets are known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribute: Option<DistributionUnit>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub may_trigger_origin: Option<MayTriggerOrigin>,
    /// CR 603.2c: For batched triggers with a `valid_card` filter, the count
    /// of subjects in the firing event batch that satisfied the filter. Flows
    /// from `collect_matching_triggers` →
    /// `push_pending_trigger_to_stack_with_event_batch` →
    /// `StackEntryKind::TriggeredAbility.subject_match_count`. `None` for
    /// non-batched triggers and for batched triggers without a `valid_card`
    /// filter (or `valid_card: SelfRef`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_match_count: Option<u32>,
}

pub(super) struct TriggerEventContextSnapshot {
    current_trigger_event: Option<GameEvent>,
    current_trigger_events: Vec<GameEvent>,
    current_trigger_match_count: Option<u32>,
}

pub(super) fn push_trigger_event_context(
    state: &mut GameState,
    trigger_event: Option<&GameEvent>,
    trigger_events: &[GameEvent],
    subject_match_count: Option<u32>,
) -> TriggerEventContextSnapshot {
    let snapshot = TriggerEventContextSnapshot {
        current_trigger_event: state.current_trigger_event.clone(),
        current_trigger_events: state.current_trigger_events.clone(),
        current_trigger_match_count: state.current_trigger_match_count,
    };
    state.current_trigger_event = trigger_event
        .cloned()
        .or_else(|| trigger_events.first().cloned());
    state.current_trigger_events = if trigger_events.is_empty() {
        trigger_event.cloned().into_iter().collect()
    } else {
        trigger_events.to_vec()
    };
    state.current_trigger_match_count = subject_match_count;
    snapshot
}

pub(super) fn restore_trigger_event_context(
    state: &mut GameState,
    snapshot: TriggerEventContextSnapshot,
) {
    state.current_trigger_event = snapshot.current_trigger_event;
    state.current_trigger_events = snapshot.current_trigger_events;
    state.current_trigger_match_count = snapshot.current_trigger_match_count;
}

/// CR 702.21a + CR 118.12: Convert a WardCost to an `AbilityCost` for the
/// counter effect's `unless_pay` modifier. Post-fold, ward and counter share
/// the unified `AbilityCost` taxonomy.
fn ward_cost_to_ability_cost(ward_cost: &WardCost) -> AbilityCost {
    match ward_cost {
        WardCost::Mana(mana_cost) => AbilityCost::Mana {
            cost: mana_cost.clone(),
        },
        WardCost::PayLife(amount) => AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: *amount },
        },
        WardCost::DiscardCard => AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            random: false,
            self_ref: false,
        },
        WardCost::Sacrifice { count, filter } => AbilityCost::Sacrifice {
            target: filter.clone(),
            count: *count,
        },
        // CR 702.21a + CR 701.67: Waterbend ward cost maps to mana payment.
        // Full tap-to-help semantics deferred to waterbend cost integration.
        WardCost::Waterbend(mana_cost) => AbilityCost::Mana {
            cost: mana_cost.clone(),
        },
        // CR 702.21a: Compound ward cost — use the first mana component as
        // the unless cost. Full compound cost resolution deferred to ward
        // cost payment integration.
        WardCost::Compound(costs) => {
            if let Some(first) = costs.first() {
                ward_cost_to_ability_cost(first)
            } else {
                AbilityCost::Mana {
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
    trigger_events: Vec<GameEvent>,
    batched: bool,
    constraint: Option<crate::types::ability::TriggerConstraint>,
}

/// A trigger that has been collected and is queued for stack placement.
///
/// CR 113.2c + CR 603.2 + CR 603.3b: Each instance of a printed triggered
/// ability fires independently. When two or more triggers fire in the same
/// pass and one of them needs player input (modal choice, target selection,
/// or division), the others must NOT be dropped — they wait in
/// `GameState::deferred_triggers` and are drained after the active trigger
/// is pushed to the stack.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingTriggerContext {
    pub pending: PendingTrigger,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trigger_events: Vec<GameEvent>,
}

/// Public alias for the deferred-queue element type used by `GameState`.
pub type DeferredTrigger = PendingTriggerContext;

impl PendingTriggerContext {
    fn single(pending: PendingTrigger) -> Self {
        let trigger_events = pending.trigger_event.iter().cloned().collect();
        Self {
            pending,
            trigger_events,
        }
    }

    fn batched(pending: PendingTrigger, trigger_events: Vec<GameEvent>) -> Self {
        Self {
            pending,
            trigger_events,
        }
    }
}

fn matching_batched_trigger_events(
    state: &GameState,
    event_batch: &[GameEvent],
    trig_def: &TriggerDefinition,
    obj_id: ObjectId,
    controller: PlayerId,
    matcher: TriggerMatcher,
) -> Vec<GameEvent> {
    event_batch
        .iter()
        .filter(|candidate| !event_is_suppressed_by_static_triggers(state, candidate))
        .filter(|candidate| matcher(candidate, trig_def, obj_id, state))
        .filter(|candidate| {
            trig_def.condition.as_ref().is_none_or(|condition| {
                check_trigger_condition(state, condition, controller, Some(obj_id), Some(candidate))
            })
        })
        .filter_map(|candidate| {
            contextual_batched_trigger_event(state, candidate, trig_def, obj_id)
        })
        .collect()
}

fn contextual_batched_trigger_event(
    state: &GameState,
    event: &GameEvent,
    trig_def: &TriggerDefinition,
    obj_id: ObjectId,
) -> Option<GameEvent> {
    let GameEvent::AttackersDeclared {
        defending_player, ..
    } = event
    else {
        return Some(event.clone());
    };

    let matching_attacks = match trig_def.mode {
        TriggerMode::Attacks => {
            super::trigger_matchers::matching_attack_events(event, trig_def, obj_id, state)
                .into_iter()
                .flat_map(|event| match event {
                    GameEvent::AttackersDeclared { attacks, .. } => attacks,
                    _ => Vec::new(),
                })
                .collect()
        }
        TriggerMode::YouAttack => {
            super::trigger_matchers::matching_you_attack_pairs(event, trig_def, obj_id, state)
        }
        _ => return Some(event.clone()),
    };

    let matching_attackers: Vec<_> = matching_attacks
        .iter()
        .map(|(attacker_id, _)| *attacker_id)
        .collect();
    if matching_attackers.is_empty() {
        return None;
    }

    // CR 603.2c + CR 608.2c: batched "one or more ... attack" triggers fire
    // once, but later "that many" text refers to the members of that matching
    // event subset, not every attacker in the declaration.
    Some(GameEvent::AttackersDeclared {
        attacker_ids: matching_attackers,
        defending_player: *defending_player,
        attacks: matching_attacks,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_matching_triggers(
    state: &GameState,
    event: &GameEvent,
    event_batch: &[GameEvent],
    source_obj: &GameObject,
    timestamp: u32,
    zone_filter: Option<Zone>,
    batched_this_pass: &mut HashSet<(ObjectId, usize)>,
    registered_this_event: &mut HashSet<(ObjectId, usize)>,
) -> Vec<MatchedTrigger> {
    let mut pending = Vec::new();
    let obj_id = source_obj.id;
    let controller = source_obj.controller;

    // CR 604.1 + CR 702.62a: Companion triggered abilities for keywords granted
    // to an *off-zone* card. `evaluate_layers` (Layer 6) only installs
    // granted-keyword triggers onto battlefield objects; a card that gains
    // Suspend while in the exile zone (Jhoira of the Ghitu, The Tenth Doctor)
    // has its effective keyword computed by `off_zone_characteristics` but no
    // companion triggers on `obj.trigger_definitions`. Synthesize them here so
    // the off-zone trigger scan sees them, mirroring how `off_zone_characteristics`
    // synthesizes the keyword itself. The printed-keyword path is unaffected:
    // printed Suspend already carries these triggers in `base_trigger_definitions`.
    let granted_off_zone_triggers: Vec<(crate::types::keywords::KeywordKind, TriggerDefinition)> =
        if zone_filter.is_some_and(|z| z != Zone::Battlefield) {
            let base_keyword_kinds: Vec<_> =
                source_obj.base_keywords.iter().map(|k| k.kind()).collect();
            crate::game::off_zone_characteristics::effective_off_zone_keywords(state, obj_id)
                .iter()
                // Only synthesize for keywords that were *granted* (absent from
                // the printed/base set) — printed keywords already carry their
                // companion triggers via synthesis at database-build time.
                .filter(|kw| !base_keyword_kinds.contains(&kw.kind()))
                .flat_map(|kw| {
                    let kind = kw.kind();
                    crate::database::synthesis::KeywordTriggerInstaller::triggers_for(kw)
                        .into_iter()
                        .map(move |trig| (kind, trig))
                })
                .collect()
        } else {
            Vec::new()
        };

    // CR 702.26b + CR 114.4: `active_trigger_definitions` owns the phased-out /
    // command-zone gate. CR 603.4 intervening-if is still the two-point check
    // inside this function (condition block below) and at resolution.
    //
    // Synthesized off-zone granted-keyword triggers are appended after the
    // printed set with indices offset past `obj.trigger_definitions.len()` so
    // the `(obj_id, trig_idx)` dedup keys never collide with printed triggers.
    let printed_trigger_count = source_obj.trigger_definitions.len();
    let printed_triggers: Vec<(
        usize,
        &TriggerDefinition,
        Option<crate::types::keywords::KeywordKind>,
    )> = super::functioning_abilities::active_trigger_definitions(state, source_obj)
        .map(|(idx, def)| (idx, def, None))
        .collect();
    let all_triggers = printed_triggers.into_iter().chain(
        granted_off_zone_triggers
            .iter()
            .enumerate()
            .map(|(i, (kind, def))| (printed_trigger_count + i, def, Some(*kind))),
    );
    for (trig_idx, trig_def, granted_keyword_kind) in all_triggers {
        // Synthesized granted-keyword companion triggers (off-zone Suspend
        // grant) carry a keyword-keyed `MayTriggerOrigin` — the synthetic
        // `trig_idx` points past `trigger_definitions` and must not be used as
        // a `Printed` index. Printed triggers keep their stable index.
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
            if !check_trigger_constraint(state, trig_def, obj_id, trig_idx, controller, event) {
                continue;
            }
            if !trig_def.batched {
                if let Some(ref condition) = trig_def.condition {
                    if !check_trigger_condition(
                        state,
                        condition,
                        controller,
                        Some(obj_id),
                        Some(event),
                    ) {
                        continue;
                    }
                }
            }
            let mut ability = build_triggered_ability(state, trig_def, obj_id, controller);
            // CR 603.4: Stamp the printed-trigger index so per-turn resolution
            // tracking (`AbilityCondition::NthResolutionThisTurn`) can identify
            // "this ability" at resolution time.
            ability.ability_index = Some(trig_idx);
            // CR 605.4a: A `TapsForMana` triggered mana ability coupled to an
            // auto-tap event was already resolved inline during cost payment
            // (`resolve_tap_mana_triggers_inline`), which flipped the event to
            // `FromTapTriggersResolved`. The deferred scan must not resolve it
            // again. Gated on `TapsForMana` mode so `ManaAdded`-mode mana
            // abilities (not resolved at payment) still fire, on the resolved
            // marker so manual `TapLandForMana` taps (still `FromTap`) still
            // fire, and on `is_triggered_mana_ability` so non-mana `TapsForMana`
            // triggers still fire (CR 603.3).
            if matches!(trig_def.mode, TriggerMode::TapsForMana)
                && matches!(
                    event,
                    GameEvent::TappedForMana {
                        tap_state: ManaTapState::FromTapTriggersResolved,
                        ..
                    }
                )
                && super::mana_abilities::is_triggered_mana_ability(&ability, Some(event))
            {
                continue;
            }
            let (modal, mode_abilities) = trig_def
                .execute
                .as_ref()
                .map(|exec| (exec.modal.clone(), exec.mode_abilities.clone()))
                .unwrap_or_default();
            let trigger_event_batches = if trig_def.batched {
                let trigger_events = matching_batched_trigger_events(
                    state,
                    event_batch,
                    trig_def,
                    obj_id,
                    controller,
                    matcher,
                );
                if trigger_events.is_empty() {
                    continue;
                }
                vec![trigger_events]
            } else if matches!(trig_def.mode, TriggerMode::Attacks) && trig_def.condition.is_none()
            {
                super::trigger_matchers::matching_attack_events(event, trig_def, obj_id, state)
                    .into_iter()
                    .map(|trigger_event| vec![trigger_event])
                    .collect()
            } else if matches!(trig_def.mode, TriggerMode::Blocks) {
                super::trigger_matchers::matching_block_events(event, trig_def, obj_id, state)
                    .into_iter()
                    .map(|trigger_event| vec![trigger_event])
                    .collect()
            } else if matches!(trig_def.mode, TriggerMode::DamageDoneOnceByController) {
                // CR 603.2c: One aggregate combat-damage event may satisfy this
                // trigger once, while CR 608.2c makes the filtered source set
                // available to later "those creatures" instructions.
                super::trigger_matchers::matching_damage_done_once_by_controller_event(
                    event, trig_def, obj_id, state,
                )
                .into_iter()
                .map(|trigger_event| vec![trigger_event])
                .collect()
            } else {
                vec![vec![event.clone()]]
            };
            for trigger_events in trigger_event_batches {
                let trigger_event = trigger_events
                    .first()
                    .cloned()
                    .expect("trigger event batch is never empty");
                // CR 603.2c: For batched triggers, stash the filtered subject
                // count so the resolved ability's `EventContextAmount` reads
                // "that many" as the number of matching subjects (Dragons that
                // attacked, creatures that ETB'd, etc.). `None` for
                // non-batched triggers and for batched triggers without a
                // concrete `valid_card` filter.
                let subject_match_count = if trig_def.batched {
                    super::trigger_matchers::count_trigger_subjects_in_batch(
                        state,
                        trig_def.valid_card.as_ref(),
                        obj_id,
                        &trigger_events,
                    )
                } else {
                    None
                };
                pending.push(MatchedTrigger {
                    trig_idx,
                    pending: PendingTrigger {
                        source_id: obj_id,
                        controller,
                        condition: trig_def.condition.clone(),
                        ability: ability.clone(),
                        timestamp,
                        target_constraints: trig_def
                            .execute
                            .as_ref()
                            .map(|execute| execute.target_constraints.clone())
                            .unwrap_or_default(),
                        distribute: trig_def
                            .execute
                            .as_ref()
                            .and_then(|execute| execute.distribute.clone()),
                        trigger_event: Some(trigger_event),
                        modal: modal.clone(),
                        mode_abilities: mode_abilities.clone(),
                        description: trig_def.description.clone(),
                        may_trigger_origin: Some(match granted_keyword_kind {
                            Some(kind) => MayTriggerOrigin::Keyword { keyword: kind },
                            None => MayTriggerOrigin::Printed {
                                trigger_index: trig_idx,
                            },
                        }),
                        subject_match_count,
                    },
                    trigger_events,
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
        // CR 114.4 + CR 113.6b: Abilities of emblems function in the command
        // zone by default. Non-emblem command-zone objects contribute only
        // triggers that explicitly opt in via `trigger_zones` (Eminence).
        // `active_trigger_definitions` performs the per-definition filtering;
        // this source scan only needs to include objects that might have at
        // least one command-zone trigger.
        Zone::Command => state
            .command_zone
            .iter()
            .copied()
            .filter(|id| {
                state.objects.get(id).is_some_and(|o| {
                    !o.is_phased_out()
                        && (o.is_emblem
                            || o.trigger_definitions
                                .iter_all()
                                .any(super::functioning_abilities::trigger_opts_in_to_command_zone))
                })
            })
            .collect(),
        Zone::Hand | Zone::Library => Vec::new(),
    }
}

fn storm_copy_count_before_cast(state: &GameState) -> i32 {
    state
        .spells_cast_this_turn_by_player
        .values()
        .map(|records| records.len())
        .sum::<usize>()
        .saturating_sub(1) as i32
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

/// CR 605.4a: Resolve `TapsForMana` triggered mana abilities inline, immediately
/// after the mana abilities that triggered them.
///
/// The cost-payment path calls this right after auto-tapping mana sources so the
/// bonus mana from Leyline of Abundance / Wild Growth-class permanents is in the
/// pool before the affordability check. A triggered mana ability (CR 605.1b)
/// does not use the stack (CR 605.4a) — it resolves at once.
///
/// Scope is restricted to triggered *mana* abilities. Non-mana `TapsForMana`
/// triggers are deliberately left untouched so they go on the stack via the
/// deferred post-action trigger scan (CR 603.3) rather than being placed there
/// mid-payment, where a modal/targeted trigger could corrupt the in-flight cast.
///
/// `events[events_before..]` is the batch produced by auto-tap. Every freshly
/// produced `FromTap` `TappedForMana` event in that range is flipped to
/// `FromTapTriggersResolved`; the post-action scan's double-resolution guard
/// keys off that marker to skip the triggered mana abilities resolved here.
pub(super) fn resolve_tap_mana_triggers_inline(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    events_before: usize,
) {
    // Capture the scan bound before resolution — triggered mana abilities append
    // their own bonus `ManaAdded` events (CR 605.4a), which must not be rescanned.
    let scan_end = events.len();

    // Pass 1: resolve every coupled triggered mana ability for each tap event.
    for idx in events_before..scan_end {
        let tap_event = match events.get(idx) {
            Some(
                ev @ GameEvent::TappedForMana {
                    tap_state: ManaTapState::FromTap,
                    ..
                },
            ) => ev.clone(),
            _ => continue,
        };
        // CR 605.1b: Collect every `TapsForMana` triggered mana ability coupled
        // to this tap. `match_taps_for_mana` is the single matcher authority and
        // `is_triggered_mana_ability` the single CR 605.1b classifier — the same
        // predicate the post-action scan's skip guard uses, so "resolved here"
        // and "skipped there" cannot diverge.
        let mut coupled: Vec<ResolvedAbility> = Vec::new();
        for (&obj_id, obj) in state.objects.iter() {
            if obj.zone != Zone::Battlefield {
                continue;
            }
            for (trig_idx, trig_def) in
                super::functioning_abilities::active_trigger_definitions(state, obj)
            {
                if !matches!(trig_def.mode, TriggerMode::TapsForMana) {
                    continue;
                }
                if !super::trigger_matchers::match_taps_for_mana(
                    &tap_event, trig_def, obj_id, state,
                ) {
                    continue;
                }
                let mut ability = build_triggered_ability(state, trig_def, obj_id, obj.controller);
                ability.ability_index = Some(trig_idx);
                if super::mana_abilities::is_triggered_mana_ability(&ability, Some(&tap_event)) {
                    coupled.push(ability);
                }
            }
        }
        for ability in coupled {
            super::mana_abilities::resolve_triggered_mana_ability_inline(
                state,
                &ability,
                Some(&tap_event),
                events,
            );
        }
    }

    // Pass 2: mark the auto-tap events resolved. The post-action trigger scan's
    // double-resolution guard skips `is_triggered_mana_ability` `TapsForMana`
    // triggers on `FromTapTriggersResolved` events; non-mana `TapsForMana`
    // triggers still match and fire there (CR 603.3).
    for ev in &mut events[events_before..scan_end] {
        if let GameEvent::TappedForMana { tap_state, .. } = ev {
            if matches!(tap_state, ManaTapState::FromTap) {
                *tap_state = ManaTapState::FromTapTriggersResolved;
            }
        }
    }
}

/// CR 101.4 + CR 603.3b: APNAP rank of `controller` for trigger ordering — its
/// index in the living turn order from the active player (0 = active player,
/// then each non-active player in turn order). This is the primary key the
/// simultaneous-trigger sorts must use: a binary "active vs non-active" key
/// collapses every non-active player into one bucket and cannot order two or
/// more of them by turn order. Controllers not in the living order (e.g. an
/// eliminated player) sort after all living players. In a two-player game the
/// rank is 0/1, identical to the old binary key, so nothing regresses there.
fn apnap_rank(order: &[PlayerId], controller: PlayerId) -> usize {
    order
        .iter()
        .position(|p| *p == controller)
        .unwrap_or(order.len())
}

/// CR 603.2 + CR 603.3b: Collect every triggered ability matching `events`,
/// apply trigger doubling, and return the contexts sorted into APNAP stack
/// placement order (active player first / bottom of stack, then each non-active
/// player in turn order). This is the pure *collection* half of trigger
/// processing — it never touches `state.pending_trigger` or `state.waiting_for`
/// and never pushes to the stack. `process_triggers` composes this with
/// `dispatch_pending_trigger_context` for the standard path;
/// `collect_triggers_into_deferred` composes it with the `deferred_triggers`
/// queue for resolution-choice handlers that must collect without dispatching
/// (issue #423).
fn collect_pending_triggers(
    state: &mut GameState,
    events: &[GameEvent],
) -> Vec<PendingTriggerContext> {
    // CR 603.6a + CR 611.2e: Continuous effects (including statics that grant
    // triggered abilities to a class — sliver-lord pattern) apply the moment
    // the affected permanent is on the battlefield. The newcomers must be
    // checked for ETB triggers including any granted by their own static
    // abilities (Harmonic Sliver) and by other lords already on the
    // battlefield. Flushing pending layer evaluation here guarantees
    // `obj.trigger_definitions` and `obj.keywords` reflect all active
    // continuous effects before this pass scans for matching triggers.
    super::layers::flush_layers(state);
    let mut pending: Vec<PendingTriggerContext> = Vec::new();
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

        // CR 603.2 over-approximation differential check (debug-only):
        // snapshot the dedup sets BEFORE the production loop mutates them so
        // the shadow scan operates on the same pre-event state the production
        // loop saw. Without this, batched triggers the production loop would
        // have found get falsely skipped as "already batched this pass" in
        // the shadow path.
        #[cfg(debug_assertions)]
        let (mut shadow_batched, mut shadow_registered) =
            (batched_this_pass.clone(), registered_this_event.clone());

        // CR 603.2 + CR 603.6a + CR 611.2e: Consult the maintained TriggerIndex
        // for candidate battlefield objects. Replaces the legacy full
        // battlefield scan with an event-keyed bucket union. The
        // `evaluate_layers` rebuild at the top of `collect_pending_triggers`
        // guarantees the index reflects post-layer trigger sets.
        //
        // Lazy-rebuild sentinel: TriggerIndex is `#[serde(skip)]` and defaults
        // to empty after deserialize. A genuinely-empty index over a
        // non-empty battlefield means we need to rebuild before reading; the
        // common steady-state case (empty index, empty battlefield) is a
        // harmless no-op.
        if state.trigger_index.by_key.is_empty()
            && state.trigger_index.unclassified.is_empty()
            && !state.battlefield.is_empty()
        {
            crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
        }
        let candidates = crate::game::trigger_index::candidates_for_event(state, event);

        // CR 603.2 differential test (debug-only): run a SHADOW scan over the
        // full battlefield with cloned dedup sets and verify every matched
        // (source_id, trig_idx) pair found by the legacy path is also found
        // by the index path. Compares matched-context sets — not visited
        // candidate IDs — so vanilla creatures invisible to the index by
        // design do not falsely panic.
        #[cfg(debug_assertions)]
        let mut shadow_matched: HashSet<(ObjectId, usize)> = HashSet::new();
        #[cfg(debug_assertions)]
        let mut production_matched: HashSet<(ObjectId, usize)> = HashSet::new();

        // Scan candidate permanents for matching triggers
        for obj_id in candidates.iter().copied() {
            let (
                controller,
                timestamp,
                has_prowess,
                has_exploit,
                has_ravenous,
                firebending_amount,
                ward_costs,
                has_decayed,
                matched_triggers,
            ) = {
                let obj = match state.objects.get(&obj_id) {
                    Some(o) => o,
                    None => continue,
                };
                let fb_amount = obj.keywords.iter().find_map(|k| {
                    if let Keyword::Firebending(amount) = k {
                        Some(amount.clone())
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
                    obj.has_keyword(&Keyword::Ravenous),
                    fb_amount,
                    wards,
                    obj.has_keyword(&Keyword::Decayed),
                    collect_matching_triggers(
                        state,
                        event,
                        events,
                        obj,
                        obj.entered_battlefield_turn.unwrap_or(0),
                        Some(Zone::Battlefield),
                        &mut batched_this_pass,
                        &mut registered_this_event,
                    ),
                )
            };

            for matched in matched_triggers {
                #[cfg(debug_assertions)]
                production_matched.insert((obj_id, matched.trig_idx));
                record_trigger_fired(state, matched.constraint.as_ref(), obj_id, matched.trig_idx);
                if matched.batched {
                    batched_this_pass.insert((obj_id, matched.trig_idx));
                }
                registered_this_event.insert((obj_id, matched.trig_idx));
                pending.push(PendingTriggerContext::batched(
                    matched.pending,
                    matched.trigger_events,
                ));
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
                        pending.push(PendingTriggerContext::single(PendingTrigger {
                            source_id: obj_id,
                            controller,
                            condition: prowess_trig_def.condition,
                            ability: prowess_ability,
                            timestamp,
                            target_constraints: Vec::new(),
                            distribute: None,
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                            description: prowess_trig_def.description,
                            may_trigger_origin: None,
                            subject_match_count: None,
                        }));
                    }
                }
            }

            // CR 702.156a + CR 107.3m: Ravenous includes "When this permanent
            // enters, if X is 5 or more, draw a card." The paid X is stamped
            // on the permanent as `cost_x_paid` during spell finalization.
            if has_ravenous {
                if let GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } = event
                {
                    let x_paid = state
                        .objects
                        .get(&obj_id)
                        .and_then(|obj| obj.cost_x_paid)
                        .unwrap_or(0);
                    if *object_id == obj_id && x_paid >= 5 {
                        let draw_ability = ResolvedAbility::new(
                            Effect::Draw {
                                count: QuantityExpr::Fixed { value: 1 },
                                target: TargetFilter::Controller,
                            },
                            Vec::new(),
                            obj_id,
                            controller,
                        );
                        let ravenous_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
                            .description("Ravenous".to_string());
                        pending.push(PendingTriggerContext::single(PendingTrigger {
                            source_id: obj_id,
                            controller,
                            condition: ravenous_trigger.condition,
                            ability: draw_ability,
                            timestamp,
                            target_constraints: Vec::new(),
                            distribute: None,
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                            description: ravenous_trigger.description,
                            may_trigger_origin: None,
                            subject_match_count: None,
                        }));
                    }
                }
            }

            // Keyword-based triggers: Firebending
            // Firebending N triggers when a creature with firebending is declared as attacker.
            // Produces N {R} mana with EndOfCombat expiry.
            if let GameEvent::AttackersDeclared { attacker_ids, .. } = event {
                if let Some(amount) = firebending_amount {
                    if attacker_ids.contains(&obj_id) {
                        let fb_effect = Effect::Mana {
                            produced: crate::types::ability::ManaProduction::AnyOneColor {
                                count: amount,
                                color_options: vec![crate::types::mana::ManaColor::Red],
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
                            .description("Firebending".to_string());
                        pending.push(PendingTriggerContext::single(PendingTrigger {
                            source_id: obj_id,
                            controller,
                            condition: fb_trig_def.condition,
                            ability: fb_ability,
                            timestamp,
                            target_constraints: Vec::new(),
                            distribute: None,
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                            description: fb_trig_def.description,
                            may_trigger_origin: Some(MayTriggerOrigin::Keyword {
                                keyword: KeywordKind::Firebending,
                            }),
                            subject_match_count: None,
                        }));
                    }
                }
            }

            // CR 702.147a: Decayed means "When this creature attacks, sacrifice
            // it at end of combat." The keyword creates a normal triggered
            // ability on attack; when that trigger resolves, it creates the
            // one-shot delayed trigger for the end of combat step.
            if let GameEvent::AttackersDeclared { attacker_ids, .. } = event {
                if has_decayed && attacker_ids.contains(&obj_id) {
                    let delayed_sacrifice = AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Sacrifice {
                            target: TargetFilter::SelfRef,
                            count: QuantityExpr::Fixed { value: 1 },
                            min_count: 0,
                        },
                    );
                    let decayed_effect = Effect::CreateDelayedTrigger {
                        condition: DelayedTriggerCondition::AtNextPhase {
                            phase: Phase::EndCombat,
                        },
                        effect: Box::new(delayed_sacrifice),
                        uses_tracked_set: false,
                    };
                    let decayed_ability =
                        ResolvedAbility::new(decayed_effect, Vec::new(), obj_id, controller);
                    let decayed_trigger = TriggerDefinition::new(TriggerMode::Attacks)
                        .description("Decayed".to_string());
                    pending.push(PendingTriggerContext::single(PendingTrigger {
                        source_id: obj_id,
                        controller,
                        condition: decayed_trigger.condition,
                        ability: decayed_ability,
                        timestamp,
                        target_constraints: Vec::new(),
                        distribute: None,
                        trigger_event: Some(event.clone()),
                        modal: None,
                        mode_abilities: vec![],
                        description: decayed_trigger.description,
                        may_trigger_origin: None,
                        subject_match_count: None,
                    }));
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
                        pending.push(PendingTriggerContext::single(PendingTrigger {
                            source_id: *object_id,
                            controller,
                            condition: None,
                            ability: exploit_ability,
                            timestamp,
                            target_constraints: Vec::new(),
                            distribute: None,
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                            description: None,
                            may_trigger_origin: Some(MayTriggerOrigin::Keyword {
                                keyword: KeywordKind::Exploit,
                            }),
                            subject_match_count: None,
                        }));
                    }
                }
            }

            // CR 702.21a: Ward triggers when this permanent becomes the target
            // of a spell or ability an opponent controls. Each ward instance
            // triggers independently. Only fires for permanents (battlefield scan).
            if !ward_costs.is_empty() {
                if let GameEvent::BecomesTarget {
                    target: TargetRef::Object(targeted_id),
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
                                    // CR 702.21a + CR 118.12: Ward generates a counter
                                    // effect with an unless-pay modifier. Post-fold, the
                                    // modifier lives on `ResolvedAbility.unless_pay` and
                                    // is intercepted by the unified runtime pipeline.
                                    let unless_cost = ward_cost_to_ability_cost(ward);
                                    let counter_effect = Effect::Counter {
                                        target: TargetFilter::TriggeringSource,
                                        source_rider: None,
                                    };
                                    let mut ward_ability = ResolvedAbility::new(
                                        counter_effect,
                                        Vec::new(),
                                        obj_id,
                                        controller,
                                    );
                                    ward_ability.unless_pay =
                                        Some(crate::types::ability::UnlessPayModifier {
                                            cost: unless_cost,
                                            payer: TargetFilter::TriggeringSpellController,
                                        });
                                    pending.push(PendingTriggerContext::single(PendingTrigger {
                                        source_id: obj_id,
                                        controller,
                                        condition: None,
                                        ability: ward_ability,
                                        timestamp,
                                        target_constraints: Vec::new(),
                                        distribute: None,
                                        trigger_event: Some(event.clone()),
                                        modal: None,
                                        mode_abilities: vec![],
                                        description: Some("Ward".to_string()),
                                        may_trigger_origin: None,
                                        subject_match_count: None,
                                    }));
                                }
                            }
                        }
                    }
                }
            }
        }

        // CR 603.2 over-approximation differential test (debug-only): after the
        // production loop completes, run a SHADOW battlefield scan with the
        // pre-event-snapshot dedup sets. Compare matched `(source_id, trig_idx)`
        // contexts — every match found by the legacy full scan must appear in
        // the index path's match set. Vanilla creatures visited by the legacy
        // scan but invisible to the index by design return zero matches and
        // do not enter the comparison.
        #[cfg(debug_assertions)]
        {
            for obj_id in trigger_source_ids_for_zone(state, Zone::Battlefield) {
                let Some(obj) = state.objects.get(&obj_id) else {
                    continue;
                };
                let matched = collect_matching_triggers(
                    state,
                    event,
                    events,
                    obj,
                    obj.entered_battlefield_turn.unwrap_or(0),
                    Some(Zone::Battlefield),
                    &mut shadow_batched,
                    &mut shadow_registered,
                );
                for m in matched {
                    shadow_matched.insert((obj_id, m.trig_idx));
                }
            }
            let dropped: Vec<(ObjectId, usize)> = shadow_matched
                .difference(&production_matched)
                .copied()
                .collect();
            debug_assert!(
                dropped.is_empty(),
                "TriggerIndex under-approximation (CR 603.2 silent trigger drop): \
                 event={event:?} dropped_matches={dropped:?} \
                 candidates_visited={candidates:?}",
            );
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
                        events,
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
                    pending.push(PendingTriggerContext::batched(
                        matched.pending,
                        matched.trigger_events,
                    ));
                }
            }
        }

        // CR 603.10a (continued): an observer that left the battlefield in the
        // SAME simultaneous event as this departure observes it via last-known
        // information. The producer stamps that group onto `record.co_departed`
        // (see `zones::mark_simultaneous_departures`); this is the authority for
        // simultaneity. The `moved_id` block above handles an object observing
        // its own departure, and the live battlefield scan covers surviving
        // observers — so this covers the remaining case: a leaves-the-battlefield
        // observer (Blood Artist, Zulaport Cutthroat, Elas il-Kor) destroyed by
        // the same board wipe triggers once for each co-dying creature
        // (CR 603.10a's worked example). Because the group comes from the
        // producer rather than the shape of the accumulated event vector,
        // sequential departures within one resolution never cross-observe.
        if let GameEvent::ZoneChanged {
            object_id: moved_id,
            from: Some(Zone::Battlefield),
            record,
            ..
        } = event
        {
            for observer_id in record.co_departed.iter().copied() {
                // The departing object itself is handled by the `moved_id` block
                // above; observers still on the battlefield are handled by the
                // live scan. Only co-departed observers remain.
                if observer_id == *moved_id {
                    continue;
                }
                if !state
                    .objects
                    .get(&observer_id)
                    .is_some_and(|o| o.zone != Zone::Battlefield)
                {
                    continue;
                }
                let matched_triggers = {
                    let Some(obj) = state.objects.get(&observer_id) else {
                        continue;
                    };
                    collect_matching_triggers(
                        state,
                        event,
                        events,
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
                        observer_id,
                        matched.trig_idx,
                    );
                    if matched.batched {
                        batched_this_pass.insert((observer_id, matched.trig_idx));
                    }
                    registered_this_event.insert((observer_id, matched.trig_idx));
                    pending.push(PendingTriggerContext::batched(
                        matched.pending,
                        matched.trigger_events,
                    ));
                }
            }
        }

        // CR 113.6k + CR 114.4: Non-battlefield trigger zones are opt-in via
        // `trigger_zones`. Command-zone emblems function by default; non-emblem
        // command-zone sources require a trigger-level `Zone::Command` opt-in
        // (CR 113.6b), enforced by `active_trigger_definitions`.
        // Synthetic battlefield-only keyword triggers (prowess / ward /
        // firebending / exploit) deliberately do NOT run in this loop.
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
                        events,
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
                    pending.push(PendingTriggerContext::batched(
                        matched.pending,
                        matched.trigger_events,
                    ));
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
            controller: caster,
            ..
        } = event
        {
            let storm_instances =
                super::casting::effective_spell_keywords(state, *caster, *cast_obj_id)
                    .iter()
                    .filter(|keyword| matches!(keyword, Keyword::Storm))
                    .count();
            if storm_instances > 0 {
                let copy_count = storm_copy_count_before_cast(state);
                for _ in 0..storm_instances {
                    let mut storm_ability = ResolvedAbility::new(
                        // CR 707.10c: Storm — "You may choose new targets for the copies."
                        Effect::CopySpell {
                            target: TargetFilter::SelfRef,
                            retarget: CopyRetargetPermission::MayChooseNewTargets,
                        },
                        Vec::new(),
                        *cast_obj_id,
                        *caster,
                    );
                    storm_ability.repeat_for = Some(QuantityExpr::Fixed { value: copy_count });
                    let storm_trig_def = TriggerDefinition::new(TriggerMode::SpellCast)
                        .description("Storm".to_string())
                        .condition(TriggerCondition::WasCast { zone: None });
                    let timestamp = state.next_timestamp() as u32;
                    pending.push(PendingTriggerContext::single(PendingTrigger {
                        source_id: *cast_obj_id,
                        controller: *caster,
                        condition: storm_trig_def.condition,
                        ability: storm_ability,
                        timestamp,
                        target_constraints: Vec::new(),
                        distribute: None,
                        trigger_event: Some(event.clone()),
                        modal: None,
                        mode_abilities: vec![],
                        description: storm_trig_def.description,
                        may_trigger_origin: None,
                        subject_match_count: None,
                    }));
                }
            }

            let (instance_count, controller) = state
                .objects
                .get(cast_obj_id)
                .map(|obj| {
                    let n = super::casting::effective_spell_keywords(state, *caster, *cast_obj_id)
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
                    .condition(TriggerCondition::WasCast { zone: None });
                let cascade_ability =
                    ResolvedAbility::new(Effect::Cascade, Vec::new(), *cast_obj_id, controller);
                let timestamp = state.next_timestamp() as u32;
                pending.push(PendingTriggerContext::single(PendingTrigger {
                    source_id: *cast_obj_id,
                    controller,
                    condition: cascade_trig_def.condition,
                    ability: cascade_ability,
                    timestamp,
                    target_constraints: Vec::new(),
                    distribute: None,
                    trigger_event: Some(event.clone()),
                    modal: None,
                    mode_abilities: vec![],
                    description: cascade_trig_def.description,
                    may_trigger_origin: None,
                    subject_match_count: None,
                }));
            }

            // CR 702.153a: Casualty triggers when the spell is cast with the
            // cost paid. Applies to both printed Casualty (obj.additional_cost
            // is set) and dynamically granted Casualty (e.g. from Silverquill).
            // The WasCast intervening-if is intentionally omitted: the trigger
            // only fires on SpellCast, which is only emitted for an actual
            // cast — so the cast-ness is already implied by the trigger event
            // itself and a redundant WasCast condition would add nothing.
            // CR 702.153a: Skip cards with a printed Casualty keyword — those
            // already have the copy trigger from synthesize_casualty (face-level
            // trigger synthesis). Only cards whose Casualty is dynamically
            // granted (e.g. from Silverquill) need this path. We must NOT use
            // `obj.additional_cost.is_none()` here: that excluded any card
            // with ANY printed additional cost (e.g. Corrupted Conviction's
            // Required sacrifice), which is wrong — only the presence of a
            // printed Casualty keyword means the face trigger already exists.
            let dynamically_granted_casualty_instances = state
                .objects
                .get(cast_obj_id)
                .filter(|obj| {
                    !obj.keywords
                        .iter()
                        .any(|k| matches!(k, Keyword::Casualty(_)))
                })
                .and_then(|obj| {
                    let paid = state
                        .stack
                        .iter()
                        .find(|entry| entry.id == *cast_obj_id)
                        .is_some_and(|entry| {
                            entry
                                .ability()
                                .is_some_and(|ability| ability.context.additional_cost_paid)
                        });
                    paid.then_some(obj.controller)
                })
                .map(|controller| {
                    let n =
                        super::casting::effective_spell_keywords(state, controller, *cast_obj_id)
                            .iter()
                            .filter(|keyword| matches!(keyword, Keyword::Casualty(_)))
                            .count();
                    (n, controller)
                })
                .unwrap_or((0, PlayerId(0)));
            for _ in 0..dynamically_granted_casualty_instances.0 {
                // CR 702.153a: Reuse the canonical casualty AbilityDefinition so
                // both intrinsic (face-synthesized) and dynamically-granted
                // casualty triggers share one structural source of truth. The
                // pre-gate above already verified the cast paid casualty;
                // surface that on the new ability's context so the embedded
                // `additional_cost_paid_any` condition evaluates correctly at
                // resolution.
                let mut casualty_ability = build_resolved_from_def(
                    &crate::database::synthesis::casualty_copy_ability_definition(),
                    *cast_obj_id,
                    dynamically_granted_casualty_instances.1,
                );
                casualty_ability.context.additional_cost_paid = true;
                let timestamp = state.next_timestamp() as u32;
                pending.push(PendingTriggerContext::single(PendingTrigger {
                    source_id: *cast_obj_id,
                    controller: dynamically_granted_casualty_instances.1,
                    // No WasCast intervening-if: SpellCast only fires for an
                    // actual cast, so cast-ness is already implied by the
                    // trigger event itself. The copy is gated instead by
                    // AbilityCondition::additional_cost_paid_any() inside
                    // casualty_copy_ability_definition(), which reads the already-set
                    // `ability.context.additional_cost_paid = true` above.
                    condition: None,
                    ability: casualty_ability,
                    timestamp,
                    target_constraints: Vec::new(),
                    distribute: None,
                    trigger_event: Some(event.clone()),
                    modal: None,
                    mode_abilities: vec![],
                    description: Some("Casualty".to_string()),
                    may_trigger_origin: None,
                    subject_match_count: None,
                }));
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
                    };
                    let draw_ability =
                        ResolvedAbility::new(draw_effect, Vec::new(), ObjectId(0), monarch_id);
                    let trig_def = TriggerDefinition::new(TriggerMode::Phase)
                        .description("Monarch draw (CR 725.2)".to_string());
                    pending.push(PendingTriggerContext::single(PendingTrigger {
                        source_id: ObjectId(0),
                        controller: monarch_id,
                        condition: trig_def.condition,
                        ability: draw_ability,
                        timestamp: 0,
                        target_constraints: Vec::new(),
                        distribute: None,
                        trigger_event: Some(event.clone()),
                        modal: None,
                        mode_abilities: vec![],
                        description: trig_def.description,
                        may_trigger_origin: None,
                        subject_match_count: None,
                    }));
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
                    pending.push(PendingTriggerContext::single(PendingTrigger {
                        source_id: ObjectId(0),
                        controller: init_holder,
                        condition: trig_def.condition,
                        ability: venture_ability,
                        timestamp: 0,
                        target_constraints: Vec::new(),
                        distribute: None,
                        trigger_event: Some(event.clone()),
                        modal: None,
                        mode_abilities: vec![],
                        description: trig_def.description,
                        may_trigger_origin: None,
                        subject_match_count: None,
                    }));
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
                        pending.push(PendingTriggerContext::single(PendingTrigger {
                            source_id: *source_id,
                            controller: new_monarch,
                            condition: trig_def.condition,
                            ability: become_ability,
                            timestamp: 0,
                            target_constraints: Vec::new(),
                            distribute: None,
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                            description: trig_def.description,
                            may_trigger_origin: None,
                            subject_match_count: None,
                        }));
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
                        pending.push(PendingTriggerContext::single(PendingTrigger {
                            source_id: *source_id,
                            controller: new_holder,
                            condition: trig_def.condition,
                            ability: take_init,
                            timestamp: 0,
                            target_constraints: Vec::new(),
                            distribute: None,
                            trigger_event: Some(event.clone()),
                            modal: None,
                            mode_abilities: vec![],
                            description: trig_def.description,
                            may_trigger_origin: None,
                            subject_match_count: None,
                        }));
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
                    Effect::ChangeSpeed {
                        player_scope: PlayerFilter::Controller,
                        amount: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        direction: crate::types::ability::SpeedDelta::Increase,
                        floor: None,
                    },
                    Vec::new(),
                    speed_key_source(),
                    trigger_controller,
                );
                let trig_def = TriggerDefinition::new(TriggerMode::LifeLost)
                    .description("Start your engines! (CR 702.179d)".to_string());
                pending.push(PendingTriggerContext::single(PendingTrigger {
                    source_id: speed_key_source(),
                    controller: trigger_controller,
                    condition: trig_def.condition,
                    ability: increase_ability,
                    timestamp: 0,
                    target_constraints: Vec::new(),
                    distribute: None,
                    trigger_event: Some(event.clone()),
                    modal: None,
                    mode_abilities: vec![],
                    description: trig_def.description,
                    may_trigger_origin: None,
                    subject_match_count: None,
                }));
                mark_speed_trigger_used(state, trigger_controller);
            }
        }

        // CR 728.1: At the beginning of each player's precombat main phase,
        // if that player has one or more rad counters, that player mills cards
        // equal to their rad counter count. For each nonland card milled,
        // that player loses 1 life and removes one rad counter.
        // Note: "each player's precombat main phase" — since only the active
        // player's precombat main phase fires at any given time, checking
        // state.active_player is equivalent. Same pattern as monarch (CR 725.2).
        if let GameEvent::PhaseChanged {
            phase: Phase::PreCombatMain,
        } = event
        {
            let active = state.active_player;
            let rad_count = state
                .players
                .iter()
                .find(|p| p.id == active)
                .map(|p| p.player_counter(&PlayerCounterKind::Rad))
                .unwrap_or(0);
            if rad_count > 0 {
                let rad_ability = ResolvedAbility::new(
                    Effect::ProcessRadCounters,
                    Vec::new(),
                    ObjectId(0),
                    active,
                );
                let trig_def = TriggerDefinition::new(TriggerMode::Phase)
                    .description("Rad counters (CR 728.1)".to_string());
                pending.push(PendingTriggerContext::single(PendingTrigger {
                    source_id: ObjectId(0),
                    controller: active,
                    condition: trig_def.condition,
                    ability: rad_ability,
                    timestamp: 0,
                    target_constraints: Vec::new(),
                    distribute: None,
                    trigger_event: Some(event.clone()),
                    modal: None,
                    mode_abilities: vec![],
                    description: trig_def.description,
                    may_trigger_origin: None,
                    subject_match_count: None,
                }));
            }
        }
    }

    collect_ring_emblem_triggers(state, events, &mut pending);

    // CR 603.2d: Trigger doubling — Panharmonicon-style effects.
    // Scan battlefield for objects with StaticMode::Panharmonicon statics,
    // then clone matching pending triggers.
    apply_trigger_doubling(state, &mut pending);

    // CR 603.3b + CR 101.4: Stack-placement order is full APNAP turn order
    // (active player first / lowest on stack, then each non-active player in
    // turn order) — not a binary active/non-active split, which mis-orders 2+
    // non-active players in multiplayer. Within the same controller, seed the
    // ordering prompt by timestamp.
    let apnap = crate::game::players::apnap_order(state);
    pending.sort_by_key(|t| {
        (
            apnap_rank(&apnap, t.pending.controller),
            t.pending.timestamp,
        )
    });
    pending
}

fn collect_ring_emblem_triggers(
    state: &GameState,
    events: &[GameEvent],
    pending: &mut Vec<PendingTriggerContext>,
) {
    for event in events {
        let players: Vec<_> = state.ring_level.keys().copied().collect();
        for player in players {
            let level = state.ring_level.get(&player).copied().unwrap_or(0);
            let Some(bearer_id) = super::effects::ring::ring_bearer_for(state, player) else {
                continue;
            };

            if level >= 2 {
                // CR 701.54c: Once the Ring has tempted a player two or more
                // times, it has "Whenever your Ring-bearer attacks, draw a
                // card, then discard a card."
                if let GameEvent::AttackersDeclared { attacker_ids, .. } = event {
                    if attacker_ids.contains(&bearer_id) {
                        pending.push(ring_pending_trigger(
                            bearer_id,
                            player,
                            player,
                            event,
                            ring_level_two_ability(bearer_id, player),
                            "The Ring level 2",
                        ));
                    }
                }
            }

            if level >= 3 {
                // CR 701.54c: Once the Ring has tempted a player three or more
                // times, it has "Whenever your Ring-bearer becomes blocked by
                // a creature, the blocking creature's controller sacrifices it
                // at end of combat."
                if let GameEvent::BlockersDeclared { assignments } = event {
                    for (blocker_id, attacker_id) in assignments {
                        if *attacker_id != bearer_id {
                            continue;
                        }
                        let Some(blocker) = state.objects.get(blocker_id) else {
                            continue;
                        };
                        if !blocker.card_types.core_types.contains(&CoreType::Creature) {
                            continue;
                        }
                        pending.push(ring_pending_trigger(
                            bearer_id,
                            player,
                            player,
                            event,
                            ring_level_three_ability(
                                bearer_id,
                                blocker.id,
                                player,
                                blocker.controller,
                            ),
                            "The Ring level 3",
                        ));
                    }
                }
            }

            if level >= 4 {
                // CR 701.54c: Once the Ring has tempted a player four or more
                // times, it has "Whenever your Ring-bearer deals combat damage
                // to a player, each opponent loses 3 life."
                if let GameEvent::CombatDamageDealtToPlayer { source_amounts, .. } = event {
                    if source_amounts.iter().any(|(id, _)| *id == bearer_id) {
                        pending.push(ring_pending_trigger(
                            bearer_id,
                            player,
                            player,
                            event,
                            ring_level_four_ability(bearer_id, player),
                            "The Ring level 4",
                        ));
                    }
                }
            }
        }
    }
}

fn ring_pending_trigger(
    source_id: ObjectId,
    trigger_controller: PlayerId,
    ability_controller: PlayerId,
    event: &GameEvent,
    mut ability: ResolvedAbility,
    description: &str,
) -> PendingTriggerContext {
    ability.controller = ability_controller;
    PendingTriggerContext::single(PendingTrigger {
        source_id,
        controller: trigger_controller,
        condition: None,
        ability,
        timestamp: 0,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: Some(event.clone()),
        modal: None,
        mode_abilities: vec![],
        description: Some(description.to_string()),
        may_trigger_origin: None,
        subject_match_count: None,
    })
}

fn ring_level_two_ability(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    let discard = ResolvedAbility::new(
        Effect::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
            random: false,
            unless_filter: None,
            filter: None,
        },
        vec![],
        source_id,
        controller,
    );
    ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        source_id,
        controller,
    )
    .sub_ability(discard)
}

fn ring_level_three_ability(
    source_id: ObjectId,
    blocker_id: ObjectId,
    controller: PlayerId,
    blocker_controller: PlayerId,
) -> ResolvedAbility {
    let target = TargetFilter::And {
        filters: vec![
            TargetFilter::SpecificObject { id: blocker_id },
            TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::ScopedPlayer)),
        ],
    };
    let sacrifice = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    );
    let mut ability = ResolvedAbility::new(
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase {
                phase: Phase::EndCombat,
            },
            effect: Box::new(sacrifice),
            uses_tracked_set: false,
        },
        vec![],
        source_id,
        controller,
    );
    // CR 701.54c + CR 701.21a: The Ring-bearer's controller controls the
    // triggered and delayed abilities, but the blocking creature's controller
    // performs the sacrifice.
    ability.scoped_player = Some(blocker_controller);
    ability
}

fn ring_level_four_ability(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    let mut ability = ResolvedAbility::new(
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 3 },
            target: None,
        },
        vec![],
        source_id,
        controller,
    );
    ability.player_scope = Some(PlayerFilter::Opponent);
    ability
}

/// Process events and place triggered abilities on the stack in APNAP order.
/// CR 603.3b: Process triggered abilities waiting to be put on the stack.
pub fn process_triggers(state: &mut GameState, events: &[GameEvent]) {
    let pending = collect_pending_triggers(state, events);

    // CR 603.2 / CR 603.3: Nothing triggered — short-circuit before the
    // transient-field cleanup below. This early return is load-bearing: the
    // cleanup must run *only* when triggers were actually collected, so that
    // `process_triggers` is a no-op (byte-identical) for events that trigger
    // nothing. Callers rely on this (e.g. mana-tap event scans).
    if pending.is_empty() {
        return;
    }

    // CR 603.3b + CR 101.4: If any controller has 2+ triggers in this pass,
    // each such controller must choose the order their group is placed.
    // Collect every controller's order up front (issue #531) — the v1
    // interleaved design silently skipped later groups when a dispatched
    // trigger paused, because the post-pause `deferred_triggers` drain has
    // no concept of remaining ordering choices.
    match begin_trigger_ordering(state, pending) {
        TriggerOrderingDisposition::PromptForChoice(wf) => {
            state.waiting_for = *wf;
            clear_post_collection_transients(state);
        }
        // No controller has 2+ triggers — dispatch immediately (zero behaviour
        // change for the single-trigger common case). `collect_pending_triggers`
        // already returned the vec in APNAP stack-placement order.
        TriggerOrderingDisposition::NoChoiceNeeded(pending) => {
            dispatch_collected_triggers(state, pending);
            clear_post_collection_transients(state);
        }
    }
}

/// CR 113.2c + CR 603.2 + CR 603.3b: Drive each collected trigger through
/// its disposition (pushed to stack, resolved inline as a mana ability, or
/// paused for player input). If `dispatch_pending_trigger_context` reports
/// a pause, the remaining contexts are stashed into `deferred_triggers` so
/// they reach the stack once the active `pending_trigger` is resolved by
/// its dispatcher. Without this, every queued trigger after the first
/// input-requiring one would be silently dropped (issue #416).
fn dispatch_collected_triggers(state: &mut GameState, pending: Vec<PendingTriggerContext>) {
    let mut events_out = Vec::new();
    let mut iter = pending.into_iter();
    while let Some(trigger_context) = iter.next() {
        if dispatch_pending_trigger_context(state, trigger_context, &mut events_out) {
            // Active trigger paused on player input. Stash the remaining
            // contexts to be drained by `drain_deferred_trigger_queue` after
            // the active trigger is finalized.
            state.deferred_triggers.extend(iter);
            return;
        }
    }
}

/// Clear transient cast-tally booleans/color breakdown on all objects after
/// trigger collection. `mana_spent_to_cast_amount` is intentionally NOT
/// cleared: it is a historical fact about the object (how much mana was
/// spent to cast it) used by spell resolution effects like "deals damage
/// equal to the amount of mana spent to cast this spell" (Molten Note) and
/// by CR 603.4 intervening-if resolution re-checks (Hungry Graffalon /
/// Topiary Lecturer Increment).
///
/// CR 603.4: `cast_from_zone` is likewise preserved for permanents on the
/// battlefield — a `WasCast` / "if you cast it" ETB intervening-if is
/// re-checked when the triggered ability *resolves* (`stack.rs`), not only
/// when it is collected. Clearing it on all objects here would make every
/// `WasCast`-conditioned ETB trigger (Wedding Ring's token-copy, Discover
/// ETBs) silently do nothing at resolution. It is still cleared for
/// non-battlefield objects (a fizzled stack spell, an object that bounced)
/// since their cast provenance is no longer meaningful, and is cleared on
/// battlefield exit by `reset_for_battlefield_exit`.
fn clear_post_collection_transients(state: &mut GameState) {
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        if obj.zone != Zone::Battlefield {
            obj.cast_from_zone = None;
        }
        obj.mana_spent_to_cast = false;
        obj.colors_spent_to_cast = crate::types::mana::ColoredManaCount::default();
    }
}

/// CR 603.3b: Outcome of the per-controller ordering pass for a freshly
/// collected pending-trigger set. `WaitingFor` is large (~432 bytes), so the
/// prompt arm is boxed to keep the enum size proportional to the common case.
enum TriggerOrderingDisposition {
    /// At least one controller has 2+ triggers — `state.pending_trigger_order`
    /// has been populated and the inner `WaitingFor::OrderTriggers` must be
    /// set as `state.waiting_for`.
    PromptForChoice(Box<crate::types::game_state::WaitingFor>),
    /// Every controller has at most one trigger — no choice is needed; the
    /// caller dispatches the original vec directly.
    NoChoiceNeeded(Vec<PendingTriggerContext>),
}

/// CR 603.3b: Partition `pending` by controller (preserving the APNAP
/// placement order produced by `collect_pending_triggers`), populate
/// `state.pending_trigger_order` with one `TriggerOrderGroup` per controller,
/// and return either the first ordering prompt (earliest APNAP unordered group) or
/// `NoChoiceNeeded` when no group requires a choice.
///
/// Choice order vs placement order — the two are intentionally distinct:
///   * Placement order (which group sits lower on the stack) is APNAP per
///     CR 405.3 + 603.3b — active player first / lowest, then each non-active
///     player in turn order — and is locked by the input vec.
///   * Choice order (which controller is prompted first) is APNAP per
///     CR 101.4 — active player chooses first — so we prompt the first
///     unordered group in the same APNAP order.
fn begin_trigger_ordering(
    state: &mut GameState,
    pending: Vec<PendingTriggerContext>,
) -> TriggerOrderingDisposition {
    use crate::types::game_state::{PendingTriggerOrder, TriggerOrderGroup};

    // Partition by controller while preserving the input (placement) order.
    let mut groups: Vec<TriggerOrderGroup> = Vec::new();
    for ctx in pending {
        let controller = ctx.pending.controller;
        if let Some(last) = groups.last_mut() {
            if last.controller == controller {
                last.triggers.push(ctx);
                continue;
            }
        }
        groups.push(TriggerOrderGroup {
            controller,
            triggers: vec![ctx],
            ordered: false,
        });
    }

    // Single-trigger groups are trivially in final order.
    for g in groups.iter_mut() {
        if g.triggers.len() <= 1 {
            g.ordered = true;
        }
    }

    // Common case: every group has at most one trigger. No choice needed —
    // return the concatenated vec for direct dispatch.
    if groups.iter().all(|g| g.ordered) {
        let pending: Vec<PendingTriggerContext> =
            groups.into_iter().flat_map(|g| g.triggers).collect();
        return TriggerOrderingDisposition::NoChoiceNeeded(pending);
    }

    state.pending_trigger_order = Some(PendingTriggerOrder {
        groups,
        resume_after_ordering: None,
    });
    let wf = build_next_order_triggers_prompt(state)
        .expect("just-populated pending_trigger_order must yield a prompt");
    TriggerOrderingDisposition::PromptForChoice(Box::new(wf))
}

/// CR 603.3b + CR 605.4a: If a trigger-ordering choice interrupted a
/// non-Priority resume path (for example ChooseManaColor returning to
/// ManaPayment), carry that resume through the ordering pass. The ordering
/// prompt itself remains the public WaitingFor; this state is restored only
/// after all ordered triggers have been dispatched and no trigger-specific
/// prompt is pending.
pub(crate) fn preserve_order_triggers_resume(
    state: &mut GameState,
    resume: crate::types::game_state::WaitingFor,
) -> Option<crate::types::game_state::WaitingFor> {
    use crate::types::game_state::WaitingFor;

    if !matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
        return None;
    }

    let order = state
        .pending_trigger_order
        .as_mut()
        .expect("OrderTriggers waiting state must have pending_trigger_order");
    order.resume_after_ordering = Some(Box::new(resume));
    Some(state.waiting_for.clone())
}

/// CR 603.3b: Public wrapper around `build_next_order_triggers_prompt` for
/// cross-module use (e.g., `elimination::prune_pending_trigger_order`).
pub(crate) fn build_next_order_triggers_prompt_public(
    state: &GameState,
) -> Option<crate::types::game_state::WaitingFor> {
    build_next_order_triggers_prompt(state)
}

/// CR 603.3b: Test/legacy helper that drains every outstanding
/// `WaitingFor::OrderTriggers` prompt by submitting the identity order (which
/// is equivalent to the pre-issue-#531 deterministic placement). Used by
/// existing tests that assert on stack contents without modeling the new
/// per-controller ordering choice, and by callers (e.g., the engine's
/// auto-advance path) that should preserve legacy behaviour for the
/// stack-placement-only assertions. Returns the number of prompts drained.
pub fn drain_order_triggers_with_identity(
    state: &mut crate::types::game_state::GameState,
) -> usize {
    use crate::types::actions::GameAction;
    use crate::types::game_state::WaitingFor;
    let mut drained = 0;
    while let WaitingFor::OrderTriggers { triggers, .. } = &state.waiting_for {
        let order: Vec<usize> = (0..triggers.len()).collect();
        super::engine::apply_as_current(state, GameAction::OrderTriggers { order })
            .expect("identity OrderTriggers must succeed");
        drained += 1;
        if drained > 16 {
            panic!("drain_order_triggers_with_identity: too many APNAP groups");
        }
    }
    drained
}

/// CR 603.3b: Build the next `WaitingFor::OrderTriggers` prompt by finding
/// the earliest unordered group in APNAP order.
/// Returns `None` if every group is `ordered` (caller should dispatch).
fn build_next_order_triggers_prompt(
    state: &GameState,
) -> Option<crate::types::game_state::WaitingFor> {
    use crate::types::game_state::{PendingTriggerSummary, WaitingFor};

    let order = state.pending_trigger_order.as_ref()?;
    let group = order.groups.iter().find(|g| !g.ordered)?;
    let triggers: Vec<PendingTriggerSummary> = group
        .triggers
        .iter()
        .map(|ctx| PendingTriggerSummary {
            source_id: ctx.pending.source_id,
            source_name: state
                .objects
                .get(&ctx.pending.source_id)
                .map(|o| o.name.clone())
                .unwrap_or_default(),
            description: ctx.pending.description.clone().unwrap_or_default(),
        })
        .collect();
    Some(WaitingFor::OrderTriggers {
        player: group.controller,
        triggers,
    })
}

/// CR 603.3b: Validate `order` is a permutation of `0..len`. Returns true if
/// `order` has the right length, no duplicates, and every index is in range.
fn is_valid_permutation(order: &[usize], len: usize) -> bool {
    if order.len() != len {
        return false;
    }
    let mut seen = vec![false; len];
    for &i in order {
        if i >= len || seen[i] {
            return false;
        }
        seen[i] = true;
    }
    true
}

/// CR 603.3b: Apply a player's chosen order to their group, then either emit
/// the next `OrderTriggers` prompt (next APNAP unordered group) or — when
/// every group is ordered — concatenate them in APNAP placement order
/// and dispatch through the standard pipeline. The ordering-vs-input-pause
/// invariant (issue #531 v2): every group's ordering is fully resolved
/// *before* any trigger is dispatched, so a trigger that pauses on input
/// stashes only already-ordered un-dispatched triggers into `deferred_triggers`
/// — no ordering choice can be skipped.
pub(crate) fn handle_order_triggers(
    state: &mut GameState,
    order: Vec<usize>,
) -> Result<crate::types::game_state::WaitingFor, super::engine::EngineError> {
    use crate::types::game_state::WaitingFor;

    let pending_order = state.pending_trigger_order.as_mut().ok_or_else(|| {
        super::engine::EngineError::InvalidAction(
            "OrderTriggers submitted with no pending ordering pass".to_string(),
        )
    })?;

    // Locate the earliest APNAP unordered group — same selector as
    // `build_next_order_triggers_prompt`.
    let target_idx = pending_order
        .groups
        .iter()
        .position(|g| !g.ordered)
        .ok_or_else(|| {
            super::engine::EngineError::InvalidAction(
                "OrderTriggers submitted but every group is already ordered".to_string(),
            )
        })?;

    let group = &mut pending_order.groups[target_idx];
    let group_len = group.triggers.len();
    if !is_valid_permutation(&order, group_len) {
        return Err(super::engine::EngineError::InvalidAction(format!(
            "OrderTriggers order {order:?} is not a permutation of 0..{group_len}"
        )));
    }

    // Apply the permutation: index 0 of `order` selects which input trigger
    // ends up at output position 0 (bottom of this controller's stack-slot).
    let mut reordered: Vec<PendingTriggerContext> = Vec::with_capacity(group_len);
    let mut taken: Vec<Option<PendingTriggerContext>> =
        group.triggers.drain(..).map(Some).collect();
    for &i in &order {
        // permutation validity ensures `taken[i]` is `Some`.
        reordered.push(taken[i].take().expect("valid permutation"));
    }
    group.triggers = reordered;
    group.ordered = true;

    // More groups awaiting a choice? Emit the next prompt.
    if let Some(wf) = build_next_order_triggers_prompt(state) {
        return Ok(wf);
    }

    // All groups ordered — concatenate in APNAP placement order and
    // dispatch through the same loop `process_triggers` uses. Reset
    // `state.waiting_for` to Priority before dispatch so the post-dispatch
    // check below correctly detects whether dispatch set a NEW pause state
    // (vs leaving the stale `OrderTriggers` that we entered with).
    let order_state = state
        .pending_trigger_order
        .take()
        .expect("pending_trigger_order populated above");
    let resume_after_ordering = order_state.resume_after_ordering.map(|wf| *wf);
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    let pending: Vec<PendingTriggerContext> = order_state
        .groups
        .into_iter()
        .flat_map(|g| g.triggers)
        .collect();
    dispatch_collected_triggers(state, pending);

    // After dispatch, `state.pending_trigger` and/or `state.waiting_for` may
    // have been set by `dispatch_pending_trigger_context` (target-selection,
    // distribute-among, etc.). Surface whichever target-selection state the
    // engine entered, mirroring the `ChooseReplacement` post-handler pipeline.
    if state.pending_trigger.is_some()
        && !matches!(
            state.waiting_for,
            WaitingFor::DistributeAmong { .. } | WaitingFor::TriggerTargetSelection { .. }
        )
    {
        if let Some(wf) = super::engine::begin_pending_trigger_target_selection(state)? {
            return Ok(wf);
        }
    }
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        // dispatch_pending_trigger_context set a non-Priority waiting_for
        // (DistributeAmong, etc.) — surface it.
        return Ok(state.waiting_for.clone());
    }
    // No trigger-specific pause remains. Restore an interrupted casting/payment
    // state if this ordering pass preempted one; otherwise fall through to
    // Priority for the active player.
    Ok(resume_after_ordering.unwrap_or(WaitingFor::Priority {
        player: state.active_player,
    }))
}

/// CR 603.2 + CR 603.3b: Collect triggers matching `events` and enqueue them
/// into `state.deferred_triggers` *without dispatching* — never touches
/// `state.pending_trigger` or `state.waiting_for`, never pushes to the stack.
///
/// This is the collect-only counterpart to `process_triggers`, used by
/// resolution-choice handlers (`engine_resolution_choices.rs`) that move a
/// permanent to the graveyard inside the handler (e.g. `Effect::Sacrifice`).
/// In that flow `waiting_for` is not `Priority`, so `run_post_action_pipeline`
/// cannot run `process_triggers` for the sacrifice events; the resulting
/// dies-triggers (Undying, Blood Artist) would otherwise be lost (issue #423).
/// Batching them into `deferred_triggers` lets the handler dispatch them
/// itself via `drain_deferred_trigger_queue` once its reflexive continuation
/// has resolved, or lets `finalize_trigger_target_selection` drain them later
/// if the continuation paused on a target.
pub(crate) fn collect_triggers_into_deferred(state: &mut GameState, events: &[GameEvent]) {
    let pending = collect_pending_triggers(state, events);
    state.deferred_triggers.extend(pending);
}

/// CR 603.3: Put triggered ability on the stack. Returns the `ObjectId` of the
/// newly created `StackEntry` so callers that need to track the entry for
/// in-construction mutation (mode / target / division still being chosen) can
/// stash it in `state.pending_trigger_entry`.
pub fn push_pending_trigger_to_stack(
    state: &mut GameState,
    trigger: PendingTrigger,
    events: &mut Vec<GameEvent>,
) -> ObjectId {
    let trigger_events = take_pending_trigger_event_batch(state, &trigger);
    push_pending_trigger_to_stack_with_event_batch(state, trigger, trigger_events, events)
}

pub(crate) fn take_pending_trigger_event_batch(
    state: &mut GameState,
    trigger: &PendingTrigger,
) -> Vec<GameEvent> {
    if state
        .pending_trigger_event_batch
        .first()
        .is_some_and(|event| Some(event) == trigger.trigger_event.as_ref())
    {
        std::mem::take(&mut state.pending_trigger_event_batch)
    } else {
        state.pending_trigger_event_batch.clear();
        trigger.trigger_event.iter().cloned().collect()
    }
}

/// CR 603.3 + CR 603.3c + CR 603.3d: Push a pending trigger to the stack with
/// its event batch keyed by entry id. Returns the new entry's `ObjectId` so
/// callers can stash it in `state.pending_trigger_entry` when the entry is
/// being constructed in pieces across multiple `WaitingFor` cycles (mode
/// choice, target selection, distribute-among).
pub(crate) fn push_pending_trigger_to_stack_with_event_batch(
    state: &mut GameState,
    trigger: PendingTrigger,
    trigger_events: Vec<GameEvent>,
    events: &mut Vec<GameEvent>,
) -> ObjectId {
    let PendingTrigger {
        source_id,
        controller,
        condition,
        mut ability,
        trigger_event,
        description,
        may_trigger_origin,
        subject_match_count,
        ..
    } = trigger;

    if let Some(origin) = may_trigger_origin {
        ability.set_may_trigger_origin_recursive(origin);
    }

    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    if trigger_events.len() > 1 {
        state
            .stack_trigger_event_batches
            .insert(entry_id, trigger_events);
    }
    // Capture the source's display name at stack-push time so viewers can
    // render "From <name>" without rederiving from `objects` (display-layer
    // logic belongs in the engine per CLAUDE.md). Synthetic game-rule triggers
    // (monarch draw, rad counters) use `ObjectId(0)`, which has no object —
    // `source_name` is left empty in that case.
    let source_name = state
        .objects
        .get(&source_id)
        .map(|o| o.name.clone())
        .unwrap_or_default();
    let entry = StackEntry {
        id: entry_id,
        source_id,
        controller,
        kind: StackEntryKind::TriggeredAbility {
            source_id,
            ability: Box::new(ability),
            condition,
            trigger_event,
            description,
            source_name,
            subject_match_count,
        },
    };
    stack::push_to_stack(state, entry, events);
    entry_id
}

/// CR 603.3c + CR 603.3d: True iff the top of `state.stack` is the trigger
/// entry currently being constructed (mode / target / division still being
/// chosen). Callers MUST guard `drain_deferred_trigger_queue` invocations with
/// this — the deferred-trigger drain pipeline would otherwise push siblings
/// onto a stack whose top is mid-construction and the resolver would either
/// fire the in-flight entry or fire siblings out of order.
pub(crate) fn is_pending_trigger_construction_active(state: &GameState) -> bool {
    state.pending_trigger_entry.is_some()
}

/// CR 603.3c + CR 603.3d: Overwrite the in-construction stack entry's resolved
/// ability with `source_ability`, leaving `pending_trigger_entry` untouched.
/// Use for intermediate construction steps (e.g. a mode chosen while target
/// selection is still outstanding) where the entry must remain non-resolvable.
///
/// Invariants (panic on violation — the push-first contract guarantees them):
/// * `state.pending_trigger_entry` is `Some(_)`.
/// * That id references a `TriggeredAbility` `StackEntry` in `state.stack`.
pub(crate) fn mutate_pending_trigger_entry(
    state: &mut GameState,
    source_ability: &ResolvedAbility,
) {
    let entry_id = state
        .pending_trigger_entry
        .expect("mutate_pending_trigger_entry: pending_trigger_entry must be set under the push-first contract");
    assign_pending_trigger_entry_ability(state, entry_id, source_ability);
}

/// CR 603.3c + CR 603.3d: Overwrite the in-construction stack entry's resolved
/// ability with `source_ability` AND clear `pending_trigger_entry` —
/// construction is complete, so the resolver is now free to fire this entry.
///
/// Invariants (panic on violation — no recovery path):
/// * `state.pending_trigger_entry` is `Some(_)` (every caller pushed under the
///   push-first contract).
/// * That id references a `TriggeredAbility` `StackEntry` in `state.stack`.
pub(crate) fn finalize_pending_trigger_entry(
    state: &mut GameState,
    source_ability: &ResolvedAbility,
) {
    let entry_id = state
        .pending_trigger_entry
        .take()
        .expect("finalize_pending_trigger_entry: pending_trigger_entry must be set under the push-first contract");
    assign_pending_trigger_entry_ability(state, entry_id, source_ability);
}

/// Locate the in-construction `TriggeredAbility` entry identified by `entry_id`
/// (searching from the top of the stack down) and overwrite its resolved
/// ability. Shared find-and-assign logic for `mutate`/`finalize` above.
fn assign_pending_trigger_entry_ability(
    state: &mut GameState,
    entry_id: ObjectId,
    source_ability: &ResolvedAbility,
) {
    let entry = state
        .stack
        .iter_mut()
        .rev()
        .find(|entry| entry.id == entry_id)
        .expect("pending_trigger_entry must reference a stack entry");
    let ability = entry
        .ability_mut()
        .expect("pending_trigger_entry must reference a TriggeredAbility stack entry");
    *ability = source_ability.clone();
}

/// CR 603.2 + CR 603.3b + CR 309.4c: Dispatch a synthetic
/// single trigger (game-rule trigger queued mid-resolution, e.g. dungeon
/// room ability from `effects::venture::queue_room_trigger`). Delegates
/// to the same pipeline as `process_triggers` so target slots, modal
/// choice, distribute-among, and mana abilities are handled identically.
/// Returns `true` if the dispatch paused on player input (target / mode
/// / distribute prompt), `false` if the trigger reached the stack or
/// resolved inline.
pub(crate) fn dispatch_synthetic_trigger(
    state: &mut GameState,
    trigger: PendingTrigger,
    events_out: &mut Vec<GameEvent>,
) -> bool {
    dispatch_pending_trigger_context(state, PendingTriggerContext::single(trigger), events_out)
}

/// CR 113.2c + CR 603.2 + CR 603.3b: Drive a single collected trigger through
/// its disposition. Returns `true` when the trigger paused on player input
/// (modal mode choice, target selection, or division-among) — callers must
/// then stash the remaining queue into `state.deferred_triggers`. Returns
/// `false` when the trigger reached the stack (or resolved inline as a mana
/// ability, or was dropped because targets became illegal).
///
/// All three pause paths set `state.pending_trigger` / `state.waiting_for`
/// (where appropriate) before returning so the engine's existing
/// `begin_pending_trigger_target_selection` / mode-choice / distribute-among
/// dispatchers pick up the active trigger unchanged.
fn dispatch_pending_trigger_context(
    state: &mut GameState,
    trigger_context: PendingTriggerContext,
    events_out: &mut Vec<GameEvent>,
) -> bool {
    let PendingTriggerContext {
        pending: mut trigger,
        trigger_events,
    } = trigger_context;

    // CR 603.3c: Modal triggered ability — push the entry to the stack FIRST
    // (in mid-construction state; the on-stack `ResolvedAbility` does not yet
    // know which mode is selected), then prompt for mode. Mode/target/division
    // data lives in `state.pending_trigger` until `handle_triggered_mode_choice`
    // mutates the on-stack entry's `ability` with the resolved choice. The
    // resolver refuses to fire entries identified by `pending_trigger_entry`
    // (see `stack::resolve_top`), so the in-flight entry is safe at the top of
    // the stack until construction completes.
    //
    // Exception: when no legal mode can be chosen (CR 603.3c "If no mode can
    // be chosen, the ability is removed from the stack"), the trigger is
    // dropped before any `StackPushed` event is emitted — the entry never
    // exists on the stack.
    if let Some(modal_ref) = trigger.modal.as_ref() {
        if !trigger.mode_abilities.is_empty() {
            let modal_for_player = super::ability_utils::modal_choice_for_player(
                state,
                trigger.controller,
                trigger.source_id,
                modal_ref,
                &crate::types::ability::SpellContext::default(),
            );
            let mut unavailable_modes = super::ability_utils::compute_unavailable_modes(
                state,
                trigger.source_id,
                &modal_for_player,
            );
            let context_snapshot = push_trigger_event_context(
                state,
                trigger.trigger_event.as_ref(),
                &trigger_events,
                trigger.subject_match_count,
            );
            super::ability_utils::filter_modes_by_target_legality(
                state,
                trigger.source_id,
                trigger.controller,
                &trigger.mode_abilities,
                &modal_for_player,
                &mut unavailable_modes,
            );
            restore_trigger_event_context(state, context_snapshot);
            if unavailable_modes.len() >= modal_for_player.mode_count {
                // CR 603.3c: No legal mode; drop the trigger entirely.
                return false;
            }
            let pending_for_state = trigger.clone();
            let entry_id = push_pending_trigger_to_stack_with_event_batch(
                state,
                trigger,
                trigger_events.clone(),
                events_out,
            );
            state.pending_trigger_event_batch = trigger_events;
            state.pending_trigger = Some(pending_for_state);
            state.pending_trigger_entry = Some(entry_id);
            return true;
        }
    }

    let trigger_event = trigger.trigger_event.clone();
    let subject_match_count = trigger.subject_match_count;
    let context_snapshot = push_trigger_event_context(
        state,
        trigger_event.as_ref(),
        &trigger_events,
        subject_match_count,
    );

    let target_slots = match super::ability_utils::build_target_slots(state, &trigger.ability) {
        Ok(target_slots) => target_slots,
        Err(_) => {
            restore_trigger_event_context(state, context_snapshot);
            return false;
        }
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
                trigger.trigger_event.as_ref(),
                events_out,
            );
            restore_trigger_event_context(state, context_snapshot);
            return false;
        }
        push_pending_trigger_to_stack_with_event_batch(state, trigger, trigger_events, events_out);
        restore_trigger_event_context(state, context_snapshot);
        return false;
    }

    // CR 115.1 + CR 701.9b: Random-target triggered abilities short-circuit
    // to RNG-driven selection. Falls back to controller-choice degenerate
    // auto-select otherwise.
    let auto_targets = if matches!(
        trigger.ability.target_selection_mode,
        crate::types::ability::TargetSelectionMode::Random
    ) {
        super::ability_utils::random_select_targets_for_ability(
            state,
            &target_slots,
            &trigger.target_constraints,
        )
        .map(Some)
    } else {
        super::ability_utils::auto_select_targets_for_ability(
            state,
            &trigger.ability,
            &target_slots,
            &trigger.target_constraints,
        )
    };

    match auto_targets {
        Ok(Some(targets)) => {
            if super::ability_utils::assign_targets_in_chain(state, &mut trigger.ability, &targets)
                .is_err()
            {
                restore_trigger_event_context(state, context_snapshot);
                return false;
            }
            super::casting::emit_targeting_events(
                state,
                &super::ability_utils::flatten_targets_in_chain(&trigger.ability),
                trigger.source_id,
                trigger.controller,
                events_out,
            );
            if let Some(unit) = trigger.distribute.clone() {
                if let Some(total) = super::casting_targets::extract_distribution_total(
                    state,
                    &trigger.ability,
                    &trigger.ability.effect,
                ) {
                    let assigned_targets =
                        super::ability_utils::flatten_targets_in_chain(&trigger.ability);
                    if assigned_targets.len() == 1 {
                        trigger.ability.distribution =
                            Some(vec![(assigned_targets[0].clone(), total)]);
                    } else {
                        // CR 601.2d + CR 603.3d: Distribute-among with targets
                        // already chosen but division still pending. Push the
                        // entry to the stack FIRST (ability has `targets`
                        // populated, `distribution` still empty), then prompt
                        // for division. `engine_stack::finalize_trigger_target_selection`
                        // mutates the on-stack entry's `ability.distribution`
                        // when the division choice completes.
                        let player = trigger.controller;
                        let pending_for_state = trigger.clone();
                        let entry_id = push_pending_trigger_to_stack_with_event_batch(
                            state,
                            trigger,
                            trigger_events.clone(),
                            events_out,
                        );
                        state.pending_trigger_event_batch = trigger_events;
                        state.pending_trigger = Some(pending_for_state);
                        state.pending_trigger_entry = Some(entry_id);
                        state.waiting_for = crate::types::game_state::WaitingFor::DistributeAmong {
                            player,
                            total,
                            targets: assigned_targets,
                            unit,
                        };
                        restore_trigger_event_context(state, context_snapshot);
                        return true;
                    }
                }
            }
            push_pending_trigger_to_stack_with_event_batch(
                state,
                trigger,
                trigger_events,
                events_out,
            );
            restore_trigger_event_context(state, context_snapshot);
            false
        }
        Ok(None) => {
            // CR 601.2c + CR 603.3d: Manual target selection pending. Push the
            // entry to the stack FIRST (ability has empty `targets`), then
            // prompt for target. `engine_stack::finalize_trigger_target_selection`
            // mutates the on-stack entry's `ability.targets` when each target
            // is chosen.
            let pending_for_state = trigger.clone();
            let entry_id = push_pending_trigger_to_stack_with_event_batch(
                state,
                trigger,
                trigger_events.clone(),
                events_out,
            );
            state.pending_trigger_event_batch = trigger_events;
            state.pending_trigger = Some(pending_for_state);
            state.pending_trigger_entry = Some(entry_id);
            restore_trigger_event_context(state, context_snapshot);
            true
        }
        Err(_) => {
            restore_trigger_event_context(state, context_snapshot);
            false
        }
    }
}

/// CR 113.2c + CR 603.2 + CR 603.3b: Drain the deferred-trigger queue after
/// the active `pending_trigger` has been resolved (target chosen, mode
/// chosen, distribution assigned) and pushed to the stack. Each queued
/// trigger is dispatched FIFO. If one of them pauses on player input, the
/// caller returns the resulting `WaitingFor` (already set on `state`) and the
/// queue retains the still-unprocessed remainder. If the queue fully drains,
/// returns the events produced (caller appends them to its own event vec).
///
/// Returns `Some(waiting_for)` if the drain paused on a deferred trigger
/// needing input (its target-selection / mode-choice / distribute-among
/// `WaitingFor` to enter), or `None` if every deferred trigger reached the
/// stack and the caller should continue with its existing `WaitingFor`
/// (typically `Priority`).
pub(crate) fn drain_deferred_trigger_queue(
    state: &mut GameState,
    events_out: &mut Vec<GameEvent>,
) -> Option<crate::types::game_state::WaitingFor> {
    while !state.deferred_triggers.is_empty() {
        let next = state.deferred_triggers.remove(0);
        if dispatch_pending_trigger_context(state, next, events_out) {
            // Paused on player input — the dispatcher set
            // `state.pending_trigger` (and `state.waiting_for` for
            // distribute-among). Defer to the engine's existing transition
            // logic: for target/mode selection the caller invokes
            // `begin_pending_trigger_target_selection`; for distribute-among
            // the dispatcher already set `state.waiting_for`.
            if matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::DistributeAmong { .. }
            ) {
                return Some(state.waiting_for.clone());
            }
            return super::engine::begin_pending_trigger_target_selection(state)
                .ok()
                .flatten();
        }
    }
    None
}

/// CR 603.2d: Apply trigger doubling from `StaticMode::DoubleTriggers`
/// static abilities. Scans battlefield for permanents with a DoubleTriggers
/// static, then clones matching pending triggers an additional time. The
/// `TriggerCause` predicate restricts which spawning events qualify
/// (Panharmonicon: ETB; Isshin: creature attacking; Any: unrestricted).
fn apply_trigger_doubling(state: &GameState, pending: &mut Vec<PendingTriggerContext>) {
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

    let mut extra: Vec<PendingTriggerContext> = Vec::new();
    for (doubler_controller, doubler_id, cause, ref affected) in &doublers {
        for trigger_context in pending.iter() {
            let trigger = &trigger_context.pending;
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
            extra.push(trigger_context.clone());
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

                let target_constraints = execute.target_constraints.clone();
                let ability = build_resolved_from_def(&execute, obj_id, controller);
                pending.push(PendingTrigger {
                    source_id: obj_id,
                    controller,
                    condition: trigger.condition.clone(),
                    ability,
                    timestamp,
                    target_constraints,
                    distribute: trigger
                        .execute
                        .as_ref()
                        .and_then(|execute| execute.distribute.clone()),
                    trigger_event: None,
                    modal: None,
                    mode_abilities: vec![],
                    description: trigger.description.clone(),
                    may_trigger_origin: None,
                    subject_match_count: None,
                });
            }
        }
    }

    if pending.is_empty() {
        return;
    }

    // CR 603.3b + CR 101.4: Full APNAP stack-placement order for state triggers
    // (active player lowest, then each non-active player in turn order),
    // tiebroken by timestamp.
    let apnap = crate::game::players::apnap_order(state);
    pending.sort_by_key(|t| (apnap_rank(&apnap, t.controller), t.timestamp));

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

    // CR 603.3b + CR 101.4: Full APNAP stack-placement order — active player's
    // triggers go lowest, then each non-active player's triggers in turn order.
    // The old `state.turn_number` tiebreaker was constant across this batch, so
    // dropping it changes nothing; `sort_by_key` is stable, preserving the
    // prior same-controller ordering before stack placement.
    let apnap = crate::game::players::apnap_order(state);
    to_fire.sort_by_key(|(trigger, _)| apnap_rank(&apnap, trigger.controller));

    for (trigger, trigger_event) in to_fire {
        let pending = PendingTrigger {
            source_id: trigger.source_id,
            controller: trigger.controller,
            condition: None,
            ability: trigger.ability,
            timestamp: state.turn_number,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
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
        // CR 603.7b: An `AtNextPhase` delayed trigger fires once, the next time
        // its `PhaseChanged` event occurs (e.g. Flickerwisp's "at the beginning
        // of the next end step" return).
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
        DelayedTriggerCondition::WhenDies { filter } => delayed_zone_change_event(
            events,
            state,
            source_id,
            controller,
            Some(Zone::Battlefield),
            Some(Zone::Graveyard),
            filter,
        ),
        // CR 603.7c: "when [object] leaves the battlefield" — any zone change from battlefield
        DelayedTriggerCondition::WhenLeavesPlayFiltered { filter } => delayed_zone_change_event(
            events,
            state,
            source_id,
            controller,
            Some(Zone::Battlefield),
            None,
            filter,
        ),
        // CR 603.7c: "when [object] enters the battlefield" — zone change to battlefield
        DelayedTriggerCondition::WhenEntersBattlefield { filter } => delayed_zone_change_event(
            events,
            state,
            source_id,
            controller,
            None,
            Some(Zone::Battlefield),
            filter,
        ),
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

fn delayed_zone_change_event(
    events: &[GameEvent],
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    from: Option<Zone>,
    to: Option<Zone>,
    filter: &crate::types::ability::TargetFilter,
) -> Option<GameEvent> {
    events
        .iter()
        .find(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    from: event_from,
                    to: event_to,
                    ..
                } if from.is_none_or(|zone| *event_from == Some(zone))
                    && to.is_none_or(|zone| *event_to == zone)
                    && crate::game::filter::matches_target_filter(
                        state,
                        *object_id,
                        filter,
                        &FilterContext::from_source_with_controller(source_id, controller),
                    )
            )
        })
        .cloned()
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
            let spells = state.spells_cast_this_turn_by_player.get(&caster);
            if let (Some(filter), Some(current_record)) =
                (filter.as_ref(), spells.and_then(|spells| spells.back()))
            {
                if !spell_record_matches_filter(
                    current_record,
                    filter,
                    caster,
                    &state.all_creature_types,
                ) {
                    return false;
                }
            }
            let count = spells.map_or(0, |spells| match filter {
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
        // CR 121.2: Use the ordinal stamped onto the individual draw event
        // rather than the final per-turn count after a multi-card draw batch.
        TriggerConstraint::NthDrawThisTurn { n } => {
            let nth_in_turn = match event {
                GameEvent::CardDrawn { nth_in_turn, .. } => *nth_in_turn,
                _ => return false,
            };
            nth_in_turn == *n
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
        TriggerCondition::SourceEnteredThisTurn => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.entered_battlefield_turn == Some(state.turn_number)),
        TriggerCondition::EchoDue => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.echo_due),
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
        //
        // Negation ("if it wasn't cast" / "if none of them were cast") wraps via
        // `Not { Box::new(WasCast) }`. The `Not` arm inverts the result, so a
        // missing entering-object resolves Not(WasCast) to `true` (consistent
        // with CR 603.4's intervening-if being permissive when source state is
        // indeterminate; the ability is removed from the stack at resolution
        // anyway per CR 603.4 if the source has left the relevant zone).
        // CR 601.2 + CR 603.4: cast-origin check. zone=None → cast from anywhere
        // (Discover/Wedding Ring/Satoru back-compat). zone=Some(z) → cast specifically
        // from zone z (Twilight Diviner: graveyard). Reads the ENTERING object's
        // cast_from_zone, never the trigger source.
        TriggerCondition::WasCast { zone } => {
            let checked_id = trigger_event
                .and_then(|e| match e {
                    GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                    _ => None,
                })
                .or(source_id);
            checked_id
                .and_then(|id| state.objects.get(&id))
                .and_then(|obj| obj.cast_from_zone)
                .is_some_and(|cz| zone.is_none_or(|z| cz == z))
        }
        // CR 603.4 + CR 603.6a: "put onto the battlefield with this ability" —
        // the entering object was placed by THIS trigger's source ability.
        // Resolve the entering object from the ZoneChanged event (self-referential
        // triggers fall back to the trigger source); compare its
        // `entered_via_ability_source` to the trigger source id. The negation
        // ("wasn't ... with this ability") wraps via `Not`.
        TriggerCondition::PlacedByAbilitySource => {
            let checked_id = trigger_event
                .and_then(|e| match e {
                    GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                    _ => None,
                })
                .or(source_id);
            matches!(
                (
                    checked_id
                        .and_then(|id| state.objects.get(&id))
                        .and_then(|o| o.entered_via_ability_source),
                    source_id,
                ),
                (Some(via), Some(src)) if via == src
            )
        }
        // CR 305.1 + CR 603.4: "without being played" is encoded as
        // `Not(WasPlayed)` and checks the triggering zone-change object first.
        TriggerCondition::WasPlayed => {
            let checked_id = trigger_event
                .and_then(|e| match e {
                    GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                    _ => None,
                })
                .or(source_id);
            checked_id
                .and_then(|id| state.objects.get(&id))
                .is_some_and(|obj| obj.played_from_zone.is_some())
        }
        // CR 603.4 + CR 702.33d-f: "if it was kicked" intervening-if.
        // ETB/LTB trigger conditions refer to the triggering zone-change
        // object; self-referential triggers fall back to the trigger source.
        TriggerCondition::AdditionalCostPaid {
            source,
            variant,
            kicker_cost,
            min_count,
        } => {
            if kicker_cost.is_some() && variant.is_none() {
                false
            } else {
                let checked_id = trigger_event
                    .and_then(|event| match event {
                        GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                        _ => None,
                    })
                    .or(source_id);
                checked_id
                    .and_then(|id| state.objects.get(&id))
                    .is_some_and(|obj| match variant {
                        Some(kicker) => obj.kickers_paid.contains(kicker),
                        None => crate::types::ability::additional_cost_payment_count_matches(
                            *source,
                            obj.additional_cost_payment_count > 0 || !obj.kickers_paid.is_empty(),
                            obj.kickers_paid.len(),
                            obj.additional_cost_payment_count,
                            *min_count,
                        ),
                    })
            }
        }
        // CR 508.1: "if it's attacking" — true when the trigger source is in combat.attackers.
        TriggerCondition::SourceIsAttacking => {
            let sid = source_id.unwrap_or(ObjectId(0));
            state
                .combat
                .as_ref()
                .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == sid))
        }
        // CR 702.49 + CR 702.190a + CR 603.4: "if its sneak/ninjutsu cost was paid
        // this turn". Negation ("unless it escaped") wraps via `Not`.
        TriggerCondition::CastVariantPaid { variant } => source_id
            .and_then(|id| state.objects.get(&id))
            .map(|obj| obj.cast_variant_paid == Some((*variant, state.turn_number)))
            .unwrap_or(false),
        // CR 605.1a: "that isn't a mana ability" gate on activated-ability
        // trigger events. `KeywordAbilityActivated` carries the explicit flag
        // (Exhaust mana abilities still emit this event). `AbilityActivated`
        // is emitted only by stack-using activations (CR 605.3b: mana
        // abilities never reach the stack-pushing emission sites), so it
        // trivially satisfies the qualifier; the explicit arm keeps the
        // AST-level qualifier honest if the event family ever widens.
        TriggerCondition::ActivatedAbilityIsNonMana => match trigger_event {
            Some(GameEvent::KeywordAbilityActivated {
                is_mana_ability, ..
            }) => !*is_mana_ability,
            Some(GameEvent::AbilityActivated { .. }) => true,
            _ => false,
        },
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
        // CR 603.4 + CR 603.6 + CR 603.10: Intervening-if subject is the
        // zone-change event object, not necessarily the trigger source.
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin,
            destination,
            filter,
        } => trigger_event.is_some_and(|event| {
            super::filter::matches_zone_change_event_object_filter(
                state,
                event,
                *origin,
                *destination,
                filter,
                &FilterContext::from_source(state, source_id.unwrap_or(ObjectId(0))),
            )
        }),
        // CR 603.4 + CR 611.2b: Source-bound intervening-if predicate. Reuse
        // the engine's normal TargetFilter matcher so properties such as
        // enchanted/equipped, attacked this turn, and other composable
        // source-state checks do not need bespoke TriggerCondition siblings.
        TriggerCondition::SourceMatchesFilter { filter } => source_id.is_some_and(|id| {
            matches_target_filter(state, id, filter, &FilterContext::from_source(state, id))
        }),
        // CR 614.12c + CR 607.2d + CR 603.4: True iff the trigger source's
        // persisted `ChosenAttribute::Label` (set when the anchor-word
        // permanent entered the battlefield) matches the linked anchor word.
        // Case-insensitive to match the persistence canonicalisation used by
        // `StaticCondition::ChosenLabelIs`.
        TriggerCondition::ChosenLabelIs { label } => source_id
            .and_then(|id| state.objects.get(&id))
            .and_then(|obj| obj.chosen_label())
            .is_some_and(|chosen| chosen.eq_ignore_ascii_case(label)),
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
        // CR 603.4 + CR 102.1: "if it's <player>'s turn" — true when the named
        // player is currently the active player. Negation ("if it isn't <player>'s
        // turn") wraps via `Not { Box::new(DuringPlayersTurn { player }) }`.
        //
        // The match is exhaustive over PlayerFilter so future additions force a
        // deliberate decision here. Variants with no single-player "whose turn"
        // semantic (set-valued predicates, action-result predicates) fail-closed.
        TriggerCondition::DuringPlayersTurn { player } => match player {
            // CR 102.1: "your turn" — controller is active.
            PlayerFilter::Controller => state.active_player == controller,
            // CR 102.1 + CR 102.2: "an opponent's turn" — active player is any
            // non-controller (set-valued match: true whenever it isn't your turn).
            PlayerFilter::Opponent => state.active_player != controller,
            // CR 603.4 + CR 102.1: "that player's turn" — the player named by
            // the trigger event (drawer / tapper / damaged player / etc.) is
            // currently the active player.
            PlayerFilter::TriggeringPlayer => trigger_event
                .and_then(|e| crate::game::targeting::extract_player_from_event(e, state))
                .is_some_and(|p| state.active_player == p),
            // Set-valued / action-result / no-turn-binding variants: no natural
            // "whose turn" semantic. Fail-closed.
            PlayerFilter::DefendingPlayer
            | PlayerFilter::OpponentLostLife
            | PlayerFilter::OpponentGainedLife
            // CR 120.1 + CR 510.1: a set-valued combat-damaged-this-turn
            // predicate has no single-player "whose turn" semantic.
            | PlayerFilter::OpponentDealtCombatDamage { .. }
            // CR 508.6: a set-valued attacked-this-turn predicate has no
            // single-player "whose turn" semantic.
            | PlayerFilter::OpponentAttackedThisTurn
            | PlayerFilter::All
            | PlayerFilter::HighestSpeed
            | PlayerFilter::ZoneChangedThisWay
            | PlayerFilter::PerformedActionThisWay { .. }
            | PlayerFilter::VotedFor { .. }
            | PlayerFilter::OwnersOfCardsExiledBySource
            | PlayerFilter::ParentObjectTargetController
            // CR 102.1: a controls-a-permanent population predicate is
            // set-valued — it has no single-player "whose turn" semantic.
            // Fail-closed alongside the other set-valued variants.
            | PlayerFilter::ControlsCount { .. }
            // CR 402.1 / 119.1 / 122.1f / 404.1: a player-scalar population
            // predicate is likewise set-valued — no "whose turn" semantic.
            | PlayerFilter::PlayerAttribute { .. }
            | PlayerFilter::OpponentOtherThanTriggering => false,
        },
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
        // CR 500.8 + CR 506.1 + CR 603.4: Intervening-if for "if it's the
        // first combat phase of the turn".
        TriggerCondition::FirstCombatPhaseOfTurn => state.combat_phases_started_this_turn == 1,
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
            // so event-scoped refs (e.g. triggering-spell mana spent) must resolve
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
            .counter_added_this_turn
            .iter()
            .any(|record| record.actor == controller),
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
        // CR 103.1: True when the scoped player took the first turn of the
        // game (fixed at game start). The parser only emits
        // `ControllerRef::You` (Radiant Smite's Cycling trigger — "if you
        // weren't the starting player").
        TriggerCondition::WasStartingPlayer { .. } => state.current_starting_player == controller,
        // CR 702.185c: True when any player cast a spell using `variant` (e.g.
        // Warp) this turn. Not controller-scoped — scans every player's
        // turn-history.
        TriggerCondition::SpellCastWithVariantThisTurn { variant } => {
            crate::game::restrictions::spell_cast_with_variant_this_turn(state, variant)
        }
        // CR 725.1: True when the controller is the monarch.
        TriggerCondition::IsMonarch => state.monarch == Some(controller),
        // CR 725.1: True when no player holds the monarch designation.
        TriggerCondition::NoMonarch => state.monarch.is_none(),
        // CR 702.131a: True when the controller has the city's blessing.
        TriggerCondition::HasCityBlessing => state.city_blessing.contains(&controller),
        // CR 110.5b: True when the trigger source is tapped. Negation ("untapped")
        // wraps via `Not { Box::new(SourceIsTapped) }`.
        TriggerCondition::SourceIsTapped => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.tapped),
        // CR 603.4 + CR 603.6a + CR 110.5b: "enters tapped" rider — the subject
        // is the permanent named by the triggering zone-change event (the
        // entering permanent), not the ability's own source. Resolve the
        // entering object from `trigger_event`; fall back to `source_id` for
        // the SelfRef case where the entering permanent IS the source.
        // Negation ("enters untapped") wraps via `Not`. Permissive on a missing
        // object: an unfindable id yields `false` here (so `Not` yields `true`),
        // matching the `WasCast` arm's documented permissive-on-missing behavior.
        TriggerCondition::ZoneChangeObjectIsTapped => trigger_event
            .and_then(|e| match e {
                GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .or(source_id)
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.tapped),
        // CR 701.27g: True when the trigger source is a transformed permanent (DFC
        // with its back face up). Negation wraps via `Not { Box::new(SourceIsTransformed) }`.
        TriggerCondition::SourceIsTransformed => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.transformed),
        // CR 708.2: True when the trigger source is face-up. Face-up is the inverse
        // of the GameObject `face_down` flag — there is no separate `face_up` field.
        // Negation wraps via `Not { Box::new(SourceIsFaceUp) }`.
        TriggerCondition::SourceIsFaceUp => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| !obj.face_down),
        // CR 708.2: True when the trigger source is face-down. Negation wraps via
        // `Not { Box::new(SourceIsFaceDown) }`.
        TriggerCondition::SourceIsFaceDown => source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.face_down),
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
        // CR 207.2c + CR 601.2: cast during the configured phase set.
        TriggerCondition::CastDuringPhase { phases } => phases.contains(&state.phase),
        // CR 601.3b + CR 702.8a: source permanent came from a spell cast using
        // the specified timing permission this turn.
        TriggerCondition::CastTimingPermission { permission } => source_id
            .and_then(|id| state.objects.get(&id))
            .map(|obj| obj.cast_timing_permission == Some((*permission, state.turn_number)))
            .unwrap_or(false),
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
                Some(ct) => lki.counters.get(ct).is_some_and(|&v| v > 0),
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
        // CR 603.4 + CR 608.2c: Logical negation — invert the wrapped condition's
        // truth value. Used for "unless [phrase]" intervening-if patterns; mirrors
        // `TargetFilter::Not` and `StaticCondition::Not`.
        TriggerCondition::Not { condition } => {
            !check_trigger_condition(state, condition, controller, source_id, trigger_event)
        }
        // CR 309.7: True when the controller has completed a dungeon. `specific: None`
        // matches "any dungeon"; `specific: Some(d)` matches dungeon `d`. Negation
        // ("haven't completed Tomb of Annihilation") wraps via `Not`.
        TriggerCondition::CompletedDungeon { specific } => state
            .dungeon_progress
            .get(&controller)
            .is_some_and(|p| match specific {
                None => !p.completed.is_empty(),
                Some(dungeon) => p.completed.contains(dungeon),
            }),
        // CR 903.3 / CR 903.3d: Lieutenant ("your commander") requires ownership;
        // generic ("a commander") is controller-only.
        TriggerCondition::ControlsCommander { ownership } => match ownership {
            CommanderOwnership::Own => {
                crate::game::commander::controls_own_commander(state, controller)
            }
            CommanderOwnership::Any => {
                crate::game::commander::controls_any_commander(state, controller)
            }
        },
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
        // CR 702.33d + CR 702.33f: Propagate kicker payments from the source
        // object's `kickers_paid` (set at cast resolution) into the
        // triggered ability's context so `AbilityCondition::AdditionalCostPaid`
        // (with kicker variant or multikicker count) can evaluate.
        if let Some(obj) = state.objects.get(&source_id) {
            if !obj.kickers_paid.is_empty() {
                resolved.context.kickers_paid.clone_from(&obj.kickers_paid);
                // Maintain the legacy single-bool flag for "if it was kicked"
                // (no variant, min_count=1) so the default-shape evaluator
                // remains correct on triggered abilities (the bool reads
                // `additional_cost_paid` directly per the evaluator contract).
                resolved.context.additional_cost_paid = true;
            }
            if obj.additional_cost_payment_count > 0 {
                resolved.context.additional_cost_payment_count = obj.additional_cost_payment_count;
                resolved.context.additional_cost_paid = true;
            }
        }
        // CR 118.12: Carry unless_pay modifier from trigger definition.
        if trig_def.unless_pay.is_some() {
            resolved.unless_pay = trig_def.unless_pay.clone();
        }
        // CR 603.2b + CR 102.1: Phase triggers ("at the beginning of each
        // player's [phase], that player ...") fire when the phase begins
        // (CR 603.2b), and "that player" anaphors to the active player whose
        // phase it is (CR 102.1). Stamping `scoped_player` recursively here
        // makes `TargetFilter::ScopedPlayer` and `PlayerScope::ScopedPlayer`
        // resolve to the active player at both effect-resolution and
        // intervening-if recheck time, so Dictate of Kruphix / Kami of the
        // Crescent Moon / Howling Mine-class triggers no longer fall back to
        // the source's controller.
        if matches!(trig_def.mode, TriggerMode::Phase) {
            resolved.set_scoped_player_recursive(state.active_player);
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
    // CR 701.21a: Sacrifice does not target — the controller chooses permanents
    // at resolution time via EffectZoneChoice. Returning a filter here would
    // cause collect_target_slots to create target selection slots, routing
    // resolution through the targeted path which lacks controller scoping.
    if matches!(effect, Effect::Sacrifice { .. }) {
        return None;
    }
    // CR 115.1 + Whitemane Lion ruling: A `Bounce` whose Oracle text omitted
    // the word "target" ("return a creature you control to its owner's hand")
    // is NOT a targeted effect — the controller chooses an eligible permanent
    // at resolution time via `EffectZoneChoice`. Returning a filter here would
    // route resolution through the targeted path, which (a) creates spurious
    // target selection slots at stack-push time and (b) leaves the resolver's
    // targets vector empty, causing the effect to silently no-op. Mirrors the
    // Sacrifice carve-out above.
    if matches!(
        effect,
        Effect::Bounce {
            selection: BounceSelection::AtResolution,
            ..
        }
    ) {
        return None;
    }
    // CR 702.95a + CR 115.10a + CR 608.2d: Soulbond pair choices are not
    // targets. PairWith computes its legal partner while resolving.
    if matches!(effect, Effect::PairWith { .. }) {
        return None;
    }
    // CR 115.1 + CR 303.4f + CR 303.4g: An Aura's enchanted permanent is a
    // CHOICE, not a target — Old-Growth Troll / Bronzehide Lion / Harold and
    // Bob's return-as-Aura sub-effect contains no "target" word in Oracle
    // text. The pick happens at resolution time via
    // `WaitingFor::ReturnAsAuraTarget` (built without hexproof / protection
    // filtering per CR 702.16b's targeting scope).
    if matches!(effect, Effect::ReturnAsAura { .. }) {
        return None;
    }
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
    // CR 115.1 + CR 400.2: PutAtLibraryPosition from a private zone (hand/library)
    // is a resolution-time selection, not a casting-time target. Brainstorm's
    // "put two cards from your hand on top of your library" does not use the word
    // "target" — the player chooses cards during resolution via EffectZoneChoice.
    if let Effect::PutAtLibraryPosition { target, .. } = effect {
        if let Some(zone) = target.extract_in_zone() {
            if matches!(zone, Zone::Hand | Zone::Library) {
                return None;
            }
        }
    }
    // CR 115.1 / CR 115.1d: Only effects that use the word "target" require stack-time target
    // selection. `TargetFilter::Any` is a sentinel value meaning "broadcast to all
    // matching permanents at resolution time" — it is never a declared target on any
    // effect. The one exception is `DealDamage`, which uses `TargetFilter::Any` to
    // represent the "any target" wording in damage-dealing spells and abilities (e.g.
    // "deals 3 damage to any target"), where the player does choose a single target
    // from the combined pool of creatures, planeswalkers, and players.
    //
    // For all other effects, `TargetFilter::Any` arises in two ways: (a) as a mass
    // broadcast where the Oracle text contains no "target" keyword (e.g. "creatures
    // get -N/-M until end of turn"), or (b) as an unthreaded subject sentinel produced
    // by a sub-parser before the calling parser threads the real subject (SelfRef,
    // ParentTarget, etc.). In both cases no player-chosen target is required.
    // Generating a slot for `Any` causes a spurious WaitingFor::TriggerTargetSelection
    // entry that players and the AI cannot resolve, producing a hard freeze (issue #824
    // class).
    if effect.target_filter() == Some(&TargetFilter::Any)
        && !matches!(effect, Effect::DealDamage { .. })
    {
        return None;
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
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost,
        AggregateFunction, ChosenAttribute, ChosenSubtypeKind, CommanderOwnership, Comparator,
        ContinuousModification, ControllerRef, DelayedTriggerCondition, Duration, Effect,
        FilterProp, GainLifePlayer, KickerVariant, MultiTargetSpec, PaymentCost, PlayerFilter,
        PlayerScope, PtStat, PtValueScope, QuantityExpr, QuantityRef, ResolvedAbility,
        SearchSelectionConstraint, SharedQuality, SharedQualityRelation, StaticCondition,
        StaticDefinition, TargetFilter, TargetRef, TriggerCondition, TriggerConstraint,
        TriggerDefinition, TypeFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::events::{GameEvent, ManaTapState};
    use crate::types::game_state::{
        DelayedTrigger, DistributionUnit, GameState, SpellCastRecord, StackEntry, StackEntryKind,
        WaitingFor, ZoneChangeRecord,
    };
    use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use crate::types::keywords::{Keyword, KeywordKind};
    use crate::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::triggers::AttackTargetFilter;
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

    fn make_creature(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        id
    }

    /// Places a battlefield commander object with the given owner/controller.
    fn make_commander(state: &mut GameState, owner: PlayerId, controller: PlayerId) -> ObjectId {
        let id = make_creature(state, owner, "Test Commander", 3, 3);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.is_commander = true;
        obj.controller = controller;
        id
    }

    /// CR 903.3 + CR 109.5: the trigger-mirror Lieutenant condition ("you control
    /// your commander") is NOT satisfied by a stolen opponent's commander.
    /// Revert-discriminating: pre-fix controller-only code returns `true`.
    #[test]
    fn trigger_controls_commander_own_excludes_stolen() {
        let mut state = setup();
        // Opponent (P1) owns the commander; you (P0) have gained control.
        make_commander(&mut state, PlayerId(1), PlayerId(0));
        assert!(!check_trigger_condition(
            &state,
            &TriggerCondition::ControlsCommander {
                ownership: CommanderOwnership::Own,
            },
            PlayerId(0),
            None,
            None,
        ));
    }

    /// CR 903.3d: the generic trigger-mirror condition ("you control a commander")
    /// STILL counts a stolen opponent's commander.
    #[test]
    fn trigger_controls_commander_any_counts_stolen() {
        let mut state = setup();
        make_commander(&mut state, PlayerId(1), PlayerId(0));
        assert!(check_trigger_condition(
            &state,
            &TriggerCondition::ControlsCommander {
                ownership: CommanderOwnership::Any,
            },
            PlayerId(0),
            None,
            None,
        ));
    }

    /// CR 605.1a + CR 605.3b: `AbilityActivated` is emitted only by stack-using
    /// activations (mana abilities never reach those emission sites), so the
    /// "that isn't a mana ability" qualifier on Burning-Tree Shaman /
    /// Flamescroll Celebrant is trivially satisfied — the explicit arm keeps
    /// the AST-level gate honest if the event family ever widens.
    #[test]
    fn activated_ability_is_non_mana_accepts_ability_activated_event() {
        let state = setup();
        let event = GameEvent::AbilityActivated {
            player_id: PlayerId(0),
            source_id: ObjectId(1),
        };
        assert!(check_trigger_condition(
            &state,
            &TriggerCondition::ActivatedAbilityIsNonMana,
            PlayerId(0),
            None,
            Some(&event),
        ));
    }

    #[test]
    fn was_played_condition_checks_zone_change_object_play_provenance() {
        let mut state = setup();
        let land = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Test Plains".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.played_from_zone = Some(Zone::Hand);

        let event = GameEvent::ZoneChanged {
            object_id: land,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                land,
                Some(Zone::Hand),
                Zone::Battlefield,
            )),
        };
        assert!(check_trigger_condition(
            &state,
            &TriggerCondition::WasPlayed,
            PlayerId(0),
            None,
            Some(&event),
        ));
        assert!(!check_trigger_condition(
            &state,
            &TriggerCondition::Not {
                condition: Box::new(TriggerCondition::WasPlayed),
            },
            PlayerId(0),
            None,
            Some(&event),
        ));

        state.objects.get_mut(&land).unwrap().played_from_zone = None;
        assert!(!check_trigger_condition(
            &state,
            &TriggerCondition::WasPlayed,
            PlayerId(0),
            None,
            Some(&event),
        ));
    }

    /// CR 605.1a: With no trigger event present, the non-mana qualifier
    /// cannot be evaluated — and so must conservatively return false.
    #[test]
    fn activated_ability_is_non_mana_rejects_missing_event() {
        let state = setup();
        assert!(!check_trigger_condition(
            &state,
            &TriggerCondition::ActivatedAbilityIsNonMana,
            PlayerId(0),
            None,
            None,
        ));
    }

    fn make_soulbond_creature(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
        let id = make_creature(state, player, name, 2, 2);
        let triggers =
            crate::database::synthesis::KeywordTriggerInstaller::triggers_for(&Keyword::Soulbond);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.keywords.push(Keyword::Soulbond);
        obj.base_keywords.push(Keyword::Soulbond);
        for trigger in &triggers {
            obj.trigger_definitions.push(trigger.clone());
        }
        std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).extend(triggers);
        id
    }

    fn add_wolfir_static(state: &mut GameState, source: ObjectId) {
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::SourceOrPaired)
            .condition(StaticCondition::SourceIsPaired)
            .modifications(vec![
                ContinuousModification::AddPower { value: 4 },
                ContinuousModification::AddToughness { value: 4 },
            ]);
        let obj = state.objects.get_mut(&source).unwrap();
        obj.static_definitions.push(static_def.clone());
        std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(static_def);
    }

    fn add_becomes_target_draw_trigger(state: &mut GameState, source: ObjectId) {
        let trigger = TriggerDefinition::new(TriggerMode::BecomesTarget)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ))
            .description("Whenever this creature becomes a target, draw a card.".to_string());
        let obj = state.objects.get_mut(&source).unwrap();
        obj.trigger_definitions.push(trigger.clone());
        std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger);
    }

    fn resolve_stack_to_optional_choice(state: &mut GameState) {
        for _ in 0..20 {
            if matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }) {
                return;
            }
            assert!(!state.stack.is_empty(), "expected pending stack object");
            crate::game::engine::apply_as_current(state, GameAction::PassPriority)
                .expect("pass priority");
        }
        panic!("stack did not reach OptionalEffectChoice");
    }

    fn accept_optional_effect(state: &mut GameState) -> Vec<GameEvent> {
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );
        crate::game::engine::apply_as_current(
            state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .expect("accept optional effect")
        .events
    }

    fn choose_soulbond_partner(state: &mut GameState, target: ObjectId) -> Vec<GameEvent> {
        assert!(
            matches!(state.waiting_for, WaitingFor::PairChoice { .. }),
            "expected PairChoice, got {:?}",
            state.waiting_for
        );
        crate::game::engine::apply_as_current(
            state,
            GameAction::ChoosePair {
                partner: Some(target),
            },
        )
        .expect("choose soulbond partner")
        .events
    }

    fn select_soulbond_target_and_accept(state: &mut GameState, target: ObjectId) {
        resolve_stack_to_optional_choice(state);
        accept_optional_effect(state);
        choose_soulbond_partner(state, target);
    }

    fn resolve_stack_without_soulbond_prompt(state: &mut GameState) {
        for _ in 0..20 {
            assert!(
                !matches!(
                    state.waiting_for,
                    WaitingFor::OptionalEffectChoice { .. } | WaitingFor::PairChoice { .. }
                ),
                "unexpected Soulbond prompt: {:?}",
                state.waiting_for
            );
            if state.stack.is_empty() {
                return;
            }
            crate::game::engine::apply_as_current(state, GameAction::PassPriority)
                .expect("pass priority");
        }
        panic!("stack did not resolve");
    }

    #[test]
    fn dies_trigger_optional_composite_ability_cost_pays_and_draws_through_stack() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.players[0].life = 20;
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(200),
            false,
            Vec::new(),
        ));
        create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Drawn Card".to_string(),
            Zone::Library,
        );

        let source = make_creature(&mut state, PlayerId(0), "Miara Stand-In", 1, 2);
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.subtypes.push("Elf".to_string());
            obj.base_card_types = obj.card_types.clone();
        }
        let dying_elf = make_creature(&mut state, PlayerId(0), "Dying Elf", 1, 1);
        {
            let obj = state.objects.get_mut(&dying_elf).unwrap();
            obj.card_types.subtypes.push("Elf".to_string());
            obj.base_card_types = obj.card_types.clone();
        }

        let draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .condition(AbilityCondition::effect_performed());
        let pay_then_draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PayCost {
                cost: PaymentCost::AbilityCost {
                    cost: AbilityCost::Composite {
                        costs: vec![
                            AbilityCost::Mana {
                                cost: ManaCost::generic(1),
                            },
                            AbilityCost::PayLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                            },
                        ],
                    },
                },
                payer: TargetFilter::Controller,
            },
        )
        .sub_ability(draw)
        .optional();
        let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .execute(pay_then_draw)
            .valid_card(TargetFilter::Typed(
                TypedFilter::default()
                    .with_type(TypeFilter::Subtype("Elf".to_string()))
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another]),
            ))
            .origin(Zone::Battlefield)
            .destination(Zone::Graveyard);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, dying_elf, Zone::Graveyard, &mut events);
        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 1);

        resolve_stack_to_optional_choice(&mut state);
        accept_optional_effect(&mut state);

        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].mana_pool.mana.len(), 0);
        assert_eq!(state.players[0].life, 19);
        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[0].library.len(), 0);
    }

    #[test]
    fn soulbond_source_enters_pairs_with_selected_unpaired_creature() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let source = make_soulbond_creature(&mut state, PlayerId(0), "Soulbond Source");
        let chosen = make_creature(&mut state, PlayerId(0), "Chosen Partner", 1, 1);
        let _other = make_creature(&mut state, PlayerId(0), "Other Partner", 1, 1);

        process_triggers(
            &mut state,
            &[zone_changed_event(
                source,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );

        select_soulbond_target_and_accept(&mut state, chosen);

        assert_eq!(state.objects[&source].paired_with, Some(chosen));
        assert_eq!(state.objects[&chosen].paired_with, Some(source));
    }

    #[test]
    fn soulbond_lone_source_entering_does_not_prompt() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let source = make_soulbond_creature(&mut state, PlayerId(0), "Lone Soulbond Source");

        process_triggers(
            &mut state,
            &[zone_changed_event(
                source,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );

        assert!(state.stack.is_empty());
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::PairChoice { .. }
        ));
        assert_eq!(state.objects[&source].paired_with, None);
    }

    #[test]
    fn soulbond_source_enter_rechecks_source_on_battlefield() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let source = make_soulbond_creature(&mut state, PlayerId(0), "Soulbond Source");
        let partner = make_creature(&mut state, PlayerId(0), "Partner", 1, 1);

        process_triggers(
            &mut state,
            &[zone_changed_event(
                source,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        assert_eq!(state.stack.len(), 1);
        crate::game::zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut Vec::new());

        resolve_stack_without_soulbond_prompt(&mut state);

        assert_eq!(state.objects[&source].paired_with, None);
        assert_eq!(state.objects[&partner].paired_with, None);
    }

    #[test]
    fn soulbond_pair_choice_ignores_targeting_restrictions() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let source = make_soulbond_creature(&mut state, PlayerId(0), "Soulbond Source");
        let shrouded = make_creature(&mut state, PlayerId(0), "Shrouded Partner", 1, 1);
        let _other = make_creature(&mut state, PlayerId(0), "Other Partner", 1, 1);
        {
            let obj = state.objects.get_mut(&shrouded).unwrap();
            obj.keywords.push(Keyword::Shroud);
            obj.base_keywords.push(Keyword::Shroud);
        }

        process_triggers(
            &mut state,
            &[zone_changed_event(
                source,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        select_soulbond_target_and_accept(&mut state, shrouded);

        assert_eq!(state.objects[&source].paired_with, Some(shrouded));
        assert_eq!(state.objects[&shrouded].paired_with, Some(source));
    }

    #[test]
    fn soulbond_partner_choice_does_not_become_target() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let source = make_soulbond_creature(&mut state, PlayerId(0), "Soulbond Source");
        let partner = make_creature(&mut state, PlayerId(0), "Target Watcher", 1, 1);
        add_becomes_target_draw_trigger(&mut state, partner);

        process_triggers(
            &mut state,
            &[zone_changed_event(
                source,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }),
            "Soulbond must not choose a partner before the trigger is on the stack"
        );
        resolve_stack_to_optional_choice(&mut state);
        let accept_events = accept_optional_effect(&mut state);
        let choose_events = choose_soulbond_partner(&mut state, partner);

        assert!(
            accept_events
                .iter()
                .chain(choose_events.iter())
                .all(|event| !matches!(event, GameEvent::BecomesTarget { .. })),
            "Soulbond partner choice must not emit BecomesTarget"
        );
        assert!(
            !state.stack.iter().any(|entry| entry.source_id == partner),
            "a becomes-target trigger on the partner must not fire"
        );
        assert_eq!(state.objects[&source].paired_with, Some(partner));
    }

    #[test]
    fn soulbond_other_creature_enters_pairs_with_source() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let source = make_soulbond_creature(&mut state, PlayerId(0), "Soulbond Source");
        let entrant = make_creature(&mut state, PlayerId(0), "New Partner", 1, 1);

        process_triggers(
            &mut state,
            &[zone_changed_event(
                entrant,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        resolve_stack_to_optional_choice(&mut state);
        accept_optional_effect(&mut state);

        assert_eq!(state.objects[&source].paired_with, Some(entrant));
        assert_eq!(state.objects[&entrant].paired_with, Some(source));
    }

    #[test]
    fn soulbond_other_enters_rechecks_triggering_creature_legality() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let source = make_soulbond_creature(&mut state, PlayerId(0), "Soulbond Source");
        let entrant = make_creature(&mut state, PlayerId(0), "New Partner", 1, 1);

        process_triggers(
            &mut state,
            &[zone_changed_event(
                entrant,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        assert_eq!(state.stack.len(), 1);
        state.objects.get_mut(&entrant).unwrap().controller = PlayerId(1);

        resolve_stack_without_soulbond_prompt(&mut state);

        assert_eq!(state.objects[&source].paired_with, None);
        assert_eq!(state.objects[&entrant].paired_with, None);
    }

    #[test]
    fn soulbond_other_enters_rechecks_triggering_creature_on_battlefield() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let source = make_soulbond_creature(&mut state, PlayerId(0), "Soulbond Source");
        let entrant = make_creature(&mut state, PlayerId(0), "New Partner", 1, 1);

        process_triggers(
            &mut state,
            &[zone_changed_event(
                entrant,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        assert_eq!(state.stack.len(), 1);
        crate::game::zones::move_to_zone(&mut state, entrant, Zone::Graveyard, &mut Vec::new());

        resolve_stack_without_soulbond_prompt(&mut state);

        assert_eq!(state.objects[&source].paired_with, None);
        assert_eq!(state.objects[&entrant].paired_with, None);
    }

    /// Install the synthesized Evolve trigger (CR 702.100a) onto a battlefield
    /// creature, mirroring `make_soulbond_creature` / `make_undying_creature`.
    fn make_evolve_creature(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = make_creature(state, player, name, power, toughness);
        let triggers =
            crate::database::synthesis::KeywordTriggerInstaller::triggers_for(&Keyword::Evolve);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.keywords.push(Keyword::Evolve);
        obj.base_keywords.push(Keyword::Evolve);
        for trigger in &triggers {
            obj.trigger_definitions.push(trigger.clone());
        }
        std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).extend(triggers);
        id
    }

    /// Count `Plus1Plus1` counters on an object.
    fn plus1_counters(state: &GameState, id: ObjectId) -> u32 {
        state.objects[&id]
            .counters
            .iter()
            .filter(|(ct, _)| **ct == crate::types::counter::CounterType::Plus1Plus1)
            .map(|(_, n)| *n)
            .sum()
    }

    /// CR 702.100a + CR 603.4: a creature with greater power entering the
    /// battlefield triggers Evolve; the intervening-if (resolved against the
    /// detection-time `EventSource` event) passes, the trigger goes on the
    /// stack, resolves, and places exactly one +1/+1 counter on the Evolve
    /// creature. Drives the real ETB detection path so the detection-time
    /// `EventSource` P/T resolution (quantity.rs section D) is exercised.
    #[test]
    fn test_evolve_larger_creature_grants_counter() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let evolver = make_evolve_creature(&mut state, PlayerId(0), "Evolve 2/2", 2, 2);
        let entrant = make_creature(&mut state, PlayerId(0), "Bigger 3/3", 3, 3);

        process_triggers(
            &mut state,
            &[zone_changed_event(
                entrant,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        assert_eq!(state.stack.len(), 1, "Evolve trigger must go on the stack");

        resolve_stack_without_soulbond_prompt(&mut state);
        assert_eq!(
            plus1_counters(&state, evolver),
            1,
            "Evolve places exactly one +1/+1 counter (CR 702.100a)"
        );
    }

    /// CR 702.100b: A creature "evolves" only after its evolve ability
    /// resolves and actually puts one or more +1/+1 counters on it. This is
    /// distinct from the CR 702.100a ETB trigger event that starts the evolve
    /// ability.
    #[test]
    fn test_evolved_trigger_fires_after_evolve_counter_is_added() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let evolver = make_evolve_creature(&mut state, PlayerId(0), "Evolve 2/2", 2, 2);
        let entrant = make_creature(&mut state, PlayerId(0), "Bigger 3/3", 3, 3);

        let draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let evolved_trigger = TriggerDefinition::new(TriggerMode::Evolved)
            .valid_card(TargetFilter::SelfRef)
            .execute(draw)
            .description("Whenever this creature evolves, draw a card.".to_string());
        state
            .objects
            .get_mut(&evolver)
            .unwrap()
            .trigger_definitions
            .push(evolved_trigger.clone());
        std::sync::Arc::make_mut(
            &mut state
                .objects
                .get_mut(&evolver)
                .unwrap()
                .base_trigger_definitions,
        )
        .push(evolved_trigger);

        process_triggers(
            &mut state,
            &[zone_changed_event(
                entrant,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        assert_eq!(state.stack.len(), 1, "only the evolve ETB trigger fires");

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::Evolved { object_id } if *object_id == evolver
            )),
            "evolve resolution must emit the CR 702.100b evolved event"
        );

        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            1,
            "the separate 'whenever this evolves' trigger must now fire"
        );
    }

    /// CR 702.100a: a creature whose power AND toughness are both not greater
    /// (smaller, or equal) does NOT trigger Evolve — the intervening-if uses
    /// strict greater-than, not greater-or-equal.
    #[test]
    fn test_evolve_smaller_or_equal_creature_no_counter() {
        // Smaller entrant — 1/1 vs the 2/2 Evolve creature.
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let evolver = make_evolve_creature(&mut state, PlayerId(0), "Evolve 2/2", 2, 2);
        let smaller = make_creature(&mut state, PlayerId(0), "Smaller 1/1", 1, 1);
        process_triggers(
            &mut state,
            &[zone_changed_event(
                smaller,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        assert_eq!(
            state.stack.len(),
            0,
            "smaller creature must not trigger Evolve"
        );
        assert_eq!(plus1_counters(&state, evolver), 0);

        // Equal entrant — 2/2 vs the 2/2 Evolve creature: CR 702.100a requires
        // *greater*, so equal P/T does not trigger.
        let equal = make_creature(&mut state, PlayerId(0), "Equal 2/2", 2, 2);
        process_triggers(
            &mut state,
            &[zone_changed_event(
                equal,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        assert_eq!(
            state.stack.len(),
            0,
            "equal-P/T creature must not trigger Evolve (greater, not greater-or-equal)"
        );
        assert_eq!(plus1_counters(&state, evolver), 0);
    }

    /// CR 702.100a + CR 604.1: Evolve granted to a creature at runtime (via the
    /// `trigger_matches_keyword_kind` install path) behaves identically to
    /// printed Evolve. A vanilla 2/2 granted Evolve gains a +1/+1 counter when
    /// a 3/3 enters.
    #[test]
    fn test_granted_evolve_triggers() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Vanilla 2/2 — Evolve is granted at runtime, not printed.
        let granted = make_creature(&mut state, PlayerId(0), "Granted Evolve 2/2", 2, 2);
        let triggers =
            crate::database::synthesis::KeywordTriggerInstaller::triggers_for(&Keyword::Evolve);
        // Confirm the granted-keyword install path recognizes the synthesized
        // trigger (step A.2 — trigger_matches_keyword_kind).
        for trigger in &triggers {
            assert!(
                crate::database::synthesis::KeywordTriggerInstaller::trigger_matches_keyword_kind(
                    trigger,
                    &Keyword::Evolve,
                ),
                "granted-Evolve install path must recognize the synthesized trigger"
            );
        }
        {
            let obj = state.objects.get_mut(&granted).unwrap();
            obj.keywords.push(Keyword::Evolve);
            for trigger in &triggers {
                obj.trigger_definitions.push(trigger.clone());
            }
        }

        let entrant = make_creature(&mut state, PlayerId(0), "Bigger 3/3", 3, 3);
        process_triggers(
            &mut state,
            &[zone_changed_event(
                entrant,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        assert_eq!(state.stack.len(), 1, "granted Evolve must go on the stack");
        resolve_stack_without_soulbond_prompt(&mut state);
        assert_eq!(
            plus1_counters(&state, granted),
            1,
            "granted Evolve places one +1/+1 counter"
        );
    }

    #[test]
    fn soulbond_paired_static_applies_to_both_and_ends_when_pair_breaks() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let source = make_soulbond_creature(&mut state, PlayerId(0), "Wolfir Test");
        add_wolfir_static(&mut state, source);
        let partner = make_creature(&mut state, PlayerId(0), "Partner", 1, 1);

        process_triggers(
            &mut state,
            &[zone_changed_event(
                source,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        resolve_stack_to_optional_choice(&mut state);
        accept_optional_effect(&mut state);
        choose_soulbond_partner(&mut state, partner);
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(state.objects[&source].power, Some(6));
        assert_eq!(state.objects[&source].toughness, Some(6));
        assert_eq!(state.objects[&partner].power, Some(5));
        assert_eq!(state.objects[&partner].toughness, Some(5));

        crate::game::pairing::break_pair(&mut state, source);
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(state.objects[&source].power, Some(2));
        assert_eq!(state.objects[&source].toughness, Some(2));
        assert_eq!(state.objects[&partner].power, Some(1));
        assert_eq!(state.objects[&partner].toughness, Some(1));
    }

    #[test]
    fn soulbond_pair_breaks_on_leave_control_change_and_stops_being_creature() {
        let mut state = setup();
        let a = make_creature(&mut state, PlayerId(0), "A", 2, 2);
        let b = make_creature(&mut state, PlayerId(0), "B", 2, 2);
        crate::game::pairing::pair_objects(&mut state, a, b, PlayerId(0));

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, a, Zone::Graveyard, &mut events);
        assert_eq!(state.objects[&b].paired_with, None);

        let c = make_creature(&mut state, PlayerId(0), "C", 2, 2);
        let d = make_creature(&mut state, PlayerId(0), "D", 2, 2);
        crate::game::pairing::pair_objects(&mut state, c, d, PlayerId(0));
        state.add_transient_continuous_effect(
            ObjectId(9000),
            PlayerId(1),
            Duration::Permanent,
            TargetFilter::SpecificObject { id: d },
            vec![ContinuousModification::ChangeController],
            None,
        );
        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(state.objects[&c].paired_with, None);
        assert_eq!(state.objects[&d].paired_with, None);

        let e = make_creature(&mut state, PlayerId(0), "E", 2, 2);
        let f = make_creature(&mut state, PlayerId(0), "F", 2, 2);
        crate::game::pairing::pair_objects(&mut state, e, f, PlayerId(0));
        state
            .objects
            .get_mut(&f)
            .unwrap()
            .card_types
            .core_types
            .retain(|ty| *ty != CoreType::Creature);
        crate::game::pairing::cleanup_invalid_pairs(&mut state);
        assert_eq!(state.objects[&e].paired_with, None);
        assert_eq!(state.objects[&f].paired_with, None);

        // CR 702.95e: a single effect gains control of BOTH halves of the pair.
        // The two creatures still share a controller, so the old
        // `obj.controller == partner.controller` check kept the pair alive; per
        // the rules the pair must break because another player gained control.
        let g = make_creature(&mut state, PlayerId(0), "G", 2, 2);
        let h = make_creature(&mut state, PlayerId(0), "H", 2, 2);
        crate::game::pairing::pair_objects(&mut state, g, h, PlayerId(0));
        state.objects.get_mut(&g).unwrap().controller = PlayerId(1);
        state.objects.get_mut(&h).unwrap().controller = PlayerId(1);
        crate::game::pairing::cleanup_invalid_pairs(&mut state);
        assert_eq!(
            state.objects[&g].paired_with, None,
            "both halves stolen by one player must unpair (CR 702.95e)"
        );
        assert_eq!(state.objects[&h].paired_with, None);

        let low = make_creature(&mut state, PlayerId(0), "Low", 2, 2);
        let high = make_creature(&mut state, PlayerId(0), "High", 2, 2);
        assert!(high.0 > low.0);
        state.objects.get_mut(&high).unwrap().paired_with = Some(low);
        crate::game::pairing::cleanup_invalid_pairs(&mut state);
        assert_eq!(state.objects[&high].paired_with, None);
        assert_eq!(state.objects[&low].paired_with, None);
    }

    #[test]
    fn soulbond_partner_choice_happens_at_resolution() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let source = make_soulbond_creature(&mut state, PlayerId(0), "Soulbond Source");
        let chosen = make_creature(&mut state, PlayerId(0), "Chosen Partner", 1, 1);
        let other = make_creature(&mut state, PlayerId(0), "Other Partner", 1, 1);

        process_triggers(
            &mut state,
            &[zone_changed_event(
                source,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );
        state
            .objects
            .get_mut(&chosen)
            .unwrap()
            .card_types
            .core_types
            .retain(|ty| *ty != CoreType::Creature);
        resolve_stack_to_optional_choice(&mut state);
        accept_optional_effect(&mut state);
        match &state.waiting_for {
            WaitingFor::PairChoice { choices, .. } => {
                assert!(!choices.contains(&chosen));
                assert!(choices.contains(&other));
            }
            other_waiting => panic!("expected PairChoice, got {other_waiting:?}"),
        }
        choose_soulbond_partner(&mut state, other);

        assert_eq!(state.objects[&source].paired_with, Some(other));
        assert_eq!(state.objects[&chosen].paired_with, None);
        assert_eq!(state.objects[&other].paired_with, Some(source));
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
    fn exploit_trigger_receives_typed_may_trigger_origin() {
        let mut state = setup();
        let player = PlayerId(0);
        let exploiter = create_object(
            &mut state,
            CardId(1),
            player,
            "Exploit Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&exploiter).unwrap();
            obj.keywords.push(Keyword::Exploit);
            obj.card_types.core_types.push(CoreType::Creature);
        }

        process_triggers(
            &mut state,
            &[zone_changed_event(
                exploiter,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec![],
            )],
        );

        let Some(StackEntryKind::TriggeredAbility { ability, .. }) =
            state.stack.back().map(|entry| &entry.kind)
        else {
            panic!("expected exploit trigger on stack");
        };
        assert_eq!(
            ability.may_trigger_origin,
            Some(MayTriggerOrigin::Keyword {
                keyword: KeywordKind::Exploit,
            })
        );
    }

    #[test]
    fn firebending_trigger_resolves_dynamic_amount_at_mana_resolution() {
        let mut state = setup();
        let player = PlayerId(0);
        let firebender = create_object(
            &mut state,
            CardId(1),
            player,
            "Firebender".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&firebender).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.keywords.push(Keyword::Firebending(QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::Power {
                    scope: crate::types::ability::ObjectScope::Source,
                },
            }));
        }

        process_triggers(
            &mut state,
            &[GameEvent::AttackersDeclared {
                attacker_ids: vec![firebender],
                defending_player: PlayerId(1),
                attacks: vec![(
                    firebender,
                    crate::game::combat::AttackTarget::Player(PlayerId(1)),
                )],
            }],
        );

        let Some(StackEntryKind::TriggeredAbility { ability, .. }) =
            state.stack.back().map(|entry| &entry.kind)
        else {
            panic!("expected firebending trigger on stack");
        };
        let ability = ability.clone();
        assert_eq!(
            ability.may_trigger_origin,
            Some(MayTriggerOrigin::Keyword {
                keyword: KeywordKind::Firebending,
            })
        );
        state.objects.get_mut(&firebender).unwrap().power = Some(5);
        let mut events = Vec::new();

        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 5);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::Firebend {
                source_id,
                controller: PlayerId(0)
            } if *source_id == firebender
        )));
    }

    #[test]
    fn becomes_plotted_trigger_fires_from_exile() {
        let mut state = setup();
        let player = PlayerId(0);
        let plotted = create_object(
            &mut state,
            CardId(1),
            player,
            "Aloe Alchemist".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&plotted).unwrap();
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::BecomesPlotted)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ))
                    .trigger_zones(vec![Zone::Exile]),
            );
        }

        process_triggers(
            &mut state,
            &[GameEvent::BecomesPlotted {
                object_id: plotted,
                player_id: player,
            }],
        );

        assert!(state.stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { ability, .. }
                if matches!(&ability.effect, Effect::Draw { .. })
        )));
    }

    #[test]
    fn ravenous_draw_triggers_when_paid_x_is_five_or_more() {
        let mut state = setup();
        let player = PlayerId(0);
        let ravener = create_object(
            &mut state,
            CardId(1),
            player,
            "Ravener".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&ravener).unwrap();
            obj.keywords.push(Keyword::Ravenous);
            obj.cost_x_paid = Some(5);
        }

        process_triggers(
            &mut state,
            &[zone_changed_event(
                ravener,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                vec!["Tyranid"],
            )],
        );

        assert!(state.stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { ability, .. }
                if matches!(ability.effect, Effect::Draw { .. })
        )));
    }

    #[test]
    fn command_emblem_cast_with_storm_creates_copies_for_prior_spells() {
        let mut state = setup();
        let player = PlayerId(0);
        let opponent = PlayerId(1);
        let emblem = create_object(
            &mut state,
            CardId(1),
            player,
            "Ral Emblem".to_string(),
            Zone::Command,
        );
        {
            let obj = state.objects.get_mut(&emblem).unwrap();
            obj.is_emblem = true;
            obj.static_definitions = vec![StaticDefinition::new(StaticMode::CastWithKeyword {
                keyword: Keyword::Storm,
            })
            .affected(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::AnyOf(vec![
                    TypeFilter::Instant,
                    TypeFilter::Sorcery,
                ]))
                .controller(ControllerRef::You),
            ))]
            .into();
        }

        let spell = create_object(
            &mut state,
            CardId(2),
            player,
            "Ral Storm Spell".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
        }
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: player,
            kind: StackEntryKind::Spell {
                card_id: CardId(2),
                ability: Some(ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                    Vec::new(),
                    spell,
                    player,
                )),
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 1,
            },
        });

        let prior_record = SpellCastRecord {
            name: String::new(),
            core_types: vec![CoreType::Sorcery],
            supertypes: Vec::new(),
            subtypes: Vec::new(),
            keywords: Vec::new(),
            colors: Vec::new(),
            mana_value: 1,
            has_x_in_cost: false,
            from_zone: Zone::Hand,
            cast_variant: crate::types::game_state::CastingVariant::Normal,
        };
        let current_record = SpellCastRecord {
            name: String::new(),
            core_types: vec![CoreType::Instant],
            supertypes: Vec::new(),
            subtypes: Vec::new(),
            keywords: Vec::new(),
            colors: Vec::new(),
            mana_value: 1,
            has_x_in_cost: false,
            from_zone: Zone::Hand,
            cast_variant: crate::types::game_state::CastingVariant::Normal,
        };
        state.spells_cast_this_turn_by_player.insert(
            player,
            crate::im::Vector::from(vec![prior_record.clone(), current_record]),
        );
        state
            .spells_cast_this_turn_by_player
            .insert(opponent, crate::im::Vector::from(vec![prior_record]));

        process_triggers(
            &mut state,
            &[GameEvent::SpellCast {
                card_id: CardId(2),
                controller: player,
                object_id: spell,
            }],
        );

        assert!(state.stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { ability, .. }
                if matches!(ability.effect, Effect::CopySpell { .. })
                    && matches!(ability.repeat_for, Some(QuantityExpr::Fixed { value: 2 }))
        )));
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

        // AP (P0) triggers are placed first, so they are lowest on the stack;
        // NAP (P1) triggers are placed after them and resolve first.
        let top = &state.stack[state.stack.len() - 1];
        let bottom = &state.stack[0];
        assert_eq!(top.controller, PlayerId(1), "NAP trigger should be on top");
        assert_eq!(
            bottom.controller,
            PlayerId(0),
            "AP trigger should be on bottom"
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
    /// `QuantityRef::Power { scope: ObjectScope::CostPaidObject }`. At
    /// resolution the event's source is the entering creature; the resolver's
    /// slot-2 (trigger-event source) fallback must read THAT creature's
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
                            counter_type: crate::types::counter::CounterType::Plus1Plus1,
                            count: QuantityExpr::Ref {
                                qty: QuantityRef::Power {
                                    scope: crate::types::ability::ObjectScope::CostPaidObject,
                                },
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
            "Power {{ CostPaidObject }} must resolve via the trigger-event-source \
             fallback to the entering creature's power (4), \
             yielding 4 +1/+1 counters on the source (got {p1p1})"
        );
    }

    #[test]
    fn delayed_enter_trigger_filters_tracked_set_and_targets_triggering_object() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lagrella".to_string(),
            Zone::Battlefield,
        );
        let tracked = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Tracked Creature".to_string(),
            Zone::Battlefield,
        );
        let other = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Other Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![tracked]);
        state.delayed_triggers.push(DelayedTrigger {
            condition: DelayedTriggerCondition::WhenEntersBattlefield {
                filter: TargetFilter::TrackedSet {
                    id: TrackedSetId(1),
                },
            },
            ability: ResolvedAbility::new(
                Effect::PutCounter {
                    counter_type: crate::types::counter::CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::TriggeringSource,
                },
                vec![],
                source,
                PlayerId(0),
            ),
            controller: PlayerId(0),
            source_id: source,
            one_shot: true,
        });

        let other_event = zone_changed_event(
            other,
            Zone::Exile,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        );
        assert!(
            check_delayed_triggers(&mut state, &[other_event]).is_empty(),
            "untracked entering objects must not fire tracked-set delayed triggers"
        );
        assert_eq!(state.stack.len(), 0);

        let tracked_event = zone_changed_event(
            tracked,
            Zone::Exile,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        );
        let queued = check_delayed_triggers(&mut state, &[tracked_event]);
        assert_eq!(queued.len(), 1);
        assert_eq!(state.stack.len(), 1);

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);
        let p1p1 = state.objects[&tracked]
            .counters
            .get(&crate::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            p1p1, 2,
            "delayed trigger body must put counters on the object that entered"
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
        // CR 603.3b: AP triggers are placed first, so they are lower on the
        // stack; NAP triggers are placed after them and resolve first.
        assert_eq!(state.stack[state.stack.len() - 1].controller, PlayerId(1));
        assert_eq!(state.stack[0].controller, PlayerId(0));
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
    fn zone_change_object_condition_checks_entering_object_not_trigger_source() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Observer".to_string(),
            Zone::Battlefield,
        );
        let entering = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Countered Entry".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&entering)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&entering)
            .unwrap()
            .counters
            .insert(crate::types::counter::CounterType::Plus1Plus1, 1);

        let condition = TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin: None,
            destination: Zone::Battlefield,
            filter: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::Counters {
                    counters: crate::types::counter::CounterMatch::Any,
                    comparator: crate::types::ability::Comparator::GE,
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                },
            ])),
        };
        let event = zone_changed_event(
            entering,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        );

        assert!(check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(source),
            Some(&event),
        ));
    }

    #[test]
    fn zone_change_object_condition_checks_entering_token_identity() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Observer".to_string(),
            Zone::Battlefield,
        );
        let entering = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Gruff Triplets".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&entering).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.is_token = false;
        }

        let condition = TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin: None,
            destination: Zone::Battlefield,
            filter: TargetFilter::Typed(
                TypedFilter::permanent().properties(vec![FilterProp::NonToken]),
            ),
        };
        let event = zone_changed_event(
            entering,
            Zone::Stack,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        );

        assert!(check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(source),
            Some(&event),
        ));

        state.objects.get_mut(&entering).unwrap().is_token = true;
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(source),
            Some(&event),
        ));
    }

    #[test]
    fn zone_change_object_condition_checks_dead_object_snapshot() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(32),
            PlayerId(0),
            "Observer".to_string(),
            Zone::Battlefield,
        );
        let dead = create_object(
            &mut state,
            CardId(33),
            PlayerId(0),
            "Countered Dead".to_string(),
            Zone::Graveyard,
        );
        let mut counters = std::collections::HashMap::new();
        counters.insert(crate::types::counter::CounterType::Plus1Plus1, 1);
        state.lki_cache.insert(
            dead,
            crate::types::game_state::LKISnapshot {
                name: "Countered Dead".to_string(),
                power: Some(2),
                toughness: Some(2),
                base_power: Some(2),
                base_toughness: Some(2),
                mana_value: 2,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters,
            },
        );

        let condition = TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin: Some(Zone::Battlefield),
            destination: Zone::Graveyard,
            filter: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::Counters {
                    counters: crate::types::counter::CounterMatch::Any,
                    comparator: crate::types::ability::Comparator::GE,
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                },
            ])),
        };
        let event = zone_changed_event(
            dead,
            Zone::Battlefield,
            Zone::Graveyard,
            vec![CoreType::Creature],
            Vec::new(),
        );

        assert!(check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(source),
            Some(&event),
        ));
    }

    #[test]
    fn first_combat_phase_condition_checks_turn_counter() {
        let mut state = setup();
        let condition = TriggerCondition::FirstCombatPhaseOfTurn;

        state.combat_phases_started_this_turn = 0;
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            None,
            None,
        ));

        state.combat_phases_started_this_turn = 1;
        assert!(check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            None,
            None,
        ));

        state.combat_phases_started_this_turn = 2;
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            None,
            None,
        ));
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

    /// CR 603.2b + CR 102.1: For Phase triggers like "At the beginning of each
    /// player's draw step, that player draws an additional card" (Dictate of
    /// Kruphix, Kami of the Crescent Moon), the resolved ability must carry
    /// `scoped_player = active_player` so `TargetFilter::ScopedPlayer` resolves
    /// to the player whose phase is beginning — NOT to the source's controller.
    /// This is the engine half of the fix; parser emits `ScopedPlayer` and the
    /// runtime binds it at fire time.
    #[test]
    fn build_triggered_ability_phase_binds_scoped_player_to_active_player() {
        let mut state = setup();
        // Source controlled by P0, but it's P1's turn — the trigger must draw
        // for P1 (active player), not P0.
        state.active_player = PlayerId(1);

        let trig_def = TriggerDefinition::new(TriggerMode::Phase)
            .phase(crate::types::phase::Phase::Draw)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::ScopedPlayer,
                },
            ));

        let ability = build_triggered_ability(&state, &trig_def, ObjectId(1), PlayerId(0));
        assert_eq!(ability.controller, PlayerId(0));
        assert_eq!(
            ability.scoped_player,
            Some(PlayerId(1)),
            "Phase trigger must bind scoped_player to the active player so 'that player draws' resolves correctly on opponent's turn"
        );
    }

    /// Issue #1304 — RUNTIME: Keeper of the Accord's intervening-if must compare
    /// the active player's creatures to the source controller's at opponent end
    /// step, not fail closed because the condition was never hoisted/parsed.
    #[test]
    fn keeper_of_the_accord_creature_intervening_if_true_when_opponent_ahead() {
        let def = crate::parser::oracle_trigger::parse_trigger_line(
            "At the beginning of each opponent's end step, if that player controls more creatures than you, create a 1/1 white Soldier creature token.",
            "Keeper of the Accord",
        );
        let condition = def
            .condition
            .expect("creature trigger must hoist intervening-if to def.condition");

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let opponent = PlayerId(1);
        state.active_player = opponent;

        let keeper = make_creature(&mut state, controller, "Keeper of the Accord", 3, 4);
        let _opp_creature_a = make_creature(&mut state, opponent, "Opp A", 1, 1);
        let _opp_creature_b = make_creature(&mut state, opponent, "Opp B", 1, 1);
        let _opp_creature_c = make_creature(&mut state, opponent, "Opp C", 1, 1);

        let phase_event = GameEvent::PhaseChanged {
            phase: crate::types::phase::Phase::End,
        };
        assert!(
            check_trigger_condition(
                &state,
                &condition,
                controller,
                Some(keeper),
                Some(&phase_event),
            ),
            "opponent with three creatures vs controller with one (keeper) must satisfy intervening-if",
        );
    }

    #[test]
    fn keeper_of_the_accord_creature_intervening_if_false_when_tied() {
        let def = crate::parser::oracle_trigger::parse_trigger_line(
            "At the beginning of each opponent's end step, if that player controls more creatures than you, create a 1/1 white Soldier creature token.",
            "Keeper of the Accord",
        );
        let condition = def.condition.unwrap();

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let opponent = PlayerId(1);
        state.active_player = opponent;

        let keeper = make_creature(&mut state, controller, "Keeper of the Accord", 3, 4);
        let _self_b = make_creature(&mut state, controller, "Self B", 1, 1);
        let _opp_a = make_creature(&mut state, opponent, "Opp A", 1, 1);
        let _opp_b = make_creature(&mut state, opponent, "Opp B", 1, 1);

        let phase_event = GameEvent::PhaseChanged {
            phase: crate::types::phase::Phase::End,
        };
        assert!(
            !check_trigger_condition(
                &state,
                &condition,
                controller,
                Some(keeper),
                Some(&phase_event),
            ),
            "two creatures each must not satisfy 'more creatures than you'",
        );
    }

    /// Non-Phase triggers must NOT have scoped_player auto-bound (preserves
    /// the existing convention that ETB/Dies/SpellCast triggers leave
    /// scoped_player None and resolve "that player" via event-context refs
    /// like `TriggeringPlayer`).
    #[test]
    fn build_triggered_ability_non_phase_leaves_scoped_player_none() {
        let mut state = setup();
        state.active_player = PlayerId(1);

        let trig_def =
            TriggerDefinition::new(TriggerMode::ChangesZone).execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));

        let ability = build_triggered_ability(&state, &trig_def, ObjectId(1), PlayerId(0));
        assert!(
            ability.scoped_player.is_none(),
            "Non-Phase triggers must not auto-bind scoped_player; they rely on event-context resolution"
        );
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
                                enters_under: None,
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

        // CR 603.3c + CR 603.3d "Push first, choose second": multiple legal
        // targets -> entry IS on the stack (mid-construction); pending_trigger
        // and pending_trigger_entry are both set; the resolver refuses to fire
        // until target selection completes.
        assert!(
            state.pending_trigger.is_some(),
            "Should have pending trigger"
        );
        assert_eq!(
            state.stack.len(),
            1,
            "trigger entry pushed at pause time per push-first contract",
        );
        assert!(
            state.pending_trigger_entry.is_some(),
            "pending_trigger_entry tracks the in-construction stack entry",
        );
        assert_eq!(
            state.stack.back().map(|e| e.id),
            state.pending_trigger_entry,
            "top of stack is the in-construction entry",
        );
        let pending = state.pending_trigger.as_ref().unwrap();
        assert_eq!(pending.source_id, trigger_creature);
        assert_eq!(pending.controller, PlayerId(0));
    }

    /// CR 601.2d + CR 603.3d: A triggered ability with a divided effect
    /// chooses targets first, then its controller divides the total among those
    /// targets while putting the trigger on the stack.
    #[test]
    fn trigger_distributed_damage_uses_chosen_amounts() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let target1 = make_creature(&mut state, PlayerId(1), "Target 1", 2, 10);
        let target2 = make_creature(&mut state, PlayerId(1), "Target 2", 2, 10);

        let source = make_creature(&mut state, PlayerId(0), "Fury-like Source", 3, 3);
        {
            let mut execute = AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 4 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                },
            );
            execute.multi_target = Some(MultiTargetSpec::unlimited(1));
            execute.distribute = Some(DistributionUnit::Damage);

            let obj = state.objects.get_mut(&source).unwrap();
            obj.entered_battlefield_turn = Some(1);
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
            vec![CoreType::Creature],
            Vec::new(),
        )];
        process_triggers(&mut state, &events);

        let wf = crate::game::engine::begin_pending_trigger_target_selection(&mut state)
            .expect("begin trigger target selection")
            .expect("target selection required");
        state.waiting_for = wf;

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target1), TargetRef::Object(target2)],
            },
        )
        .expect("target selection should succeed");

        match result.waiting_for {
            WaitingFor::DistributeAmong {
                total,
                targets,
                unit: DistributionUnit::Damage,
                ..
            } => {
                assert_eq!(total, 4);
                assert_eq!(targets.len(), 2);
            }
            other => panic!("expected DistributeAmong, got {other:?}"),
        }
        // CR 603.3c + CR 603.3d: After target selection, the trigger entry is
        // on the stack in mid-construction (distribution still pending);
        // pending_trigger_entry tracks it and the resolver refuses to fire it.
        assert!(state.pending_trigger.is_some());
        assert_eq!(state.stack.len(), 1);
        assert!(state.pending_trigger_entry.is_some());
        assert_eq!(
            state.stack.back().map(|e| e.id),
            state.pending_trigger_entry
        );

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::DistributeAmong {
                distribution: vec![
                    (TargetRef::Object(target1), 1),
                    (TargetRef::Object(target2), 3),
                ],
            },
        )
        .expect("distribution should put trigger on stack");

        assert!(state.pending_trigger.is_none());
        assert!(
            state.pending_trigger_entry.is_none(),
            "distribution chosen -> construction complete",
        );
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0].kind {
            StackEntryKind::TriggeredAbility { ability, .. } => {
                assert_eq!(
                    ability.distribution,
                    Some(vec![
                        (TargetRef::Object(target1), 1),
                        (TargetRef::Object(target2), 3),
                    ])
                );
            }
            other => panic!("expected triggered ability on stack, got {other:?}"),
        }

        let mut safety_bound = 10;
        while !state.stack.is_empty() && safety_bound > 0 {
            let actor = state.priority_player;
            crate::game::engine::apply(&mut state, actor, GameAction::PassPriority)
                .expect("pass priority");
            safety_bound -= 1;
        }

        assert_eq!(state.objects[&target1].damage_marked, 1);
        assert_eq!(state.objects[&target2].damage_marked, 3);
    }

    #[test]
    fn granted_etb_destroy_other_same_name_skips_source_when_no_other_exists() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Copied Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let mut execute = AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Destroy {
                    target: TargetFilter::Typed(
                        TypedFilter::creature()
                            .properties(vec![FilterProp::Another, FilterProp::SameName]),
                    ),
                    cant_regenerate: false,
                },
            );
            execute.optional_targeting = true;
            execute.multi_target = Some(MultiTargetSpec::fixed(0, 1));
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
            vec![CoreType::Creature],
            Vec::new(),
        )];

        process_triggers(&mut state, &events);

        let entry = state.stack.back().expect("optional trigger goes on stack");
        let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
            panic!("expected triggered ability, got {:?}", entry.kind);
        };
        assert!(
            ability.targets.is_empty(),
            "no other same-name creature exists; source must not be auto-targeted"
        );
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
                    enters_under: None,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            );
            execute.multi_target = Some(MultiTargetSpec::fixed(0, 3));
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

    #[test]
    fn damage_trigger_dynamic_multi_target_uses_trigger_event_amount() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let froghemoth = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Froghemoth".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&froghemoth)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let card_a = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Damaged player's graveyard card A".to_string(),
            Zone::Graveyard,
        );
        let card_b = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Damaged player's graveyard card B".to_string(),
            Zone::Graveyard,
        );
        let mut execute = AbilityDefinition::new(
            AbilityKind::Database,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    controller: None,
                    properties: vec![
                        FilterProp::Owned {
                            controller: ControllerRef::ScopedPlayer,
                        },
                        FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                    ],
                }),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        );
        execute.multi_target = Some(MultiTargetSpec::up_to(QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        }));
        state
            .objects
            .get_mut(&froghemoth)
            .unwrap()
            .trigger_definitions
            .push(
                TriggerDefinition::new(TriggerMode::DamageDone)
                    .execute(execute)
                    .valid_source(TargetFilter::SelfRef)
                    .valid_target(TargetFilter::Player)
                    .damage_kind(crate::types::ability::DamageKindFilter::CombatOnly),
            );

        process_triggers(
            &mut state,
            &[GameEvent::DamageDealt {
                source_id: froghemoth,
                target: TargetRef::Player(PlayerId(1)),
                amount: 2,
                is_combat: true,
                excess: 0,
            }],
        );

        let waiting = crate::game::engine::begin_pending_trigger_target_selection(&mut state)
            .expect("target selection should build")
            .expect("trigger should require target selection");
        let WaitingFor::TriggerTargetSelection { target_slots, .. } = waiting else {
            panic!("expected trigger target selection, got {waiting:?}");
        };
        assert_eq!(
            target_slots.len(),
            2,
            "combat damage amount 2 should surface two optional target slots"
        );
        for slot in target_slots {
            assert!(slot.optional);
            assert!(slot.legal_targets.contains(&TargetRef::Object(card_a)));
            assert!(slot.legal_targets.contains(&TargetRef::Object(card_b)));
        }
    }

    /// CR 603.3 + CR 115.1b: Nurturing Pixie's ETB uses "up to one target
    /// non-Faerie, nonland permanent you control." Multiple legal optional
    /// targets must produce a trigger target-selection prompt, not suppress
    /// the trigger.
    #[test]
    fn nurturing_pixie_etb_prompts_for_optional_non_faerie_nonland_target() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        for (card_id, name) in [
            (CardId(10), "Llanowar Elves"),
            (CardId(11), "Badgermole Cub"),
        ] {
            let target = create_object(
                &mut state,
                card_id,
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        let pixie = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Nurturing Pixie".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pixie).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Faerie".to_string());
            obj.card_types.subtypes.push("Rogue".to_string());
            obj.entered_battlefield_turn = Some(1);
            let mut execute = AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Bounce {
                    target: TargetFilter::Typed(
                        TypedFilter::permanent()
                            .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                                "Faerie".to_string(),
                            ))))
                            .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
                            .controller(ControllerRef::You),
                    ),
                    destination: None,
                    selection: BounceSelection::Targeted,
                },
            );
            execute.optional_targeting = true;
            execute.multi_target = Some(MultiTargetSpec::fixed(0, 1));
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(execute)
                    .valid_card(TargetFilter::SelfRef)
                    .destination(Zone::Battlefield),
            );
        }

        let events = vec![zone_changed_event(
            pixie,
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            vec!["Faerie", "Rogue"],
        )];
        process_triggers(&mut state, &events);

        assert!(state.pending_trigger.is_some(), "pending_trigger set");
        let pending = state.pending_trigger.as_ref().unwrap();
        let slots = super::super::ability_utils::build_target_slots(&state, &pending.ability)
            .expect("slot build");
        assert_eq!(slots.len(), 1);
        assert!(slots[0].optional);
        assert_eq!(slots[0].legal_targets.len(), 2);
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
                    enters_under: None,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            );
            execute.multi_target = Some(MultiTargetSpec::fixed(0, 3));
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
                                enters_under: None,
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
                            enters_under: None,
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
                            enters_under: None,
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
            // "whenever you commit a crime" → valid_target = Controller (parser sets this)
            let mut trigger =
                make_trigger(TriggerMode::CommitCrime).valid_target(TargetFilter::Controller);
            trigger.trigger_zones = vec![Zone::Graveyard];
            trigger.execute = Some(Box::new(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
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
    fn sneaky_snacker_returns_tapped_from_graveyard_on_third_draw_in_turn() {
        let mut state = setup();
        let snacker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sneaky Snacker".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&snacker).unwrap();
            let mut trigger = make_trigger(TriggerMode::Drawn);
            trigger.trigger_zones = vec![Zone::Graveyard];
            trigger.valid_target = Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ));
            trigger.constraint = Some(TriggerConstraint::NthDrawThisTurn { n: 3 });
            trigger.execute = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: true,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            )));
            obj.trigger_definitions.push(trigger);
        }

        for i in 0..4 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                format!("Drawn Card {i}"),
                Zone::Library,
            );
        }

        let draw_one = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(0),
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::draw::resolve(&mut state, &draw_one, &mut events).unwrap();
        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 0);

        events.clear();
        crate::game::effects::draw::resolve(&mut state, &draw_one, &mut events).unwrap();
        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 0);

        let draw_two = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(0),
            PlayerId(0),
        );
        events.clear();
        crate::game::effects::draw::resolve(&mut state, &draw_two, &mut events).unwrap();
        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 1);

        events.clear();
        crate::game::stack::resolve_top(&mut state, &mut events);
        let snacker_obj = state.objects.get(&snacker).unwrap();
        assert_eq!(snacker_obj.zone, Zone::Battlefield);
        assert!(snacker_obj.tapped);
        assert!(state.players[0].graveyard.iter().all(|id| *id != snacker));
        assert!(state.battlefield.contains(&snacker));
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
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: crate::types::ability::CountScope::Controller,
                        filter: None,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            });
            trigger.execute = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
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
            crate::im::Vector::from(vec![
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Instant],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![ManaColor::Blue],
                    mana_value: 1,
                    has_x_in_cost: false,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Creature],
                    supertypes: vec![],
                    subtypes: vec!["Bird".to_string()],
                    keywords: vec![Keyword::Flying],
                    colors: vec![ManaColor::Blue],
                    mana_value: 3,
                    has_x_in_cost: false,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
            ]),
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
                                random: false,
                                choice_optional: false,
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
                                    enters_under: None,
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
            target: TargetRef::Object(creature),
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
                // Post-fold: the unless-pay modifier lives on
                // `ResolvedAbility.unless_pay`, not on `Effect::Counter`.
                assert!(matches!(ability.effect, Effect::Counter { .. }));
                assert!(
                    ability.unless_pay.is_some(),
                    "ward should attach an unless_pay modifier"
                );
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
            target: TargetRef::Object(creature),
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
            target: TargetRef::Object(creature),
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
            target: TargetRef::Object(creature),
            source_id: spell,
        }];

        process_triggers(&mut state, &events);
        // CR 603.3b (#531): drain the per-controller ordering prompt.
        super::drain_order_triggers_with_identity(&mut state);

        // Two ward triggers + original spell = 3
        assert_eq!(
            state.stack.len(),
            3,
            "Two ward triggers should fire independently"
        );
    }

    #[test]
    fn ward_cost_to_ability_cost_all_variants() {
        use crate::types::keywords::WardCost;
        use crate::types::mana::ManaCost;

        // Mana cost
        let mana = WardCost::Mana(ManaCost::generic(3));
        let result = ward_cost_to_ability_cost(&mana);
        assert!(matches!(result, AbilityCost::Mana { cost } if cost == ManaCost::generic(3)));

        // Pay life
        let life = WardCost::PayLife(2);
        let result = ward_cost_to_ability_cost(&life);
        assert!(matches!(
            result,
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 }
            }
        ));

        // Discard
        let discard = WardCost::DiscardCard;
        let result = ward_cost_to_ability_cost(&discard);
        assert!(matches!(
            result,
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                random: false,
                self_ref: false,
            }
        ));

        // Sacrifice
        let sacrifice = WardCost::Sacrifice {
            count: 1,
            filter: TargetFilter::Any,
        };
        let result = ward_cost_to_ability_cost(&sacrifice);
        assert!(matches!(result, AbilityCost::Sacrifice { count: 1, .. }));

        // Waterbend
        let waterbend = WardCost::Waterbend(ManaCost::generic(4));
        let result = ward_cost_to_ability_cost(&waterbend);
        assert!(matches!(result, AbilityCost::Mana { cost } if cost == ManaCost::generic(4)));
    }

    #[test]
    fn nth_draw_constraint_uses_draw_event_ordinal_not_final_turn_total() {
        let mut state = setup();
        state.players[1].cards_drawn_this_turn = 4;

        let mut trig_def = make_trigger(TriggerMode::Drawn);
        trig_def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n: 2 });

        let controller = PlayerId(0);
        let event = GameEvent::CardDrawn {
            player_id: PlayerId(1),
            object_id: ObjectId(99),
            nth_in_turn: 2,
            nth_in_step: 1,
        };

        // Should fire: this event is the opponent's 2nd draw, even though the
        // batch has already advanced their final turn count to 4.
        assert!(check_trigger_constraint(
            &state,
            &trig_def,
            ObjectId(1),
            0,
            controller,
            &event,
        ));

        // Should NOT fire: this event is a first draw.
        let controller_draw = GameEvent::CardDrawn {
            player_id: PlayerId(0),
            object_id: ObjectId(100),
            nth_in_turn: 1,
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
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: source,
            source_controller: PlayerId(0),
            target: TargetRef::Object(dying_creature),
            target_controller: PlayerId(0),
            amount: 3,
            is_combat: false,
            ..Default::default()
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
    fn test_no_monarch_trigger_condition() {
        // CR 725.1: NoMonarch is true only when no player holds the monarch.
        let mut state = setup();
        let source = ObjectId(10);
        let condition = TriggerCondition::NoMonarch;

        // No monarch → condition true.
        state.monarch = None;
        assert!(check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(source),
            None,
        ));

        // Controller is monarch → false (distinct from Not(IsMonarch)).
        state.monarch = Some(PlayerId(0));
        assert!(!check_trigger_condition(
            &state,
            &condition,
            PlayerId(0),
            Some(source),
            None,
        ));

        // An opponent is monarch → still false: a monarch exists.
        state.monarch = Some(PlayerId(1));
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
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: ObjectId(1),
            source_controller: PlayerId(0),
            target: TargetRef::Object(ObjectId(2)),
            target_controller: PlayerId(0),
            amount: 2,
            is_combat: true,
            ..Default::default()
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

    // === CR 603.6a + CR 110.5b: "When ~ enters untapped/tapped" ETB gating ===
    //
    // Gingerbread Cabin class ("When this land enters untapped, create a Food
    // token.") relies on `Not { Box::new(ZoneChangeObjectIsTapped) }` evaluating
    // the post-replacement-pipeline tapped state of the entering permanent at
    // trigger-check time. The parser already attaches the condition; these
    // tests guard the runtime evaluator so an ETB tapped via the "enters tapped
    // unless ..." replacement suppresses the Food trigger, and an ETB untapped
    // fires it.
    //
    // NOTE: These two `source_enters_untapped_*` tests call
    // `check_trigger_condition` with `trigger_event = None`, so the
    // `ZoneChangeObjectIsTapped` evaluator resolves *only* via its
    // `.or(source_id)` fallback path — the SelfRef case where the entering
    // permanent IS the source. They exercise the fallback, not the
    // event-object resolution path. The event-object path is covered by the
    // real-pipeline observer tests in `effects/change_zone.rs`
    // (`amulet_of_vigor_*`, `charismatic_conqueror_*`).

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

        // SelfRef tapland: parser emits `Not(ZoneChangeObjectIsTapped)`. With
        // `trigger_event = None` this resolves via the `.or(source_id)`
        // fallback — exercising only the SelfRef (source == entering) path.
        let cond = TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped),
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

        // SelfRef tapland: parser emits `Not(ZoneChangeObjectIsTapped)`. With
        // `trigger_event = None` this resolves via the `.or(source_id)`
        // fallback — exercising only the SelfRef (source == entering) path.
        let cond = TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped),
        };
        assert!(!check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));
    }

    // CR 603.6a + CR 110.5b: `ZoneChangeObjectIsTapped` resolves the *entering*
    // permanent from the triggering `ZoneChanged` event, NOT the ability
    // source. This test drives a real `ZoneChanged` event with the entering
    // object distinct from `source_id` to pin the event-object resolution
    // path (the `.or(source_id)` fallback is exercised separately by the
    // SelfRef-tapland tests above).
    #[test]
    fn zone_change_object_is_tapped_reads_entering_object_from_event() {
        let mut state = setup();
        // Ability source — deliberately untapped, so a `source_id`-based read
        // would yield `false`.
        let ability_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Amulet of Vigor".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&ability_source).unwrap().tapped = false;
        // The entering permanent — a *different* object, tapped.
        let entering = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Lotus Field".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&entering).unwrap().tapped = true;

        let event = GameEvent::ZoneChanged {
            object_id: entering,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                entering,
                Some(Zone::Hand),
                Zone::Battlefield,
            )),
        };
        let cond = TriggerCondition::ZoneChangeObjectIsTapped;
        // True: the *entering* object is tapped, even though `source_id` is not.
        assert!(check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(ability_source),
            Some(&event),
        ));
        // The untapped-entering case must NOT satisfy the condition.
        state.objects.get_mut(&entering).unwrap().tapped = false;
        assert!(!check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(ability_source),
            Some(&event),
        ));
    }

    #[test]
    fn source_matches_filter_checks_trigger_source_properties() {
        let mut state = setup();
        let src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dreampod Druid".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&src)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Test Aura".to_string(),
            Zone::Battlefield,
        );
        {
            let aura_obj = state.objects.get_mut(&aura).unwrap();
            aura_obj.card_types.core_types.push(CoreType::Enchantment);
            aura_obj.card_types.subtypes.push("Aura".to_string());
            aura_obj.attached_to = Some(crate::game::game_object::AttachTarget::Object(src));
        }
        state.objects.get_mut(&src).unwrap().attachments.push(aura);

        let cond = TriggerCondition::SourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::HasAttachment {
                    kind: crate::types::ability::AttachmentKind::Aura,
                    controller: None,
                },
            ])),
        };

        assert!(check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));
    }

    // === CR 701.27g + CR 708.2: Source-state predicates (Transformed/FaceUp/FaceDown) ===

    #[test]
    fn source_is_transformed_fires_when_object_transformed() {
        let mut state = setup();
        let src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test DFC".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&src).unwrap().transformed = true;

        let cond = TriggerCondition::SourceIsTransformed;
        assert!(check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));

        let cond_neg = TriggerCondition::Not {
            condition: Box::new(TriggerCondition::SourceIsTransformed),
        };
        assert!(!check_trigger_condition(
            &state,
            &cond_neg,
            PlayerId(0),
            Some(src),
            None,
        ));
    }

    #[test]
    fn source_is_transformed_suppressed_when_object_front_face() {
        let mut state = setup();
        let src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test DFC".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&src).unwrap().transformed = false;

        let cond = TriggerCondition::SourceIsTransformed;
        assert!(!check_trigger_condition(
            &state,
            &cond,
            PlayerId(0),
            Some(src),
            None,
        ));
    }

    #[test]
    fn source_is_face_up_inverse_of_face_down() {
        let mut state = setup();
        let src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Morph Test".to_string(),
            Zone::Battlefield,
        );
        // Face-up (default): SourceIsFaceUp fires, SourceIsFaceDown does not.
        state.objects.get_mut(&src).unwrap().face_down = false;
        assert!(check_trigger_condition(
            &state,
            &TriggerCondition::SourceIsFaceUp,
            PlayerId(0),
            Some(src),
            None,
        ));
        assert!(!check_trigger_condition(
            &state,
            &TriggerCondition::SourceIsFaceDown,
            PlayerId(0),
            Some(src),
            None,
        ));

        // Flip to face-down: predicates invert.
        state.objects.get_mut(&src).unwrap().face_down = true;
        assert!(!check_trigger_condition(
            &state,
            &TriggerCondition::SourceIsFaceUp,
            PlayerId(0),
            Some(src),
            None,
        ));
        assert!(check_trigger_condition(
            &state,
            &TriggerCondition::SourceIsFaceDown,
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
                            counter_type: crate::types::counter::CounterType::Plus1Plus1,
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
            enters_under: None,
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
            enters_under: None,
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

    /// CR 701.21a: Sacrifice does not target — the sacrifice effect handler
    /// uses EffectZoneChoice for controller-scoped selection at resolution time.
    #[test]
    fn extract_target_skips_sacrifice() {
        let effect = Effect::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature()),
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_none(),
            "Sacrifice should not extract a target filter (resolution-time selection)"
        );
    }

    /// CR 115.1 + Whitemane Lion ruling (issue #563): A non-targeted Bounce
    /// (Oracle text without the word "target", e.g. "return a creature you
    /// control to its owner's hand") must not extract a target filter —
    /// resolution-time `EffectZoneChoice` handles the controller-scoped pick.
    #[test]
    fn extract_target_skips_non_targeting_bounce() {
        let effect = Effect::Bounce {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            destination: None,
            selection: BounceSelection::AtResolution,
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_none(),
            "AtResolution Bounce should not extract a target filter (resolution-time selection)"
        );
    }

    /// CR 115.1 boundary: A targeted Bounce (`selection: Targeted`) still uses
    /// the targeting pipeline. Mirrors `extract_target_keeps_change_zone_from_battlefield`.
    #[test]
    fn extract_target_keeps_targeting_bounce() {
        let effect = Effect::Bounce {
            target: TargetFilter::Typed(TypedFilter::creature()),
            destination: None,
            selection: BounceSelection::Targeted,
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_some(),
            "targeted Bounce (Targeted) must extract a target filter"
        );
    }

    #[test]
    fn extract_target_skips_copy_token_source_filter() {
        let effect = Effect::CopyTokenOf {
            target: TargetFilter::None,
            owner: TargetFilter::Controller,
            source_filter: Some(TargetFilter::Typed(
                TypedFilter::default()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Token, FilterProp::EnteredThisTurn]),
            )),
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![],
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_none(),
            "source-filtered CopyTokenOf chooses sources at resolution, not as targets"
        );
    }

    // === CR 115.1 / CR 115.1d: extract_target_filter mass-effect (Any-filter) tests ===

    /// CR 115.1 / CR 115.1d: `Pump { target: Any }` is a mass broadcast effect — no word
    /// "target" in Oracle text, so no stack-time target slot should be generated.
    /// This is also used as a sentinel value by `try_parse_pump` before the
    /// calling parser threads a real subject (issue #824 class).
    #[test]
    fn extract_target_skips_pump_with_any_filter() {
        use crate::types::ability::PtValue;
        let effect = Effect::Pump {
            power: PtValue::Fixed(-2),
            toughness: PtValue::Fixed(-2),
            target: TargetFilter::Any,
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_none(),
            "Pump{{target: Any}} is a mass effect — must not generate a target slot"
        );
    }

    /// CR 115.1 / CR 115.1d: `Pump { target: Typed(Creature) }` is a genuinely targeted
    /// effect ("target creature gets +N/+M") — must still generate a slot.
    #[test]
    fn extract_target_keeps_pump_with_typed_filter() {
        use crate::types::ability::PtValue;
        let effect = Effect::Pump {
            power: PtValue::Fixed(2),
            toughness: PtValue::Fixed(2),
            target: TargetFilter::Typed(TypedFilter::creature()),
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_some(),
            "Pump with a typed target filter must still generate a target slot"
        );
    }

    /// CR 115.1 / CR 115.1d: `GenericEffect { target: Some(Any) }` is a mass continuous
    /// modification ("each creature gets -2/-2 until end of turn") produced by
    /// `build_layer_effect_until`. No word "target" in Oracle text, so no slot.
    #[test]
    fn extract_target_skips_generic_effect_with_any_filter() {
        // StaticMode is imported below the test section; use fully qualified path.
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(
                crate::types::statics::StaticMode::Continuous,
            )],
            duration: Some(Duration::UntilEndOfTurn),
            target: Some(TargetFilter::Any),
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_none(),
            "GenericEffect{{target: Some(Any)}} is a mass effect — must not generate a target slot (issue #824)"
        );
    }

    /// CR 115.1 / CR 115.1d: `GenericEffect { target: Some(Typed(Creature)) }` is a
    /// targeted continuous modification ("target creature gains haste") — must
    /// still generate a slot.
    #[test]
    fn extract_target_keeps_generic_effect_with_typed_filter() {
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(
                crate::types::statics::StaticMode::Continuous,
            )],
            duration: Some(Duration::UntilEndOfTurn),
            target: Some(TargetFilter::Typed(TypedFilter::creature())),
        };
        assert!(
            extract_target_filter_from_effect(&effect).is_some(),
            "GenericEffect with a typed target must still generate a target slot"
        );
    }

    // === CR 603.2g + CR 603.6a + CR 700.4: SuppressTriggers integration tests ===

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
        // functioning abilities by default, so its ETB observer trigger must
        // not fire when some other creature enters. `process_triggers` must
        // reach through `active_trigger_definitions`, which drops command-zone
        // non-emblem triggers unless they opt in via `trigger_zones`.
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
    fn command_zone_non_emblem_trigger_with_command_opt_in_fires() {
        // CR 113.6b + CR 114.4: Eminence-style triggers explicitly state they
        // function from the command zone. A non-emblem command-zone object with
        // `trigger_zones = [Command]` must therefore be scanned by
        // `process_triggers`; otherwise parser-level `trigger_zones` fixes do
        // not reach runtime.
        let mut state = setup();
        state.active_player = PlayerId(0);

        let commander_id = create_object(
            &mut state,
            CardId(0xC0FFEF),
            PlayerId(0),
            "Command Opt-In Observer".to_string(),
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
                        },
                    ))
                    .destination(Zone::Battlefield)
                    .trigger_zones(vec![Zone::Command]),
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
            1,
            "A command-zone opt-in trigger on a non-emblem object must fire"
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
            false, // track_exiled_by_source
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

        // Simulate tapping the Forest for mana: TappedForMana (CR 106.12a).
        let events = vec![GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: forest,
            produced: vec![crate::types::mana::ManaType::Green],
            tap_state: ManaTapState::FromTap,
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
    fn fertile_ground_cross_controller_routes_mana_to_lands_controller() {
        // CR 109.5 + CR 605.1b regression: when P1 controls Fertile Ground
        // attached to P0's Forest, tapping that Forest for mana must route
        // the bonus mana to P0 (the land's controller / "its controller"),
        // not to P1 (the aura's controller). Bug reported in the wild: AI
        // (P1) gifted a Fertile Ground onto the human's (P0) land; the
        // human tapped the land and got no extra mana because the resolver
        // defaulted to ability.controller. Fix: parser sets
        // `player_scope: TriggeringPlayer` on the executed mana ability so
        // resolver rebinds the controller to the ManaAdded event's player_id.
        use crate::types::ability::{ManaContribution, ManaProduction, PlayerFilter, QuantityExpr};

        let mut state = setup();
        state.active_player = PlayerId(0);

        // P0 controls a Forest.
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

        // P1 controls a Fertile Ground attached to P0's Forest.
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Fertile Ground".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.attached_to = Some(forest.into());
            obj.entered_battlefield_turn = Some(1);
            // Mirror the parser-emitted shape: player_scope on the executed
            // mana ability rebinds resolution controller to TriggeringPlayer.
            let execute = AbilityDefinition::new(
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
            )
            .player_scope(PlayerFilter::TriggeringPlayer);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(execute)
                    .valid_card(TargetFilter::AttachedTo),
            );
        }

        // P0 taps their Forest for mana.
        let events = vec![GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: forest,
            produced: vec![crate::types::mana::ManaType::Green],
            tap_state: ManaTapState::FromTap,
        }];

        process_triggers(&mut state, &events);

        assert_eq!(state.stack.len(), 0, "Mana trigger must resolve inline");
        let p0_pool = state
            .players
            .iter()
            .find(|p| p.id == PlayerId(0))
            .map(|p| p.mana_pool.total())
            .unwrap_or(0);
        let p1_pool = state
            .players
            .iter()
            .find(|p| p.id == PlayerId(1))
            .map(|p| p.mana_pool.total())
            .unwrap_or(0);
        assert_eq!(
            p0_pool, 1,
            "Bonus mana must go to the land's controller (P0), not the aura's controller (P1)"
        );
        assert_eq!(
            p1_pool, 0,
            "Aura's controller (P1) must not gain mana from P0 tapping P0's land"
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
                                fixed_alternative: None,
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

        // Tap the Forest for mana — emits TappedForMana (CR 106.12a).
        let events = vec![GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: forest,
            produced: vec![crate::types::mana::ManaType::Green],
            tap_state: ManaTapState::FromTap,
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

    #[test]
    fn parsed_each_of_your_main_phases_fires_only_on_controller_main_phases() {
        fn run(active_player: PlayerId, phase: Phase) -> usize {
            let mut state = setup();
            state.active_player = active_player;
            state.priority_player = active_player;
            state.phase = phase;

            let source = make_creature(&mut state, PlayerId(0), "Carpet of Flowers", 0, 1);
            let mut trig_def = crate::parser::oracle_trigger::parse_trigger_line(
                "At the beginning of each of your main phases, if you haven't added mana with this ability this turn, you may add X mana of any one color, where X is the number of Islands target opponent controls.",
                "Carpet of Flowers",
            );
            trig_def.execute = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )));
            let obj = state.objects.get_mut(&source).unwrap();
            obj.trigger_definitions.push(trig_def.clone());
            std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trig_def);

            process_triggers(&mut state, &[GameEvent::PhaseChanged { phase }]);
            state.stack.len()
        }

        assert_eq!(run(PlayerId(0), Phase::PreCombatMain), 1);
        assert_eq!(run(PlayerId(0), Phase::PostCombatMain), 1);
        assert_eq!(run(PlayerId(0), Phase::Upkeep), 0);
        assert_eq!(run(PlayerId(1), Phase::PreCombatMain), 0);
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
                        qty: QuantityRef::ManaSpentToCast {
                            scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                            metric: crate::types::ability::CastManaSpentMetric::Total,
                        },
                    },
                    comparator: Comparator::GT,
                    rhs: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: crate::types::ability::ObjectScope::Source,
                        },
                    },
                },
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ManaSpentToCast {
                            scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                            metric: crate::types::ability::CastManaSpentMetric::Total,
                        },
                    },
                    comparator: Comparator::GT,
                    rhs: QuantityExpr::Ref {
                        qty: QuantityRef::Toughness {
                            scope: crate::types::ability::ObjectScope::Source,
                        },
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
            crate::im::Vector::from(vec![SpellCastRecord {
                name: String::new(),
                core_types: vec![CoreType::Sorcery],
                supertypes: vec![],
                subtypes: vec![],
                keywords: vec![],
                colors: vec![],
                mana_value: 3,
                has_x_in_cost: true,
                from_zone: Zone::Hand,
                cast_variant: crate::types::game_state::CastingVariant::Normal,
            }]),
        );
        assert!(
            check_trigger_constraint(&state, &trig_def, source, 0, PlayerId(0), &spell_event),
            "first qualifying X-spell must fire"
        );

        // Case B: first cast is non-qualifying (no X in cost). Constraint must NOT fire.
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            crate::im::Vector::from(vec![SpellCastRecord {
                name: String::new(),
                core_types: vec![CoreType::Instant],
                supertypes: vec![],
                subtypes: vec![],
                keywords: vec![],
                colors: vec![],
                mana_value: 1,
                has_x_in_cost: false,
                from_zone: Zone::Hand,
                cast_variant: crate::types::game_state::CastingVariant::Normal,
            }]),
        );
        assert!(
            !check_trigger_constraint(&state, &trig_def, source, 0, PlayerId(0), &spell_event),
            "non-qualifying spell (no X) must NOT match the first-X-spell constraint"
        );

        // Case C: second qualifying spell (filter count == 2). Must NOT fire.
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            crate::im::Vector::from(vec![
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Sorcery],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 2,
                    has_x_in_cost: true,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Sorcery],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 4,
                    has_x_in_cost: true,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
            ]),
        );
        assert!(
            !check_trigger_constraint(&state, &trig_def, source, 0, PlayerId(0), &spell_event),
            "second X-spell this turn must NOT fire the first-X-spell trigger"
        );

        // Case D: current spell is non-qualifying after an earlier qualifying
        // spell. The filtered count is still 1, but the event spell itself
        // does not match the trigger's filter.
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            crate::im::Vector::from(vec![
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Sorcery],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 2,
                    has_x_in_cost: true,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Instant],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 1,
                    has_x_in_cost: false,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
            ]),
        );
        assert!(
            !check_trigger_constraint(&state, &trig_def, source, 0, PlayerId(0), &spell_event),
            "non-qualifying current spell must NOT fire an Nth qualifying spell trigger"
        );

        // Case E: intervening non-X spell does NOT reset the count — second X-spell still fails.
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            crate::im::Vector::from(vec![
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Sorcery],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 2,
                    has_x_in_cost: true,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Instant],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 1,
                    has_x_in_cost: false,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
                SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Sorcery],
                    supertypes: vec![],
                    subtypes: vec![],
                    keywords: vec![],
                    colors: vec![],
                    mana_value: 4,
                    has_x_in_cost: true,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
            ]),
        );
        assert!(
            !check_trigger_constraint(&state, &trig_def, source, 0, PlayerId(0), &spell_event),
            "intervening non-X spell must not reset qualifying count"
        );
    }

    #[test]
    fn nth_spell_you_trigger_matches_source_controller_spell_only() {
        fn spell_record() -> SpellCastRecord {
            SpellCastRecord {
                name: String::new(),
                core_types: vec![CoreType::Instant],
                supertypes: vec![],
                subtypes: vec![],
                keywords: vec![],
                colors: vec![],
                mana_value: 1,
                has_x_in_cost: false,
                from_zone: Zone::Hand,
                cast_variant: crate::types::game_state::CastingVariant::Normal,
            }
        }

        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Cosmogrand Zenith".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::SpellCast)
                    .valid_target(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::You),
                    ))
                    .constraint(TriggerConstraint::NthSpellThisTurn { n: 2, filter: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    )),
            );
        }

        let opponent_spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Opponent Spell".to_string(),
            Zone::Stack,
        );
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            crate::im::Vector::from(vec![spell_record(), spell_record()]),
        );
        process_triggers(
            &mut state,
            &[GameEvent::SpellCast {
                card_id: CardId(1),
                controller: PlayerId(0),
                object_id: opponent_spell,
            }],
        );
        assert!(
            state.stack.is_empty(),
            "source controller's 'you cast your second spell' trigger must not fire for an opponent's second spell"
        );

        let controller_spell = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Controller Spell".to_string(),
            Zone::Stack,
        );
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(1),
            crate::im::Vector::from(vec![spell_record(), spell_record()]),
        );
        process_triggers(
            &mut state,
            &[GameEvent::SpellCast {
                card_id: CardId(2),
                controller: PlayerId(1),
                object_id: controller_spell,
            }],
        );

        assert_eq!(
            state.stack.len(),
            1,
            "source controller's second spell should fire the trigger"
        );
        assert!(matches!(
            state.stack.back().map(|entry| &entry.kind),
            Some(StackEntryKind::TriggeredAbility { .. })
        ));
        assert_eq!(
            state.stack.back().map(|entry| entry.source_id),
            Some(source)
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
    fn batched_discard_trigger_context_matches_second_discarded_card_type() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Diviner".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.entered_battlefield_turn = Some(1);
            let mut trigger =
                TriggerDefinition::new(TriggerMode::DiscardedAll).execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ));
            trigger.batched = true;
            obj.trigger_definitions.push(trigger);
        }

        let discarded_creature = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Discarded Creature".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&discarded_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let discarded_instant = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Discarded Instant".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&discarded_instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        let candidate_instant = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Candidate Instant".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&candidate_instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        process_triggers(
            &mut state,
            &[
                GameEvent::Discarded {
                    player_id: PlayerId(0),
                    object_id: discarded_creature,
                },
                GameEvent::Discarded {
                    player_id: PlayerId(0),
                    object_id: discarded_instant,
                },
            ],
        );

        assert_eq!(
            state.stack.len(),
            1,
            "DiscardedAll trigger should fire once for the batch"
        );
        let entry_id = state.stack.back().unwrap().id;
        let trigger_event = match &state.stack.back().unwrap().kind {
            StackEntryKind::TriggeredAbility { trigger_event, .. } => trigger_event.clone(),
            _ => panic!("Expected TriggeredAbility on stack"),
        };
        let trigger_events = state
            .stack_trigger_event_batches
            .get(&entry_id)
            .expect("batched trigger should store full event set")
            .clone();
        assert_eq!(trigger_events.len(), 2);

        state.current_trigger_event = trigger_event;
        state.current_trigger_events = trigger_events;

        let filter =
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::SharesQuality {
                    quality: SharedQuality::CardType,
                    reference: Some(Box::new(TargetFilter::TriggeringSource)),
                    relation: SharedQualityRelation::Shares,
                }]),
            );

        assert!(
            matches_target_filter(
                &state,
                candidate_instant,
                &filter,
                &FilterContext::from_source(&state, source),
            ),
            "shared-quality reference should see the second discarded card's Instant type"
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
            &TriggerCondition::WasCast { zone: None },
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
            &TriggerCondition::WasCast { zone: None },
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
            &TriggerCondition::WasCast { zone: None },
            PlayerId(0),
            Some(cast_spell),
            None,
        ));

        // And false when the self-referential source was not cast.
        state.objects.get_mut(&cast_spell).unwrap().cast_from_zone = None;
        assert!(!check_trigger_condition(
            &state,
            &TriggerCondition::WasCast { zone: None },
            PlayerId(0),
            Some(cast_spell),
            None,
        ));
    }

    /// CR 603.2c + CR 603.4: Twilight Diviner's batched intervening-if trigger
    /// must be gated by the entering creatures' graveyard origin/cast origin.
    /// This exercises the runtime trigger collection path, not just parser shape.
    #[test]
    fn twilight_diviner_batched_graveyard_origin_condition_gates_trigger_runtime() {
        let trigger = crate::parser::oracle_trigger::parse_trigger_line(
            "Whenever one or more other creatures you control enter, if they entered or were cast from a graveyard, create a token that's a copy of one of them. This ability triggers only once each turn.",
            "Twilight Diviner",
        );

        let mut hand_state = setup();
        let hand_source = install_twilight_diviner_trigger(&mut hand_state, trigger.clone());
        let hand_creature = create_entering_creature(&mut hand_state, "Hand Creature", Zone::Hand);
        hand_state
            .objects
            .get_mut(&hand_creature)
            .unwrap()
            .cast_from_zone = Some(Zone::Hand);
        process_triggers(
            &mut hand_state,
            &[zone_changed_event(
                hand_creature,
                Zone::Stack,
                Zone::Battlefield,
                vec![CoreType::Creature],
                Vec::new(),
            )],
        );
        assert!(
            hand_state.stack.is_empty() && hand_state.pending_trigger.is_none(),
            "Twilight Diviner must not trigger for a creature cast from hand; source={hand_source:?}"
        );

        let mut graveyard_state = setup();
        let _graveyard_source = install_twilight_diviner_trigger(&mut graveyard_state, trigger);
        let graveyard_creature =
            create_entering_creature(&mut graveyard_state, "Graveyard Creature", Zone::Graveyard);
        process_triggers(
            &mut graveyard_state,
            &[zone_changed_event(
                graveyard_creature,
                Zone::Graveyard,
                Zone::Battlefield,
                vec![CoreType::Creature],
                Vec::new(),
            )],
        );
        assert!(
            !graveyard_state.stack.is_empty() || graveyard_state.pending_trigger.is_some(),
            "Twilight Diviner must trigger when a creature enters from a graveyard"
        );
    }

    fn install_twilight_diviner_trigger(
        state: &mut GameState,
        trigger: TriggerDefinition,
    ) -> ObjectId {
        let source = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Twilight Diviner".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.trigger_definitions.push(trigger);
        source
    }

    fn create_entering_creature(state: &mut GameState, name: &str, from: Zone) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.cast_from_zone = (from == Zone::Stack).then_some(Zone::Hand);
        id
    }

    #[test]
    fn statically_granted_cascade_triggers_for_cast_spell() {
        let mut state = setup();
        let caster = PlayerId(0);
        let source = create_object(
            &mut state,
            CardId(1),
            caster,
            "Cascade Grant Source".to_string(),
            Zone::Battlefield,
        );
        let grant = StaticDefinition::new(StaticMode::CastWithKeyword {
            keyword: Keyword::Cascade,
        })
        .affected(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Sorcery).controller(ControllerRef::You),
        ));
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(grant);

        let spell = create_object(
            &mut state,
            CardId(2),
            caster,
            "Granted Cascade Spell".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.cast_from_zone = Some(Zone::Hand);
        }

        process_triggers(
            &mut state,
            &[GameEvent::SpellCast {
                object_id: spell,
                controller: caster,
                card_id: CardId(2),
            }],
        );

        assert!(
            state.stack.iter().any(|entry| {
                matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { ability, .. }
                        if matches!(ability.effect, Effect::Cascade)
                )
            }),
            "cascade granted by a static ability should enqueue a Cascade trigger"
        );
    }

    #[test]
    fn granted_casualty_triggers_copy_when_paid() {
        let mut state = setup();
        let caster = PlayerId(0);
        let source = create_object(
            &mut state,
            CardId(1),
            caster,
            "Silverquill Source".to_string(),
            Zone::Battlefield,
        );
        let grant = StaticDefinition::new(StaticMode::CastWithKeyword {
            keyword: Keyword::Casualty(1),
        })
        .affected(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Instant).controller(ControllerRef::You),
        ));
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(grant);

        let spell = create_object(
            &mut state,
            CardId(2),
            caster,
            "Test Instant".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.cast_from_zone = Some(Zone::Hand);
        }
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            spell,
            caster,
        );
        ability.context.additional_cost_paid = true;
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: caster,
            kind: StackEntryKind::Spell {
                card_id: CardId(2),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        process_triggers(
            &mut state,
            &[GameEvent::SpellCast {
                object_id: spell,
                controller: caster,
                card_id: CardId(2),
            }],
        );

        assert!(
            state.stack.iter().any(|entry| {
                matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { ability, .. }
                        if matches!(ability.effect, Effect::CopySpell { target: TargetFilter::SelfRef, .. })
                )
            }),
            "paid granted casualty should create a copy trigger"
        );
    }

    #[test]
    fn printed_casualty_no_copy_when_not_paid() {
        let mut state = setup();
        let caster = PlayerId(0);

        let spell = create_object(
            &mut state,
            CardId(1),
            caster,
            "Test Instant".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.cast_from_zone = Some(Zone::Hand);
            obj.additional_cost = Some(AdditionalCost::Optional {
                cost: AbilityCost::Sacrifice {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    count: 1,
                },
                repeatable: false,
            });
            obj.keywords.push(Keyword::Casualty(2));
        }
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            spell,
            caster,
        );
        // additional_cost_paid = false (default)
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: caster,
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        process_triggers(
            &mut state,
            &[GameEvent::SpellCast {
                object_id: spell,
                controller: caster,
                card_id: CardId(1),
            }],
        );

        assert!(
            !state.stack.iter().any(|entry| {
                matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { ability, .. }
                        if matches!(ability.effect, Effect::CopySpell { .. })
                )
            }),
            "unpaid casualty should not create a copy trigger"
        );
    }

    #[test]
    fn background_granted_commander_attack_trigger_uses_defending_player_life_condition() {
        let mut state = setup();
        let controller = PlayerId(0);
        state.players[0].life = 20;
        state.players[1].life = 20;

        let background = create_object(
            &mut state,
            CardId(1),
            controller,
            "Guild Artisan".to_string(),
            Zone::Battlefield,
        );
        let commander = create_object(
            &mut state,
            CardId(2),
            controller,
            "Commander".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&commander).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.is_commander = true;
        }

        let mut granted_trigger = TriggerDefinition::new(TriggerMode::Attacks)
            .valid_card(TargetFilter::SelfRef)
            .condition(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Opponent {
                            aggregate: AggregateFunction::Max,
                        },
                    },
                },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::DefendingPlayer,
                    },
                },
            })
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
        granted_trigger.attack_target_filter = Some(AttackTargetFilter::Player);

        let grant = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Creature).properties(vec![
                    FilterProp::IsCommander,
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                ]),
            ))
            .modifications(vec![ContinuousModification::GrantTrigger {
                trigger: Box::new(granted_trigger),
            }]);
        state
            .objects
            .get_mut(&background)
            .unwrap()
            .static_definitions
            .push(grant);
        state.layers_dirty.mark_full();

        process_triggers(
            &mut state,
            &[GameEvent::AttackersDeclared {
                attacker_ids: vec![commander],
                defending_player: PlayerId(1),
                attacks: vec![(
                    commander,
                    crate::game::combat::AttackTarget::Player(PlayerId(1)),
                )],
            }],
        );

        assert!(
            state.stack.iter().any(|entry| entry.source_id == commander
                && matches!(&entry.kind, StackEntryKind::TriggeredAbility { ability, .. }
                    if matches!(ability.effect, Effect::Draw { .. }))),
            "Guild Artisan-style Background grant should trigger from the attacking commander"
        );
    }

    #[test]
    fn additional_cost_paid_uses_entering_object_kicker_facts() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kicker Watcher".to_string(),
            Zone::Battlefield,
        );
        let entering = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Kicked Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&entering)
            .unwrap()
            .kickers_paid
            .push(KickerVariant::First);

        let event = zone_changed_event(
            entering,
            Zone::Stack,
            Zone::Battlefield,
            vec![CoreType::Creature],
            vec![],
        );

        assert!(check_trigger_condition(
            &state,
            &TriggerCondition::AdditionalCostPaid {
                source: crate::types::ability::AdditionalCostPaymentSource::Any,
                variant: None,
                kicker_cost: None,
                min_count: 1,
            },
            PlayerId(0),
            Some(source),
            Some(&event),
        ));
    }

    #[test]
    fn additional_cost_paid_min_count_checks_multikicker_count() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kicked Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .kickers_paid
            .extend([KickerVariant::First, KickerVariant::First]);

        assert!(check_trigger_condition(
            &state,
            &TriggerCondition::AdditionalCostPaid {
                source: crate::types::ability::AdditionalCostPaymentSource::Kicker,
                variant: None,
                kicker_cost: None,
                min_count: 2,
            },
            PlayerId(0),
            Some(source),
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
            nth_in_turn: 1,
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
            nth_in_turn: 2,
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
            nth_in_turn: 3,
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
            nth_in_turn: 1,
            nth_in_step: 1,
        };
        assert!(
            check_trigger_condition(&state, &condition, controller, None, Some(&opponent_draw)),
            "draw step draws by the non-active player must fire"
        );
    }

    /// CR 603.4 + CR 102.1 — `DuringPlayersTurn { TriggeringPlayer }`
    /// gates Tataru Taru's Scions' Secretary so it ONLY fires when an opponent
    /// draws a card on a turn that isn't theirs. Drawing on their own turn (the
    /// drawer == active player) must NOT fire.
    #[test]
    fn during_players_turn_triggering_player_tracks_event_player() {
        let mut state = setup();
        let controller = PlayerId(0); // Tataru Taru's owner
        let opponent = PlayerId(1);

        let affirmative = TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::TriggeringPlayer,
        };
        let negation = TriggerCondition::Not {
            condition: Box::new(affirmative.clone()),
        };

        // Opponent draws on their own turn → affirmative true, negation false.
        state.active_player = opponent;
        let own_turn_draw = GameEvent::CardDrawn {
            player_id: opponent,
            object_id: ObjectId(50),
            nth_in_turn: 1,
            nth_in_step: 1,
        };
        assert!(
            check_trigger_condition(&state, &affirmative, controller, None, Some(&own_turn_draw)),
            "affirmative must hold when the drawing player IS active"
        );
        assert!(
            !check_trigger_condition(&state, &negation, controller, None, Some(&own_turn_draw)),
            "Tataru Taru must NOT trigger on an opponent's own-turn draw"
        );

        // Opponent draws on the controller's turn → affirmative false, negation true.
        state.active_player = controller;
        let off_turn_draw = GameEvent::CardDrawn {
            player_id: opponent,
            object_id: ObjectId(51),
            nth_in_turn: 2,
            nth_in_step: 2,
        };
        assert!(
            !check_trigger_condition(&state, &affirmative, controller, None, Some(&off_turn_draw)),
            "affirmative must NOT hold when the drawing player is not active"
        );
        assert!(
            check_trigger_condition(&state, &negation, controller, None, Some(&off_turn_draw)),
            "Tataru Taru MUST trigger when an opponent draws on a turn that isn't theirs"
        );
    }

    /// CR 603.4 + CR 102.1 — `DuringPlayersTurn { Controller }` preserves the
    /// pre-refactor semantics of the retired `DuringYourTurn` variant: true iff
    /// the active player is the trigger controller.
    #[test]
    fn during_players_turn_controller_tracks_active_vs_controller() {
        let mut state = setup();
        let controller = PlayerId(0);
        let condition = TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        };

        state.active_player = PlayerId(0);
        assert!(check_trigger_condition(
            &state, &condition, controller, None, None
        ));

        state.active_player = PlayerId(1);
        assert!(!check_trigger_condition(
            &state, &condition, controller, None, None
        ));
    }

    /// CR 603.4 + CR 102.1 + CR 102.2 — `DuringPlayersTurn { Opponent }`
    /// preserves the pre-refactor semantics of the retired `DuringOpponentsTurn`
    /// variant: true iff the active player is NOT the trigger controller.
    #[test]
    fn during_players_turn_opponent_tracks_active_not_controller() {
        let mut state = setup();
        let controller = PlayerId(0);
        let condition = TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Opponent,
        };

        state.active_player = PlayerId(0);
        assert!(!check_trigger_condition(
            &state, &condition, controller, None, None
        ));

        state.active_player = PlayerId(1);
        assert!(check_trigger_condition(
            &state, &condition, controller, None, None
        ));
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
        state.layers_dirty.mark_full();

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
        state.layers_dirty.mark_full();

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
        state.layers_dirty.mark_full();

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
        state.layers_dirty.mark_full();
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

    #[test]
    fn arcane_adaptation_vampire_type_change_is_visible_to_etb_trigger_matching() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let adaptation = create_object(
            &mut state,
            CardId(0xADAF),
            PlayerId(0),
            "Arcane Adaptation".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&adaptation).unwrap();
            obj.chosen_attributes
                .push(ChosenAttribute::CreatureType("Vampire".to_string()));
            let static_def = StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ))
                .modifications(vec![ContinuousModification::AddChosenSubtype {
                    kind: ChosenSubtypeKind::CreatureType,
                }]);
            obj.static_definitions.push(static_def.clone());
            std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(static_def);
        }

        let evelyn = make_creature(&mut state, PlayerId(0), "Evelyn, the Covetous", 2, 5);
        {
            let obj = state.objects.get_mut(&evelyn).unwrap();
            obj.card_types.subtypes.push("Vampire".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::creature()
                            .subtype("Vampire".to_string())
                            .controller(ControllerRef::You),
                    ))
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    )),
            );
        }

        let bear = make_creature(&mut state, PlayerId(0), "Grizzly Bears", 2, 2);
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            obj.card_types.subtypes.push("Bear".to_string());
            obj.base_card_types = obj.card_types.clone();
        }
        state.layers_dirty.mark_full();

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
            1,
            "Arcane Adaptation's type-changing layer must make the entering creature match Evelyn's Vampire ETB trigger"
        );
    }

    /// CR 113.2c + CR 603.2 + CR 603.3b: Issue #416 — Boggart Prankster.
    /// Each instance of a printed triggered ability fires independently. Two
    /// Boggart Pranksters on the battlefield each have a separate
    /// `Whenever you attack, target attacking Goblin you control gets +1/+0`
    /// trigger; both must reach the stack when the controller attacks. When
    /// the first trigger's target selection requires player input (multiple
    /// legal attacking Goblins), the second was silently dropped because
    /// `process_triggers` early-returned without queuing remaining triggers.
    /// The fix uses `state.deferred_triggers` to park siblings; this test
    /// drives `declare_attackers` end-to-end and asserts both triggers reach
    /// the stack via the player-choice resolution path.
    #[test]
    fn issue_416_two_boggart_pranksters_both_attack_triggers_reach_stack() {
        use crate::game::combat::AttackTarget;
        use crate::types::ability::PtValue;

        let mut state = setup();
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![],
            valid_attack_targets: vec![],
        };

        // Boggart Prankster trigger: Whenever you attack, target attacking
        // Goblin you control gets +1/+0 until end of turn.
        let prankster_trigger = || {
            TriggerDefinition::new(TriggerMode::YouAttack).execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Pump {
                    power: PtValue::Fixed(1),
                    toughness: PtValue::Fixed(0),
                    target: TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Subtype("Goblin".to_string()))
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::Attacking]),
                    ),
                },
            ))
        };

        let make_prankster = |state: &mut GameState, name: &str| -> ObjectId {
            let id = create_object(
                state,
                CardId(state.next_object_id),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Goblin".to_string());
            obj.card_types.subtypes.push("Rogue".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(prankster_trigger());
            std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(prankster_trigger());
            id
        };

        let prankster_a = make_prankster(&mut state, "Boggart Prankster A");
        let prankster_b = make_prankster(&mut state, "Boggart Prankster B");

        // Declare both Pranksters attacking the opponent. Each Prankster is
        // itself an attacking Goblin, so each trigger has TWO legal targets
        // (prankster_a and prankster_b), forcing player-choice resolution.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::DeclareAttackers {
                attacks: vec![
                    (prankster_a, AttackTarget::Player(PlayerId(1))),
                    (prankster_b, AttackTarget::Player(PlayerId(1))),
                ],
            },
        )
        .expect("declare attackers");

        // CR 603.3b (#531): The active player controls 2 simultaneous attack
        // triggers; the engine surfaces an OrderTriggers prompt first. Drain
        // with identity order so the legacy `TriggerTargetSelection` assertion
        // below sees the post-ordering pause state.
        super::drain_order_triggers_with_identity(&mut state);

        // After declare_attackers, the engine should be prompting the
        // attacker's controller to pick a target for the first triggered
        // ability. The second trigger must be parked in `deferred_triggers`,
        // not dropped.
        assert!(
            matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }),
            "expected TriggerTargetSelection for first Prankster trigger, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_trigger.is_some(), "active trigger parked");
        assert_eq!(
            state.deferred_triggers.len(),
            1,
            "the second Prankster's trigger must wait in the deferred queue, \
             not be dropped (issue #416)"
        );

        // Player chooses the second Prankster as target for the first trigger.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(prankster_b)),
            },
        )
        .expect("first prankster choose target");

        // Now the deferred trigger should be active, again prompting for a
        // target choice (still two legal attacking Goblins).
        assert!(
            matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }),
            "expected TriggerTargetSelection for second Prankster trigger, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_trigger.is_some());
        assert_eq!(state.deferred_triggers.len(), 0);
        // CR 603.3c + CR 603.3d: Under "push first, choose second", both
        // Prankster triggers are on the stack now — the first is fully
        // constructed at the bottom, the second is mid-construction at the top
        // (target prompt outstanding). `pending_trigger_entry` identifies the
        // mid-construction entry; the resolver refuses to fire it.
        assert_eq!(
            state.stack.len(),
            2,
            "first Prankster trigger pushed (complete) before second pushed (in-construction)"
        );
        assert!(state.pending_trigger_entry.is_some());
        assert_eq!(
            state.stack.back().map(|e| e.id),
            state.pending_trigger_entry
        );

        // Player chooses the first Prankster as target for the second trigger.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(prankster_a)),
            },
        )
        .expect("second prankster choose target");

        // Both triggers are now on the stack (both fully constructed).
        assert_eq!(
            state.stack.len(),
            2,
            "both Prankster attack triggers must reach the stack"
        );
        assert!(state.pending_trigger.is_none());
        assert!(state.pending_trigger_entry.is_none());
        assert!(state.deferred_triggers.is_empty());

        // Resolve both triggers by passing priority.
        let mut safety = 20;
        while !state.stack.is_empty() && safety > 0 {
            crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                .expect("pass priority");
            safety -= 1;
            // Break out if waiting_for changes to something interactive.
            if !matches!(
                state.waiting_for,
                WaitingFor::Priority { .. } | WaitingFor::GameOver { .. }
            ) {
                break;
            }
        }

        // Each Prankster pumped the other +1/+0 until end of turn.
        let a = state.objects.get(&prankster_a).unwrap();
        let b = state.objects.get(&prankster_b).unwrap();
        assert_eq!(
            a.power,
            Some(2),
            "Prankster A should be 2/1 after receiving +1/+0 from B's trigger"
        );
        assert_eq!(
            b.power,
            Some(2),
            "Prankster B should be 2/1 after receiving +1/+0 from A's trigger"
        );
    }

    /// Issue #610 — Kratos, Stoic Father. A YouAttack trigger carrying a
    /// `valid_card` attacker-type filter (`Subtype "God"`) must fire iff at least
    /// one *God* attacks (CR 508.1 + CR 506.2 + CR 603.2c). Drives the real
    /// `declare_attackers` path end-to-end (not hand-constructed combat state):
    /// declaring only a non-God attacker must NOT fire; declaring a God attacker
    /// MUST fire. This is the discriminating test for the matcher's new
    /// `valid_card` gate — pre-fix the matcher ignored `valid_card` and fired on
    /// any attacker.
    #[test]
    fn issue_610_you_attack_with_god_filter_fires_only_for_god() {
        use crate::game::combat::AttackTarget;

        // Build the typed YouAttack trigger once: fires on "you attack with one
        // or more Gods", granting an experience counter.
        let god_attack_trigger = || {
            let mut def = TriggerDefinition::new(TriggerMode::YouAttack);
            def.batched = true;
            def.valid_target = Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ));
            def.valid_card = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
                "God".to_string(),
            ))));
            def.execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GivePlayerCounter {
                    counter_kind: crate::types::player::PlayerCounterKind::Experience,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ))
        };

        // Helper: run declare_attackers with the named attacker, return whether
        // the YouAttack trigger reached the stack.
        let trigger_fires = |attacker_is_god: bool| -> bool {
            let mut state = setup();
            state.turn_number = 2;
            state.phase = Phase::DeclareAttackers;
            state.active_player = PlayerId(0);
            state.priority_player = PlayerId(0);
            state.waiting_for = WaitingFor::DeclareAttackers {
                player: PlayerId(0),
                valid_attacker_ids: vec![],
                valid_attack_targets: vec![],
            };

            // The Kratos-like commander carries the trigger but does not attack.
            let kratos = make_creature(&mut state, PlayerId(0), "Kratos, Stoic Father", 4, 4);
            {
                let obj = state.objects.get_mut(&kratos).unwrap();
                obj.card_types.subtypes.push("God".to_string());
                obj.base_card_types = obj.card_types.clone();
                obj.entered_battlefield_turn = Some(1);
                obj.trigger_definitions.push(god_attack_trigger());
                std::sync::Arc::make_mut(&mut obj.base_trigger_definitions)
                    .push(god_attack_trigger());
            }

            // The attacker: a God or a plain creature depending on the case.
            let attacker = make_creature(&mut state, PlayerId(0), "Attacker", 2, 2);
            {
                let obj = state.objects.get_mut(&attacker).unwrap();
                if attacker_is_god {
                    obj.card_types.subtypes.push("God".to_string());
                    obj.base_card_types = obj.card_types.clone();
                }
                obj.entered_battlefield_turn = Some(1);
            }

            crate::game::engine::apply_as_current(
                &mut state,
                GameAction::DeclareAttackers {
                    attacks: vec![(attacker, AttackTarget::Player(PlayerId(1)))],
                },
            )
            .expect("declare attackers");

            // The GivePlayerCounter trigger has no target slot, so a fired trigger
            // lands directly on the stack (no interactive prompt).
            !state.stack.is_empty()
        };

        assert!(
            !trigger_fires(false),
            "YouAttack God-filter trigger must NOT fire when only a non-God attacks"
        );
        assert!(
            trigger_fires(true),
            "YouAttack God-filter trigger MUST fire when a God attacks"
        );
    }

    /// Issue #610 (subject-led regression / silent-bug repair) — Killian-class
    /// "Whenever one or more <TYPE> attack". The subject-led parser path already
    /// populated `valid_card` for count==1, but the matcher ignored it, so these
    /// fired on ANY attacker. The matcher fix (NOT a parser edit) repairs them:
    /// the trigger must now fire only on the typed attacker. Drives the real
    /// `declare_attackers` path.
    #[test]
    fn issue_610_subject_led_one_or_more_gods_attack_honors_filter() {
        use crate::game::combat::AttackTarget;

        // Subject-led shape: valid_card = God, valid_target = None (controller
        // gate falls through to "source controller == attacking player").
        let subject_led_trigger = || {
            let mut def = TriggerDefinition::new(TriggerMode::YouAttack);
            def.batched = true;
            def.valid_card = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
                "God".to_string(),
            ))));
            def.execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GivePlayerCounter {
                    counter_kind: crate::types::player::PlayerCounterKind::Experience,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ))
        };

        let trigger_fires = |attacker_is_god: bool| -> bool {
            let mut state = setup();
            state.turn_number = 2;
            state.phase = Phase::DeclareAttackers;
            state.active_player = PlayerId(0);
            state.priority_player = PlayerId(0);
            state.waiting_for = WaitingFor::DeclareAttackers {
                player: PlayerId(0),
                valid_attacker_ids: vec![],
                valid_attack_targets: vec![],
            };

            let source = make_creature(&mut state, PlayerId(0), "Subject Source", 1, 1);
            {
                let obj = state.objects.get_mut(&source).unwrap();
                obj.entered_battlefield_turn = Some(1);
                obj.trigger_definitions.push(subject_led_trigger());
                std::sync::Arc::make_mut(&mut obj.base_trigger_definitions)
                    .push(subject_led_trigger());
            }

            let attacker = make_creature(&mut state, PlayerId(0), "Attacker", 2, 2);
            {
                let obj = state.objects.get_mut(&attacker).unwrap();
                if attacker_is_god {
                    obj.card_types.subtypes.push("God".to_string());
                    obj.base_card_types = obj.card_types.clone();
                }
                obj.entered_battlefield_turn = Some(1);
            }

            crate::game::engine::apply_as_current(
                &mut state,
                GameAction::DeclareAttackers {
                    attacks: vec![(attacker, AttackTarget::Player(PlayerId(1)))],
                },
            )
            .expect("declare attackers");

            !state.stack.is_empty()
        };

        assert!(
            !trigger_fires(false),
            "subject-led God-filter trigger must NOT fire when only a non-God attacks \
             (silent-bug repair)"
        );
        assert!(
            trigger_fires(true),
            "subject-led God-filter trigger MUST fire when a God attacks"
        );
    }

    /// Issue #1055: The Earth King stores "that many" as EventContextAmount.
    /// For a batched attack trigger, that amount is the number of attackers
    /// that matched the trigger subject, not the raw number of all attackers.
    #[test]
    fn issue_1055_batched_attack_search_uses_matching_attacker_count() {
        use crate::game::combat::AttackTarget;

        fn add_basic_land(state: &mut GameState, name: &str) -> ObjectId {
            let id = create_object(
                state,
                CardId(state.next_object_id),
                PlayerId(0),
                name.to_string(),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
            id
        }

        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source = make_creature(&mut state, PlayerId(0), "The Earth King", 4, 4);
        let mut trigger =
            TriggerDefinition::new(TriggerMode::YouAttack).execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::SearchLibrary {
                    filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                        FilterProp::HasSupertype {
                            value: crate::types::card_type::Supertype::Basic,
                        },
                    ])),
                    count: QuantityExpr::up_to(QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    }),
                    reveal: false,
                    target_player: None,
                    selection_constraint: SearchSelectionConstraint::None,
                    split: None,
                    source_zones: vec![crate::types::zones::Zone::Library],
                },
            ));
        trigger.batched = true;
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 4 },
                }]),
        ));
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        let big_one = make_creature(&mut state, PlayerId(0), "Big One", 4, 4);
        let big_two = make_creature(&mut state, PlayerId(0), "Big Two", 5, 5);
        let small = make_creature(&mut state, PlayerId(0), "Small", 3, 3);
        add_basic_land(&mut state, "Plains");
        add_basic_land(&mut state, "Island");
        add_basic_land(&mut state, "Forest");

        process_triggers(
            &mut state,
            &[GameEvent::AttackersDeclared {
                attacker_ids: vec![big_one, big_two, small],
                defending_player: PlayerId(1),
                attacks: vec![
                    (big_one, AttackTarget::Player(PlayerId(1))),
                    (big_two, AttackTarget::Player(PlayerId(1))),
                    (small, AttackTarget::Player(PlayerId(1))),
                ],
            }],
        );

        assert_eq!(state.stack.len(), 1, "The Earth King trigger should fire");
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        match &state.waiting_for {
            WaitingFor::SearchChoice {
                count,
                up_to,
                cards,
                ..
            } => {
                assert_eq!(*count, 2, "two power-4+ attackers set the search cap");
                assert!(*up_to, "The Earth King's search is up to that many");
                assert_eq!(cards.len(), 3, "all three basic lands remain legal choices");
            }
            other => panic!("expected SearchChoice, got {other:?}"),
        }
    }

    #[test]
    fn batched_attack_context_filters_attacked_target_type() {
        use crate::game::combat::AttackTarget;

        fn add_basic_land(state: &mut GameState, name: &str) {
            let id = create_object(
                state,
                CardId(state.next_object_id),
                PlayerId(0),
                name.to_string(),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }

        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source = make_creature(&mut state, PlayerId(0), "Attack Trigger Source", 4, 4);
        let mut trigger =
            TriggerDefinition::new(TriggerMode::YouAttack).execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::SearchLibrary {
                    filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                        FilterProp::HasSupertype {
                            value: crate::types::card_type::Supertype::Basic,
                        },
                    ])),
                    count: QuantityExpr::up_to(QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    }),
                    reveal: false,
                    target_player: None,
                    selection_constraint: SearchSelectionConstraint::None,
                    split: None,
                    source_zones: vec![crate::types::zones::Zone::Library],
                },
            ));
        trigger.batched = true;
        trigger.valid_card = Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 4 },
            }],
        )));
        trigger.attack_target_filter = Some(AttackTargetFilter::Player);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        let attacks_player = make_creature(&mut state, PlayerId(0), "Attacks Player", 4, 4);
        let attacks_planeswalker =
            make_creature(&mut state, PlayerId(0), "Attacks Planeswalker", 5, 5);
        let small_attacks_player =
            make_creature(&mut state, PlayerId(0), "Small Attacks Player", 3, 3);
        let planeswalker = create_object(
            &mut state,
            CardId(9000),
            PlayerId(1),
            "Target Planeswalker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&planeswalker)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Planeswalker);
        add_basic_land(&mut state, "Plains");
        add_basic_land(&mut state, "Island");
        add_basic_land(&mut state, "Forest");

        process_triggers(
            &mut state,
            &[GameEvent::AttackersDeclared {
                attacker_ids: vec![attacks_player, attacks_planeswalker, small_attacks_player],
                defending_player: PlayerId(1),
                attacks: vec![
                    (attacks_player, AttackTarget::Player(PlayerId(1))),
                    (
                        attacks_planeswalker,
                        AttackTarget::Planeswalker(planeswalker),
                    ),
                    (small_attacks_player, AttackTarget::Player(PlayerId(1))),
                ],
            }],
        );

        assert_eq!(state.stack.len(), 1, "filtered attack trigger should fire");
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        match &state.waiting_for {
            WaitingFor::SearchChoice { count, .. } => {
                assert_eq!(
                    *count, 1,
                    "only the power-4+ creature attacking a player contributes to that-many"
                );
            }
            other => panic!("expected SearchChoice, got {other:?}"),
        }
    }

    /// Issue #501 (class coverage): The Tenth Doctor's `Allons-y!` attack
    /// trigger exiles cards until a nonland is exiled, puts three time counters
    /// on it, and grants it Suspend — the same runtime-granted-Suspend pattern
    /// as Jhoira of the Ghitu, reached via the exile-from-library path rather
    /// than the cost-paid-object path. Drives the real `YouAttack` trigger
    /// through `DeclareAttackers` + `PassPriority` resolution, then asserts the
    /// exiled library card ends with 3 Time counters AND carries the suspend
    /// upkeep counter-removal trigger (installed by Layer 6 →
    /// `KeywordTriggerInstaller::triggers_for`). Confirms the #501 fix covers
    /// the whole class, not just Jhoira.
    #[test]
    fn tenth_doctor_allons_y_grants_working_suspend() {
        use crate::game::combat::AttackTarget;
        use crate::types::counter::CounterType;

        let mut state = setup();
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![],
            valid_attack_targets: vec![],
        };

        // The Tenth Doctor — attacking creature carrying the Allons-y! trigger.
        let doctor = make_creature(&mut state, PlayerId(0), "The Tenth Doctor", 3, 5);
        {
            let parsed = crate::parser::oracle::parse_oracle_text(
                "Allons-y! — Whenever you attack, exile cards from the top of \
                 your library until you exile a nonland card. Put three time \
                 counters on it. If it doesn't have suspend, it gains suspend.",
                "The Tenth Doctor",
                &[],
                &[String::from("Creature")],
                &[],
            );
            let obj = state.objects.get_mut(&doctor).unwrap();
            obj.entered_battlefield_turn = Some(1);
            for trig in &parsed.triggers {
                obj.trigger_definitions.push(trig.clone());
            }
            std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).extend(parsed.triggers);
        }

        // Top of P0's library: a nonland card (the Allons-y! target).
        let nonland = create_object(
            &mut state,
            CardId(7001),
            PlayerId(0),
            "Library Sorcery".to_string(),
            Zone::Library,
        );
        {
            let nl = state.objects.get_mut(&nonland).unwrap();
            nl.card_types.core_types.push(CoreType::Sorcery);
            nl.base_card_types = nl.card_types.clone();
        }
        // CR 104.3c: stock both libraries so neither player decks out while the
        // combined A+B assertion drives real turn progression to the next upkeep.
        for player in [PlayerId(0), PlayerId(1)] {
            for i in 0..12u64 {
                create_object(
                    &mut state,
                    CardId(7100 + u64::from(player.0) * 100 + i),
                    player,
                    format!("Library Filler {}-{i}", player.0),
                    Zone::Library,
                );
            }
        }

        // Declare the Tenth Doctor attacking the opponent.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::DeclareAttackers {
                attacks: vec![(doctor, AttackTarget::Player(PlayerId(1)))],
            },
        )
        .expect("declare attackers");

        // Resolve the Allons-y! trigger by passing priority.
        let mut safety = 30;
        while !state.stack.is_empty() && safety > 0 {
            if !matches!(
                state.waiting_for,
                WaitingFor::Priority { .. } | WaitingFor::GameOver { .. }
            ) {
                break;
            }
            crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                .expect("pass priority to resolve Allons-y!");
            safety -= 1;
        }

        // The Allons-y! trigger resolved: the nonland card was exiled from the
        // library (CR 701.13a `ExileFromTopUntil` → `NextMatches` nonland).
        assert_eq!(
            state.objects[&nonland].zone,
            Zone::Exile,
            "The Tenth Doctor's Allons-y! must exile the top nonland card"
        );

        // #501 CLASS COVERAGE — CR 702.62a + CR 604.1: the `SequentialSibling`
        // `AddKeyword{Suspend}` (which targets `ParentTarget` = the exiled hit)
        // grants Suspend to the library-exiled card, and that granted keyword
        // resolves through `KeywordTriggerInstaller` to the two companion
        // triggered abilities. This is the exact #501 fix exercised on the
        // exile-from-library class member, reached via the real `YouAttack`
        // trigger pipeline rather than Jhoira's cost-paid-object path.
        let off_zone_kws =
            crate::game::off_zone_characteristics::effective_off_zone_keywords(&state, nonland);
        let granted_suspend = off_zone_kws
            .iter()
            .find(|k| k.kind() == crate::types::keywords::KeywordKind::Suspend)
            .expect("the exiled card must have suspend granted by Allons-y!");
        let companion =
            crate::database::synthesis::KeywordTriggerInstaller::triggers_for(granted_suspend);
        assert_eq!(
            companion.len(),
            2,
            "granted Suspend must carry both suspend companion triggers (CR 702.62a)"
        );
        let has_upkeep_trigger = companion.iter().any(|t| {
            matches!(t.mode, TriggerMode::Phase)
                && t.phase == Some(Phase::Upkeep)
                && t.trigger_zones == vec![Zone::Exile]
                && matches!(
                    t.execute.as_deref().map(|a| &*a.effect),
                    Some(Effect::RemoveCounter {
                        counter_type: Some(CounterType::Time),
                        target: TargetFilter::SelfRef,
                        ..
                    })
                )
        });
        assert!(
            has_upkeep_trigger,
            "granted Suspend must carry the upkeep counter-removal trigger \
             for the library-exiled card (issue #501 class coverage)"
        );

        // #501 FOLLOW-UP — ROOT CAUSE B discriminator (CR 608.2c + CR 122):
        // "Put three time counters on it" — the anaphor "it" binds to the
        // ExileFromTopUntil-introduced hit card, NOT the trigger source. The
        // `has_typed_target` arm for `ExileFromTopUntil { NextMatches }` routes
        // the `PutCounter` clause through `replace_target_with_parent`, rewriting
        // `target` SelfRef → ParentTarget so the resolver's injected `sub_clone.
        // targets` (the exiled card) receives the counters. Reverted-fix
        // discriminator: without the `has_typed_target` arm the clause keeps
        // `target: SelfRef`, the 3 counters land on the Doctor, and these two
        // assertions fail (exiled card gets 0, Doctor gets 3).
        assert_eq!(
            state.objects[&nonland]
                .counters
                .get(&CounterType::Time)
                .copied()
                .unwrap_or(0),
            3,
            "Allons-y!'s 'put three time counters on it' must land on the \
             exiled card (issue #501 follow-up, Root Cause B)"
        );
        assert_eq!(
            state.objects[&doctor]
                .counters
                .get(&CounterType::Time)
                .copied()
                .unwrap_or(0),
            0,
            "the time counters must NOT land on The Tenth Doctor itself \
             (anaphoric 'it' mis-target — issue #501 follow-up, Root Cause B)"
        );

        // #501 FOLLOW-UP — A + B COMBINED: drive real turn progression to
        // PlayerId(0)'s next upkeep. The granted Suspend (Root Cause A:
        // Duration::Permanent) must persist past the activation turn, and the
        // synthesized off-zone upkeep trigger must tick the time counter that
        // Root Cause B's fix placed on the exiled card (3 → 2). This proves the
        // real Tenth Doctor card functions end-to-end with both fixes.
        let start_turn = state.turn_number;
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 300, "turn progression stalled before P0's upkeep");
            if state.phase == Phase::Upkeep
                && state.active_player == PlayerId(0)
                && state.turn_number > start_turn
            {
                break;
            }
            if !state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                    .expect("priority pass to resolve stack");
                continue;
            }
            match &state.waiting_for {
                WaitingFor::Priority { .. } => {
                    crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                        .expect("priority pass to advance the turn");
                }
                WaitingFor::DeclareAttackers { .. } => {
                    crate::game::engine::apply_as_current(
                        &mut state,
                        GameAction::DeclareAttackers { attacks: vec![] },
                    )
                    .expect("declare no attackers");
                }
                WaitingFor::DeclareBlockers { .. } => {
                    crate::game::engine::apply_as_current(
                        &mut state,
                        GameAction::DeclareBlockers {
                            assignments: vec![],
                        },
                    )
                    .expect("declare no blockers");
                }
                other => panic!("unexpected waiting state during turn progression: {other:?}"),
            }
        }
        assert!(
            crate::game::off_zone_characteristics::effective_off_zone_keywords(&state, nonland)
                .iter()
                .any(|k| k.kind() == crate::types::keywords::KeywordKind::Suspend),
            "granted Suspend must persist past the activation turn (Root Cause A)"
        );
        let mut guard = 0;
        while !state.stack.is_empty() {
            guard += 1;
            assert!(guard < 20, "upkeep-trigger stack failed to drain");
            crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                .expect("resolve the suspend upkeep trigger");
        }
        assert_eq!(
            state.objects[&nonland]
                .counters
                .get(&CounterType::Time)
                .copied()
                .unwrap_or(0),
            2,
            "the upkeep trigger must tick the exiled card's time counter 3 → 2 \
             (issue #501 follow-up, A + B combined)"
        );
    }

    /// RUNTIME REGRESSION — issue #886 (Raph & Mikey, Troublemakers).
    /// CR 508.4: "Put that card onto the battlefield tapped and attacking."
    /// Drives the real `Attacks` trigger end-to-end and asserts the revealed
    /// creature joins combat as an attacker and deals its combat damage. Also
    /// covers the class member Fireflux Squad (same RevealUntil → attacking).
    #[test]
    fn raph_mikey_revealed_creature_enters_attacking_and_deals_damage() {
        use crate::game::combat::AttackTarget;

        let mut state = setup();
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.players[0].life = 20;
        state.players[1].life = 20;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![],
            valid_attack_targets: vec![],
        };

        // Raph & Mikey on P0's battlefield, carrying its real Attacks trigger.
        let raph = make_creature(&mut state, PlayerId(0), "Raph & Mikey, Troublemakers", 7, 7);
        {
            let parsed = crate::parser::oracle::parse_oracle_text(
                "Trample, haste\nWhenever Raph & Mikey attack, reveal cards from the \
                 top of your library until you reveal a creature card. Put that card \
                 onto the battlefield tapped and attacking and the rest on the bottom \
                 of your library in a random order.",
                "Raph & Mikey, Troublemakers",
                &[],
                &[String::from("Creature")],
                &[],
            );
            let obj = state.objects.get_mut(&raph).unwrap();
            obj.entered_battlefield_turn = Some(1);
            for trig in &parsed.triggers {
                obj.trigger_definitions.push(trig.clone());
            }
            std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).extend(parsed.triggers);
        }

        // Top of P0's library: a 6/6 creature (the reveal-until hit).
        let revealed = create_object(
            &mut state,
            CardId(8001),
            PlayerId(0),
            "Colossal Dreadmaw".to_string(),
            Zone::Library,
        );
        {
            let o = state.objects.get_mut(&revealed).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.base_card_types = o.card_types.clone();
            o.power = Some(6);
            o.toughness = Some(6);
            o.base_power = Some(6);
            o.base_toughness = Some(6);
        }
        // Library filler so neither player decks out.
        for player in [PlayerId(0), PlayerId(1)] {
            for i in 0..10u64 {
                create_object(
                    &mut state,
                    CardId(8100 + u64::from(player.0) * 100 + i),
                    player,
                    format!("Filler {}-{i}", player.0),
                    Zone::Library,
                );
            }
        }

        // Declare Raph & Mikey attacking P1.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::DeclareAttackers {
                attacks: vec![(raph, AttackTarget::Player(PlayerId(1)))],
            },
        )
        .expect("declare attackers");

        // Resolve the attack trigger by passing priority.
        let mut safety = 40;
        while !state.stack.is_empty() && safety > 0 {
            if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                .expect("pass priority to resolve Raph & Mikey trigger");
            safety -= 1;
        }

        // FIX: the revealed creature is now an attacker (CR 508.4), tapped.
        let combat = state.combat.as_ref().expect("combat in progress");
        assert!(
            combat.attackers.iter().any(|a| a.object_id == raph),
            "Raph & Mikey itself attacks"
        );
        assert!(
            combat.attackers.iter().any(|a| a.object_id == revealed),
            "issue #886 FIX: the revealed creature must enter attacking"
        );
        assert!(
            state.objects[&revealed].tapped,
            "revealed creature is tapped"
        );

        // Drive combat to damage: P1 should lose 7 (Raph) + 6 (Dreadmaw) = 13.
        let mut guard = 0;
        while state.phase != Phase::PostCombatMain && guard < 60 {
            guard += 1;
            if !state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                    .expect("resolve stack");
                continue;
            }
            match &state.waiting_for {
                WaitingFor::Priority { .. } => {
                    crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                        .expect("pass priority through combat");
                }
                WaitingFor::DeclareBlockers { .. } => {
                    crate::game::engine::apply_as_current(
                        &mut state,
                        GameAction::DeclareBlockers {
                            assignments: vec![],
                        },
                    )
                    .expect("declare no blockers");
                }
                _ => break,
            }
        }
        assert_eq!(
            state.players[1].life, 7,
            "issue #886 FIX: P1 takes 7 (Raph) + 6 (revealed Dreadmaw) = 13 combat damage"
        );
    }

    /// RUNTIME REGRESSION — multiple suspended cards (Jhoira of the Ghitu).
    /// CR 603.3b + CR 702.62a: When 2+ cards are suspended (each granted Suspend
    /// while in exile), the controller's upkeep fires one "remove a time counter"
    /// trigger per card. Two-or-more simultaneous same-controller triggers require
    /// the controller to ORDER them (CR 603.3b) before any player gets priority.
    ///
    /// This drives the scenario through the real `apply` pipeline (turn-roll →
    /// `auto_advance` → upkeep). The bug: the Upkeep arm of `auto_advance` called
    /// `process_phase_triggers` (which set `waiting_for = OrderTriggers` and
    /// populated `pending_trigger_order`) and then unconditionally returned
    /// `WaitingFor::Priority`. `apply` wrote that returned `Priority` back over
    /// `state.waiting_for`, discarding the prompt and stranding all queued triggers
    /// in `pending_trigger_order` forever — so NONE of the cards (including the
    /// first) ticked. A single suspended card took the `NoChoiceNeeded` path (no
    /// prompt to clobber), which is exactly why one card worked but several didn't.
    ///
    /// Discriminator: without the fix, the upkeep is reached with
    /// `waiting_for == Priority`, the `OrderTriggers` submission below fails, both
    /// counters stay at 3, and `pending_trigger_order` is left orphaned.
    #[test]
    fn multiple_suspended_cards_all_tick_on_upkeep() {
        use crate::types::counter::CounterType;

        let mut state = setup();
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![],
            valid_attack_targets: vec![],
        };

        // CR 104.3c: stock both libraries so neither player decks out while the
        // turn loop drives real turn progression to P0's next upkeep.
        for player in [PlayerId(0), PlayerId(1)] {
            for i in 0..12u64 {
                create_object(
                    &mut state,
                    CardId(8100 + u64::from(player.0) * 100 + i),
                    player,
                    format!("Filler {}-{i}", player.0),
                    Zone::Library,
                );
            }
        }

        // Jhoira of the Ghitu — the grant source on the battlefield. Each exiled
        // card is granted Suspend via a permanent continuous effect affecting it
        // specifically (mirrors the real `AddKeyword{Suspend}` transient effect
        // Jhoira's activated ability installs per CR 604.1 + CR 702.62a).
        let jhoira = make_creature(&mut state, PlayerId(0), "Jhoira of the Ghitu", 1, 3);

        // Two suspended cards in exile, each with 3 time counters and empty
        // base_keywords (so the off-zone synthesis path treats Suspend as
        // *granted* and installs the companion upkeep triggers).
        let mut suspended = Vec::new();
        for (i, name) in ["Nezahal, Primal Tide", "Omniscience"].iter().enumerate() {
            let card = create_object(
                &mut state,
                CardId(8200 + i as u64),
                PlayerId(0),
                (*name).to_string(),
                Zone::Exile,
            );
            state
                .objects
                .get_mut(&card)
                .unwrap()
                .counters
                .insert(CounterType::Time, 3);
            // Grant Suspend to this specific exiled card via a permanent
            // continuous effect sourced from Jhoira — exactly the
            // `AddKeyword{Suspend}` transient effect Jhoira's activated ability
            // installs in real play (CR 604.1 + CR 702.62a).
            state.transient_continuous_effects.push_back(
                crate::types::game_state::TransientContinuousEffect {
                    id: 100 + i as u64,
                    source_id: jhoira,
                    controller: PlayerId(0),
                    timestamp: 1 + i as u64,
                    duration: Duration::Permanent,
                    affected: TargetFilter::SpecificObject { id: card },
                    modifications: vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Suspend {
                            count: 0,
                            cost: ManaCost::Cost {
                                generic: 0,
                                shards: vec![],
                            },
                        },
                    }],
                    condition: None,
                    source_name: "Jhoira of the Ghitu".to_string(),
                },
            );
            suspended.push(card);
        }
        state.layers_dirty.mark_full();

        // Sanity: both cards must carry granted Suspend off-zone before we drive
        // the turn — otherwise the upkeep triggers never synthesize.
        for &card in &suspended {
            assert!(
                crate::game::off_zone_characteristics::effective_off_zone_keywords(&state, card)
                    .iter()
                    .any(|k| k.kind() == KeywordKind::Suspend),
                "exiled card {card:?} must have granted Suspend before the upkeep"
            );
        }

        // Drive real turn progression (through `apply`, the clobber site) to
        // PlayerId(0)'s NEXT upkeep.
        let start_turn = state.turn_number;
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 300, "turn progression stalled before P0's upkeep");
            if state.phase == Phase::Upkeep
                && state.active_player == PlayerId(0)
                && state.turn_number > start_turn
            {
                break;
            }
            if !state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                    .expect("priority pass to resolve stack");
                continue;
            }
            match &state.waiting_for {
                WaitingFor::Priority { .. } => {
                    crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                        .expect("priority pass to advance the turn");
                }
                WaitingFor::DeclareAttackers { .. } => {
                    crate::game::engine::apply_as_current(
                        &mut state,
                        GameAction::DeclareAttackers { attacks: vec![] },
                    )
                    .expect("declare no attackers");
                }
                WaitingFor::DeclareBlockers { .. } => {
                    crate::game::engine::apply_as_current(
                        &mut state,
                        GameAction::DeclareBlockers {
                            assignments: vec![],
                        },
                    )
                    .expect("declare no blockers");
                }
                other => panic!("unexpected waiting state during turn progression: {other:?}"),
            }
        }

        // CR 603.3b: at P0's upkeep the engine MUST surface the ordering prompt
        // for the two suspend triggers — not a bare Priority. This is the
        // assertion that fails without the `auto_advance` fix.
        match &state.waiting_for {
            WaitingFor::OrderTriggers { player, triggers } => {
                assert_eq!(*player, PlayerId(0), "controller orders own triggers");
                assert_eq!(
                    triggers.len(),
                    2,
                    "both suspend upkeep triggers must be in the ordering prompt"
                );
            }
            other => panic!(
                "expected OrderTriggers prompt for the two suspend triggers, got {other:?} \
                 (pending_trigger_order = {:?})",
                state.pending_trigger_order.is_some()
            ),
        }

        // Submit an order, then drain the two triggers off the stack.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::OrderTriggers { order: vec![0, 1] },
        )
        .expect("submit suspend trigger order");
        let mut guard = 0;
        while !state.stack.is_empty() {
            guard += 1;
            assert!(guard < 20, "suspend upkeep-trigger stack failed to drain");
            crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                .expect("resolve a suspend upkeep trigger");
        }

        // CR 702.62a: BOTH cards must have ticked 3 → 2, and the ordering queue
        // must be fully consumed (no orphan).
        for &card in &suspended {
            assert_eq!(
                state.objects[&card]
                    .counters
                    .get(&CounterType::Time)
                    .copied()
                    .unwrap_or(0),
                2,
                "suspended card {card:?} must tick 3 → 2 on P0's upkeep"
            );
        }
        assert!(
            state.pending_trigger_order.is_none(),
            "pending_trigger_order must be cleared after the ordered triggers resolve"
        );
    }

    /// RUNTIME TEST — issue #411. Drives Syr Konrad's `{1}{B}: Each player mills
    /// a card.` activated ability through the real `apply` pipeline four times.
    /// Both libraries are stacked deterministically: the controller's library
    /// holds only lands (zero clause-2 triggers from the controller's own mill),
    /// the opponent's library holds three creature cards plus one noncreature.
    /// CR 603.2c: each milled creature card is a distinct zone-change event, so
    /// the disjunctive trigger fires once per milled creature. Exactly three
    /// triggers fire (the opponent's three creatures), each dealing 1 damage to
    /// each opponent — the controller's lands and the noncreature contribute zero.
    #[test]
    fn test_syr_konrad_disjunctive_trigger_fires_per_milled_creature() {
        use crate::game::scenario::GameScenario;

        const KONRAD_ORACLE: &str = "Whenever another creature dies, or a creature card \
            is put into a graveyard from anywhere other than the battlefield, or a \
            creature card leaves your graveyard, Syr Konrad, the Grim deals 1 damage \
            to each opponent.\n{1}{B}: Each player mills a card.";

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let konrad = scenario
            .add_creature_from_oracle(PlayerId(0), "Syr Konrad, the Grim", 5, 4, KONRAD_ORACLE)
            .id();

        let mut runner = scenario.build();

        // Arrange both decks. `mill` removes `library[0..count]`, so the cards
        // listed here (front-of-vector first) are milled in order across the
        // four activations. This is test arrangement, not trigger faking.
        let p1_start_life = runner.state().players[1].life;
        stack_library(
            runner.state_mut(),
            PlayerId(0),
            &[
                CoreType::Land,
                CoreType::Land,
                CoreType::Land,
                CoreType::Land,
            ],
        );
        stack_library(
            runner.state_mut(),
            PlayerId(1),
            &[
                CoreType::Creature,
                CoreType::Creature,
                CoreType::Creature,
                CoreType::Sorcery,
            ],
        );

        // Fund four activations of the {1}{B} cost.
        {
            let p0 = runner
                .state_mut()
                .players
                .iter_mut()
                .find(|p| p.id == PlayerId(0))
                .unwrap();
            for _ in 0..4 {
                p0.mana_pool.add(ManaUnit {
                    color: ManaType::Colorless,
                    source_id: ObjectId(0),
                    snow: false,
                    source_could_produce_two_or_more_colors: false,
                    restrictions: Vec::new(),
                    grants: vec![],
                    expiry: None,
                });
                p0.mana_pool.add(ManaUnit {
                    color: ManaType::Black,
                    source_id: ObjectId(0),
                    snow: false,
                    source_could_produce_two_or_more_colors: false,
                    restrictions: Vec::new(),
                    grants: vec![],
                    expiry: None,
                });
            }
        }

        for activation in 0..4 {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: konrad,
                    ability_index: 0,
                })
                .unwrap_or_else(|e| panic!("activation {activation} failed: {e:?}"));
            resolve_stack_to_empty(&mut runner);
        }

        // Both libraries are fully milled.
        assert_eq!(runner.state().players[0].library.len(), 0);
        assert_eq!(runner.state().players[1].library.len(), 0);

        // CR 603.2: exactly three disjunctive triggers fired — one per opponent
        // creature card milled (clause 2). Each dealt 1 damage to the single
        // opponent (P1). The controller's four lands and the opponent's lone
        // noncreature card matched no clause.
        let p1_life_lost = p1_start_life - runner.state().players[1].life;
        assert_eq!(
            p1_life_lost, 3,
            "expected 3 disjunctive triggers (3 opponent creature mills); \
             lands and noncreatures must not fire the trigger"
        );
    }

    /// Place typed cards onto a player's library. The slice order is
    /// front-of-vector first, which `Effect::Mill` consumes top-down.
    fn stack_library(state: &mut GameState, player: PlayerId, core_types: &[CoreType]) {
        // Clear any starting library so the arrangement is fully deterministic.
        if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
            let existing: Vec<_> = p.library.iter().copied().collect();
            for id in &existing {
                state.objects.remove(id);
            }
            state
                .players
                .iter_mut()
                .find(|p| p.id == player)
                .unwrap()
                .library
                .clear();
        }
        for (i, &core_type) in core_types.iter().enumerate() {
            let card_id = CardId(state.next_object_id);
            let id = create_object(
                state,
                card_id,
                player,
                format!("Library Card {i}"),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(core_type);
            obj.base_card_types = obj.card_types.clone();
        }
    }

    /// Pass priority until the stack is empty, bailing on stall.
    fn resolve_stack_to_empty(runner: &mut crate::game::scenario::GameRunner) {
        for _ in 0..40 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            if runner.act(GameAction::PassPriority).is_err() {
                break;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Issue #461: Sowing Mycospawn's kicked cast-trigger ("When you cast this
    // spell, if it was kicked, exile target land.") must actually exile the
    // chosen land. The intervening-'if' AdditionalCostPaid condition (CR 603.4)
    // is rechecked at resolution; it reads `kickers_paid` off the
    // spell-on-stack object, which `finalize_cast_to_stack` must stamp.
    // -----------------------------------------------------------------------

    /// Sowing Mycospawn's full Oracle text (kicker + two SpellCast triggers).
    const SOWING_MYCOSPAWN_ORACLE: &str = "Devoid (This card has no color.)\n\
        Kicker {1}{C} (You may pay an additional {1}{C} as you cast this spell.)\n\
        When you cast this spell, search your library for a land card, put it \
        onto the battlefield, then shuffle.\n\
        When you cast this spell, if it was kicked, exile target land.";

    /// Build a scenario with Sowing Mycospawn in P0's hand and a single land
    /// (P1's) on the battlefield as the exile target. Returns the runner, the
    /// spell's `ObjectId`/`CardId`, and the target land's `ObjectId`.
    fn sowing_mycospawn_scenario() -> (
        crate::game::scenario::GameRunner,
        ObjectId,
        CardId,
        ObjectId,
    ) {
        use crate::game::scenario::GameScenario;
        use crate::types::mana::ManaCostShard;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // {4}{C} mana cost.
        let spell_builder = scenario.add_creature_to_hand_from_oracle(
            PlayerId(0),
            "Sowing Mycospawn",
            3,
            3,
            SOWING_MYCOSPAWN_ORACLE,
        );
        let spell_id = spell_builder.id();
        let spell_card_id = scenario.state.objects[&spell_id].card_id;
        scenario.state.objects.get_mut(&spell_id).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Colorless],
            generic: 4,
        };

        // The land to be exiled — opponent-controlled so it is unambiguous.
        // P0's library has no land cards, so the first cast-trigger's
        // SearchLibrary fizzles without prompting.
        let target_land = scenario.add_basic_land(PlayerId(1), ManaColor::Red);

        let runner = scenario.build();
        (runner, spell_id, spell_card_id, target_land)
    }

    /// Add enough colorless mana to pay {4}{C} plus the {1}{C} kicker.
    fn fund_sowing_mycospawn(runner: &mut crate::game::scenario::GameRunner, kicked: bool) {
        let count = if kicked { 7 } else { 5 };
        let p0 = runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..count {
            p0.mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                snow: false,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    /// Drive every casting/targeting prompt to completion, paying the kicker
    /// per `kicked` and routing any target prompt to `target_land`. Returns
    /// once the stack is empty and P0 holds priority.
    fn drive_sowing_mycospawn(
        runner: &mut crate::game::scenario::GameRunner,
        kicked: bool,
        target_land: ObjectId,
    ) {
        for _ in 0..60 {
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
                WaitingFor::OptionalCostChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalCost { pay: kicked })
                        .expect("kicker decision must be accepted");
                }
                WaitingFor::TriggerTargetSelection { .. }
                | WaitingFor::TargetSelection { .. }
                | WaitingFor::MultiTargetSelection { .. } => {
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(target_land)),
                        })
                        .or_else(|_| {
                            runner.act(GameAction::SelectTargets {
                                targets: vec![TargetRef::Object(target_land)],
                            })
                        })
                        .expect("target selection must be accepted");
                }
                // CR 603.3b (#531): drain ordering prompts with identity order.
                WaitingFor::OrderTriggers { triggers, .. } => {
                    let order: Vec<usize> = (0..triggers.len()).collect();
                    runner
                        .act(GameAction::OrderTriggers { order })
                        .expect("identity OrderTriggers must succeed");
                }
                _ => {
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                }
            }
        }
    }

    /// CR 603.4 + CR 702.33d: Casting Sowing Mycospawn KICKED must, on the
    /// second cast-trigger's resolution, exile the chosen target land. This
    /// drives the real cast pipeline (`apply`) — it is a pipeline test, not a
    /// shape test. Regression guard for issue #461.
    #[test]
    fn sowing_mycospawn_kicked_cast_trigger_exiles_target_land() {
        let (mut runner, spell_id, spell_card_id, target_land) = sowing_mycospawn_scenario();
        fund_sowing_mycospawn(&mut runner, true);

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id: spell_card_id,
                targets: vec![],
            })
            .expect("casting Sowing Mycospawn must be accepted");

        drive_sowing_mycospawn(&mut runner, true, target_land);

        assert_eq!(
            runner.state().objects[&target_land].zone,
            Zone::Exile,
            "kicked Sowing Mycospawn's second cast-trigger must exile the target land"
        );
    }

    /// CR 603.4: Casting Sowing Mycospawn UNKICKED — the second cast-trigger's
    /// intervening-'if' AdditionalCostPaid condition is false, so the land is
    /// never exiled. Negative control for issue #461.
    #[test]
    fn sowing_mycospawn_unkicked_cast_trigger_does_not_exile_land() {
        let (mut runner, spell_id, spell_card_id, target_land) = sowing_mycospawn_scenario();
        fund_sowing_mycospawn(&mut runner, false);

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id: spell_card_id,
                targets: vec![],
            })
            .expect("casting Sowing Mycospawn must be accepted");

        drive_sowing_mycospawn(&mut runner, false, target_land);

        assert_ne!(
            runner.state().objects[&target_land].zone,
            Zone::Exile,
            "unkicked Sowing Mycospawn must not exile the land (intervening-'if' false)"
        );
    }

    // -----------------------------------------------------------------------
    // Issue #494: Goblin Bushwhacker is a placeholder permanent spell (vanilla
    // creature with an ETB-only trigger and NO on-resolve Spell ability —
    // `abilities: []`). Its ETB trigger is gated by an intervening-'if'
    // `AdditionalCostPaid` condition. Because the resolving stack object has
    // `ability == None`, the kicker restore in `stack.rs` was previously
    // skipped — `move_to_zone` → `reset_for_battlefield_entry` had already
    // cleared `kickers_paid` per CR 400.7, leaving the trigger condition false
    // even when the spell was kicked. CR 400.7d permits the resulting
    // permanent's ability to reference costs paid to cast the spell.
    // -----------------------------------------------------------------------

    const GOBLIN_BUSHWHACKER_ORACLE: &str =
        "Kicker {R} (You may pay an additional {R} as you cast this spell.)\n\
        When this creature enters, if it was kicked, creatures you control get \
        +1/+0 and gain haste until end of turn.";

    /// Build a scenario with Goblin Bushwhacker ({R}, kicker {R}) in P0's hand
    /// and a pre-existing 2/2 vanilla creature P0 controls (the buff target).
    /// Returns the runner, the Bushwhacker spell `ObjectId`/`CardId`, and the
    /// vanilla creature's `ObjectId`.
    fn goblin_bushwhacker_scenario() -> (
        crate::game::scenario::GameRunner,
        ObjectId,
        CardId,
        ObjectId,
    ) {
        use crate::game::scenario::GameScenario;
        use crate::types::mana::ManaCostShard;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let bushwhacker_builder = scenario.add_creature_to_hand_from_oracle(
            PlayerId(0),
            "Goblin Bushwhacker",
            1,
            1,
            GOBLIN_BUSHWHACKER_ORACLE,
        );
        let spell_id = bushwhacker_builder.id();
        let spell_card_id = scenario.state.objects[&spell_id].card_id;
        scenario.state.objects.get_mut(&spell_id).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };

        // Pre-existing creature P0 controls — the buff target. A separate
        // object from Bushwhacker itself so the assertion proves the static
        // ability reaches other creatures, not just the source.
        let ally = scenario.add_vanilla(PlayerId(0), 2, 2);

        let runner = scenario.build();
        (runner, spell_id, spell_card_id, ally)
    }

    /// Add red mana to P0's pool: {R}{R} when `kicked` (mana cost + kicker),
    /// {R} otherwise.
    fn fund_goblin_bushwhacker(runner: &mut crate::game::scenario::GameRunner, kicked: bool) {
        let count = if kicked { 2 } else { 1 };
        let p0 = runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..count {
            p0.mana_pool.add(ManaUnit {
                color: ManaType::Red,
                source_id: ObjectId(0),
                snow: false,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    /// Drive every casting prompt to completion, paying the kicker per
    /// `kicked`. Returns once the stack is empty and P0 holds priority.
    fn drive_goblin_bushwhacker(runner: &mut crate::game::scenario::GameRunner, kicked: bool) {
        for _ in 0..60 {
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
                WaitingFor::OptionalCostChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalCost { pay: kicked })
                        .expect("kicker decision must be accepted");
                }
                _ => {
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                }
            }
        }
    }

    /// CR 702.33d + CR 400.7d: Casting Goblin Bushwhacker KICKED must, on the
    /// ETB trigger's resolution, grant +1/+0 and haste to creatures P0
    /// controls. Drives the real cast pipeline (`apply`) across the
    /// `move_to_zone → reset_for_battlefield_entry → stack.rs restore`
    /// boundary — `kickers_paid` is never hand-stamped. Regression guard for
    /// issue #494.
    #[test]
    fn goblin_bushwhacker_kicked_etb_buffs_creatures_you_control() {
        let (mut runner, spell_id, spell_card_id, ally) = goblin_bushwhacker_scenario();
        fund_goblin_bushwhacker(&mut runner, true);

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id: spell_card_id,
                targets: vec![],
            })
            .expect("casting Goblin Bushwhacker must be accepted");

        drive_goblin_bushwhacker(&mut runner, true);

        // The kicker payment must survive zone change onto the permanent — the
        // ETB trigger's intervening-'if' reads it.
        assert_eq!(
            runner.state().objects[&spell_id].kickers_paid,
            vec![KickerVariant::First],
            "kicked Bushwhacker's permanent must retain kickers_paid after resolution"
        );

        // The ETB trigger's static ability buffed the pre-existing ally.
        let ally_obj = &runner.state().objects[&ally];
        assert_eq!(
            ally_obj.power,
            Some(3),
            "kicked Bushwhacker must grant +1/+0 to creatures P0 controls (2/2 -> 3/2)"
        );
        assert!(
            ally_obj.has_keyword(&Keyword::Haste),
            "kicked Bushwhacker must grant haste to creatures P0 controls"
        );
    }

    /// Negative control: casting Goblin Bushwhacker UNKICKED — the ETB
    /// trigger's intervening-'if' `AdditionalCostPaid` condition is false, so
    /// no buff and no haste are granted.
    #[test]
    fn goblin_bushwhacker_unkicked_etb_grants_no_buff() {
        let (mut runner, spell_id, spell_card_id, ally) = goblin_bushwhacker_scenario();
        fund_goblin_bushwhacker(&mut runner, false);

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id: spell_card_id,
                targets: vec![],
            })
            .expect("casting Goblin Bushwhacker must be accepted");

        drive_goblin_bushwhacker(&mut runner, false);

        assert!(
            runner.state().objects[&spell_id].kickers_paid.is_empty(),
            "unkicked Bushwhacker must have no kicker payments"
        );

        let ally_obj = &runner.state().objects[&ally];
        assert_eq!(
            ally_obj.power,
            Some(2),
            "unkicked Bushwhacker must not buff creatures (ally stays 2/2)"
        );
        assert!(
            !ally_obj.has_keyword(&Keyword::Haste),
            "unkicked Bushwhacker must not grant haste (intervening-'if' false)"
        );
    }

    // -----------------------------------------------------------------------
    // Issue #423: dies-triggers (Undying, Blood Artist-class) must not be lost
    // when a creature is sacrificed inside a resolution-choice handler.
    // -----------------------------------------------------------------------

    /// Create an inert battlefield artifact to host a triggered ability in the
    /// #423 tests. Using a non-creature source keeps a reflexive `Destroy
    /// SelfRef` from producing a creature-dies event that would muddy
    /// observer-trigger assertions.
    fn make_artifact_source(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
        id
    }

    /// Install the synthesized Undying dies-trigger (CR 702.93a) onto a
    /// battlefield creature, mirroring `make_soulbond_creature`.
    fn make_undying_creature(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
        let id = make_creature(state, player, name, 2, 2);
        let triggers =
            crate::database::synthesis::KeywordTriggerInstaller::triggers_for(&Keyword::Undying);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.keywords.push(Keyword::Undying);
        obj.base_keywords.push(Keyword::Undying);
        for trigger in &triggers {
            obj.trigger_definitions.push(trigger.clone());
        }
        std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).extend(triggers);
        id
    }

    /// Build a Grist-style ability: `Effect::Sacrifice` of a creature you
    /// control (`count: 1`, routes through `EffectZoneChoice` when more than
    /// one creature is eligible) plus a reflexive `WhenYouDo` sub-ability
    /// carrying `reflexive_effect`. Pick a reflexive effect that resolves
    /// inline (e.g. `Destroy SelfRef`, B1) or one that itself raises a
    /// resolution choice (e.g. `Sacrifice` of an opponent's permanents, B2).
    fn sacrifice_then_when_you_do(reflexive_effect: Effect) -> AbilityDefinition {
        let reflexive = AbilityDefinition::new(AbilityKind::Spell, reflexive_effect)
            .condition(AbilityCondition::WhenYouDo);
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::Typed(
                    TypedFilter::default()
                        .with_type(TypeFilter::Creature)
                        .controller(ControllerRef::You),
                ),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        )
        .sub_ability(reflexive)
    }

    /// A reflexive `Destroy SelfRef` effect — resolves inline when resumed as a
    /// continuation (no target selection), so the sacrifice's B1 path is taken.
    fn reflexive_destroy_self() -> Effect {
        Effect::Destroy {
            target: TargetFilter::SelfRef,
            cant_regenerate: false,
        }
    }

    /// A reflexive `Sacrifice` of one creature an opponent controls. When the
    /// opponent controls more than one creature this raises a fresh
    /// `EffectZoneChoice` on resume — a deterministic B2 pause.
    fn reflexive_opponent_sacrifice() -> Effect {
        Effect::Sacrifice {
            target: TargetFilter::Typed(
                TypedFilter::default()
                    .with_type(TypeFilter::Creature)
                    .controller(ControllerRef::Opponent),
            ),
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        }
    }

    /// Push `ability` onto the stack as a triggered ability controlled by
    /// `controller`, then pass priority until the resolution pauses on a
    /// `WaitingFor` other than `Priority` (or the stack empties). Returns the
    /// collected events.
    fn resolve_stack_until_paused(state: &mut GameState) -> Vec<GameEvent> {
        let mut all = Vec::new();
        for _ in 0..30 {
            if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                return all;
            }
            if state.stack.is_empty() {
                return all;
            }
            let result = crate::game::engine::apply_as_current(state, GameAction::PassPriority)
                .expect("pass priority");
            all.extend(result.events);
        }
        panic!("stack did not settle: waiting_for={:?}", state.waiting_for);
    }

    /// CR 702.93a + issue #423 (4a, B1 baseline): an Undying creature with zero
    /// +1/+1 counters sacrificed inside the `EffectZoneChoice` resolution
    /// handler must still fire its dies-trigger and return to the battlefield
    /// with one +1/+1 counter. The reflexive `WhenYouDo Destroy` targets
    /// `SelfRef` and so resolves inline (B1: `waiting_for` stays `Priority`);
    /// `run_post_action_pipeline`'s standard trigger scan fires Undying. This
    /// is the happy-path regression guard — the discriminating B2 case (where
    /// the standard scan never runs) is `issue_423_co_triggered_targeted_
    /// observer_reaches_stack`.
    #[test]
    fn issue_423_undying_returns_when_sacrificed_in_resolution_choice() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // The ability source — its `Destroy SelfRef` reflexive will destroy
        // this object inline (B1).
        let source = make_artifact_source(&mut state, PlayerId(0), "Grist Stand-In");
        let young_wolf = make_undying_creature(&mut state, PlayerId(0), "Young Wolf");
        // A second sacrificeable creature so the Sacrifice does NOT hit the
        // mandatory-all fast-path and instead routes through EffectZoneChoice.
        let _decoy = make_creature(&mut state, PlayerId(0), "Decoy Bear", 2, 2);

        let ability = sacrifice_then_when_you_do(reflexive_destroy_self());
        let trigger = TriggerDefinition::new(TriggerMode::Phase).execute(ability);
        push_pending_trigger_to_stack(
            &mut state,
            PendingTrigger {
                source_id: source,
                controller: PlayerId(0),
                condition: trigger.condition,
                ability: crate::game::ability_utils::build_resolved_from_def(
                    trigger.execute.as_deref().unwrap(),
                    source,
                    PlayerId(0),
                ),
                timestamp: 0,
                target_constraints: Vec::new(),
                distribute: None,
                trigger_event: None,
                modal: None,
                mode_abilities: vec![],
                description: None,
                may_trigger_origin: None,
                subject_match_count: None,
            },
            &mut Vec::new(),
        );

        resolve_stack_until_paused(&mut state);
        assert!(
            matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "expected EffectZoneChoice for the sacrifice, got {:?}",
            state.waiting_for
        );

        // Player chooses to sacrifice the Undying creature.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![young_wolf],
            },
        )
        .expect("select Young Wolf to sacrifice");

        // The Undying trigger must have been collected and dispatched; resolve
        // whatever reached the stack.
        resolve_stack_until_paused(&mut state);

        let obj = state.objects.get(&young_wolf).expect("object tracked");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "Undying must return the sacrificed creature to the battlefield (CR 702.93a)"
        );
        let p1p1: u32 = obj
            .counters
            .iter()
            .filter(|(ct, _)| **ct == crate::types::counter::CounterType::Plus1Plus1)
            .map(|(_, n)| *n)
            .sum();
        assert_eq!(p1p1, 1, "Undying returns with exactly one +1/+1 counter");
    }

    /// CR 603.2c + CR 608.2e + issue #456: Syphon Mind ("Each other player
    /// discards a card. You draw a card for each card discarded this way.")
    /// resolving in a 4-player game while the controller has Waste Not on the
    /// battlefield. Each of the three opponents discards one noncreature,
    /// nonland card via an interactive `DiscardChoice`.
    ///
    /// Pre-fix this drew 1 (Waste Not fired once, Syphon Mind's TrackedSetSize
    /// read 0). Post-fix the controller must draw exactly 6: 3 from Syphon
    /// Mind's `Draw { Ref(TrackedSetSize) }` tail (all three discards
    /// accumulate into one chain tracked set across the continuation pauses)
    /// plus 3 from Waste Not's `Discarded` trigger firing once per opponent.
    #[test]
    fn syphon_mind_with_waste_not_four_player_draws_six() {
        let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 4, 99);
        state.phase = Phase::PreCombatMain;
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // P0 controls a Waste Not stand-in: a battlefield permanent carrying
        // the noncreature-nonland `Discarded` trigger ("Whenever an opponent
        // discards a noncreature, nonland card, draw a card.") — the parsed
        // AST is `valid_card: Typed{[Card], controller: Opponent}`,
        // `execute: Draw{1}`.
        let waste_not = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Waste Not".to_string(),
            Zone::Battlefield,
        );
        {
            let trigger = TriggerDefinition::new(TriggerMode::Discarded)
                .valid_card(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Card).controller(ControllerRef::Opponent),
                ))
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ))
                .description(
                    "Whenever an opponent discards a noncreature, nonland card, draw a card."
                        .to_string(),
                );
            let obj = state.objects.get_mut(&waste_not).unwrap();
            obj.trigger_definitions.push(trigger.clone());
            std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger);
        }

        // Each of the three opponents holds two noncreature, nonland cards so
        // every discard routes through an interactive `DiscardChoice`.
        for opp in 1..4u8 {
            for c in 0..2u64 {
                create_object(
                    &mut state,
                    CardId(u64::from(opp) * 100 + c),
                    PlayerId(opp),
                    format!("P{opp} Spell {c}"),
                    Zone::Hand,
                );
            }
        }
        // P0's library must hold at least 6 cards for the draws to land.
        for i in 0..10u64 {
            create_object(
                &mut state,
                CardId(900 + i),
                PlayerId(0),
                format!("P0 Lib {i}"),
                Zone::Library,
            );
        }

        // Syphon Mind on the stack — exactly the parsed AST: a `player_scope:
        // Opponent` `Discard{1}` with a `Draw { Ref(TrackedSetSize) }` tail.
        let syphon_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Syphon Mind".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&syphon_id).unwrap().zone = Zone::Stack;
        let mut syphon = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                random: false,
                unless_filter: None,
                filter: None,
            },
            vec![],
            syphon_id,
            PlayerId(0),
        );
        syphon.player_scope = Some(PlayerFilter::Opponent);
        syphon.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            syphon_id,
            PlayerId(0),
        )));
        state.stack.push_back(StackEntry {
            id: syphon_id,
            source_id: syphon_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(2),
                ability: Some(syphon),
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let p0_hand_before = state.players[0].hand.len();

        // Resolve Syphon Mind off the stack — pauses on opponent 1's discard.
        resolve_stack_until_paused(&mut state);

        // Drive each opponent's interactive `DiscardChoice` through the real
        // `apply` pipeline (so the final settle runs `run_post_action_pipeline`).
        let mut discards = 0;
        for _ in 0..60 {
            match &state.waiting_for {
                WaitingFor::DiscardChoice { cards, .. } => {
                    let pick = vec![cards[0]];
                    crate::game::engine::apply_as_current(
                        &mut state,
                        GameAction::SelectCards { cards: pick },
                    )
                    .expect("opponent discards a card");
                    discards += 1;
                }
                WaitingFor::Priority { .. } if state.stack.is_empty() => break,
                WaitingFor::Priority { .. } => {
                    crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                        .expect("pass priority to resolve Waste Not triggers");
                }
                other => panic!("unexpected waiting_for during discard: {other:?}"),
            }
        }

        assert_eq!(discards, 3, "all three opponents discard exactly once");

        // CR 603.2c: Waste Not triggered once per opponent discard → 3 draws.
        // CR 608.2e: Syphon Mind's TrackedSetSize == 3 → 3 more draws.
        let drawn = state.players[0].hand.len() - p0_hand_before;
        assert_eq!(
            drawn, 6,
            "controller must draw exactly 6 (3 Syphon Mind for-each + 3 Waste Not), got {drawn}"
        );
    }

    /// CR 702.93a + issue #423 (4b): the negative path — an Undying creature
    /// that is sacrificed WITH a +1/+1 counter already on it does NOT return;
    /// the `Not(HadCounters)` intervening-if gates the trigger out.
    #[test]
    fn issue_423_undying_does_not_return_when_sacrificed_with_counter() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source = make_artifact_source(&mut state, PlayerId(0), "Grist Stand-In");
        let strangleroot = make_undying_creature(&mut state, PlayerId(0), "Strangleroot Geist");
        state
            .objects
            .get_mut(&strangleroot)
            .unwrap()
            .counters
            .insert(crate::types::counter::CounterType::Plus1Plus1, 1);
        let _decoy = make_creature(&mut state, PlayerId(0), "Decoy Bear", 2, 2);

        let ability = sacrifice_then_when_you_do(reflexive_destroy_self());
        let trigger = TriggerDefinition::new(TriggerMode::Phase).execute(ability);
        push_pending_trigger_to_stack(
            &mut state,
            PendingTrigger {
                source_id: source,
                controller: PlayerId(0),
                condition: trigger.condition,
                ability: crate::game::ability_utils::build_resolved_from_def(
                    trigger.execute.as_deref().unwrap(),
                    source,
                    PlayerId(0),
                ),
                timestamp: 0,
                target_constraints: Vec::new(),
                distribute: None,
                trigger_event: None,
                modal: None,
                mode_abilities: vec![],
                description: None,
                may_trigger_origin: None,
                subject_match_count: None,
            },
            &mut Vec::new(),
        );

        resolve_stack_until_paused(&mut state);
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![strangleroot],
            },
        )
        .expect("select Strangleroot Geist to sacrifice");
        resolve_stack_until_paused(&mut state);

        let obj = state.objects.get(&strangleroot).expect("object tracked");
        assert_eq!(
            obj.zone,
            Zone::Graveyard,
            "Undying must NOT return a creature sacrificed with a +1/+1 counter (CR 702.93a)"
        );
    }

    /// CR 603.2 + CR 603.3b + issue #423 (4c, the blocker case): an Undying
    /// creature is sacrificed in the `EffectZoneChoice` handler whose reflexive
    /// `WhenYouDo` continuation is itself an `Effect::Sacrifice` that raises a
    /// second `EffectZoneChoice` (B2 — the action ends `!= Priority`, so
    /// `run_post_action_pipeline`'s trigger scan never runs). A co-triggered
    /// TARGETED dies-observer is on the battlefield. The handler must batch
    /// BOTH dies-triggers into `deferred_triggers`; the next handler to settle
    /// to `Priority` then flushes them. This is the case that fails pre-fix:
    /// the sacrifice triggers were stranded when the reflexive paused.
    #[test]
    fn issue_423_co_triggered_targeted_observer_reaches_stack() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source = make_artifact_source(&mut state, PlayerId(0), "Grist Stand-In");
        let young_wolf = make_undying_creature(&mut state, PlayerId(0), "Young Wolf");
        let _decoy = make_creature(&mut state, PlayerId(0), "Decoy Bear", 2, 2);

        // Two opponent creatures: the reflexive `Sacrifice` of an opponent
        // creature routes through `EffectZoneChoice` (2 eligible > 1), and they
        // are also the legal targets for the observer's `Destroy`.
        let observer = make_creature(&mut state, PlayerId(0), "Grim Observer", 1, 1);
        let opp_a = make_creature(&mut state, PlayerId(1), "Opp Bear A", 2, 2);
        let opp_b = make_creature(&mut state, PlayerId(1), "Opp Bear B", 2, 2);

        // A dies-observer with a TARGETED trigger: "whenever a creature dies,
        // tap target creature." Targets ANY creature so that — with the decoy,
        // the observer itself, and a surviving opponent creature still on the
        // battlefield — there are always at least two legal targets, forcing a
        // `TriggerTargetSelection` pause when the observer is drained from the
        // deferred queue. `Tap` (not `Destroy`) keeps the observer from
        // recursively re-triggering on the creatures it affects.
        let observer_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .origin(Zone::Battlefield)
            .destination(Zone::Graveyard)
            .valid_card(TargetFilter::Typed(
                TypedFilter::default().with_type(TypeFilter::Creature),
            ))
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Tap {
                    target: TargetFilter::Typed(
                        TypedFilter::default().with_type(TypeFilter::Creature),
                    ),
                },
            ));
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.trigger_definitions.push(observer_trigger.clone());
            std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(observer_trigger);
        }

        // The reflexive `WhenYouDo` continuation is an opponent `Sacrifice`,
        // which raises a fresh `EffectZoneChoice` (B2 pause) on resume.
        let ability = sacrifice_then_when_you_do(reflexive_opponent_sacrifice());
        let trigger = TriggerDefinition::new(TriggerMode::Phase).execute(ability);
        push_pending_trigger_to_stack(
            &mut state,
            PendingTrigger {
                source_id: source,
                controller: PlayerId(0),
                condition: trigger.condition,
                ability: crate::game::ability_utils::build_resolved_from_def(
                    trigger.execute.as_deref().unwrap(),
                    source,
                    PlayerId(0),
                ),
                timestamp: 0,
                target_constraints: Vec::new(),
                distribute: None,
                trigger_event: None,
                modal: None,
                mode_abilities: vec![],
                description: None,
                may_trigger_origin: None,
                subject_match_count: None,
            },
            &mut Vec::new(),
        );

        resolve_stack_until_paused(&mut state);
        // First EffectZoneChoice — sacrifice the Undying creature.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![young_wolf],
            },
        )
        .expect("select Young Wolf to sacrifice");

        // (ii) The reflexive `WhenYouDo` continuation was NOT dropped — it
        // resolved into a second `EffectZoneChoice` (the opponent sacrifice).
        assert!(
            matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "the reflexive opponent Sacrifice must raise an EffectZoneChoice, got {:?}",
            state.waiting_for
        );

        // The Undying dies-trigger and the co-triggered observer trigger were
        // batched into the deferred queue — they would be lost pre-fix.
        assert_eq!(
            state.deferred_triggers.len(),
            2,
            "the Undying dies-trigger and the targeted observer trigger must \
             both be batched into deferred_triggers (issue #423)"
        );

        // Resolve the reflexive opponent sacrifice → the handler settles to
        // `Priority` and drains the deferred queue.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards { cards: vec![opp_a] },
        )
        .expect("opponent sacrifices a creature");

        // (iii) The drained targeted observer reached its own target selection.
        assert!(
            matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }),
            "the targeted dies-observer must reach TriggerTargetSelection after the \
             deferred flush, got {:?}",
            state.waiting_for
        );
        // (i) The Undying dies-trigger reached the stack.
        assert!(
            state
                .stack
                .iter()
                .any(|e| matches!(&e.kind, StackEntryKind::TriggeredAbility { .. })),
            "the Undying dies-trigger must have reached the stack via the deferred flush"
        );

        // Pick the observer's target, then resolve everything. Undying returns.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(opp_b)),
            },
        )
        .expect("choose observer Tap target");
        resolve_stack_until_paused(&mut state);

        let wolf = state.objects.get(&young_wolf).expect("wolf tracked");
        assert_eq!(
            wolf.zone,
            Zone::Battlefield,
            "Undying returned the sacrificed creature despite the co-triggered observer"
        );
    }

    /// CR 701.21a + CR 702.93a + issue #423 (4d, Correction 1): a creature
    /// sacrificed through the `ChooseAndSacrificeRest` / `CategoryChoice`
    /// resolution handler (Diabolic Edict-class) must still fire its
    /// dies-trigger. An Undying creature sacrificed this way returns.
    #[test]
    fn issue_423_choose_and_sacrifice_rest_fires_dies_trigger() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source = make_artifact_source(&mut state, PlayerId(0), "Edict Stand-In");
        let young_wolf = make_undying_creature(&mut state, PlayerId(0), "Young Wolf");
        // A creature to keep, so player 0's choice is non-trivial (prompts a
        // `CategoryChoice` rather than auto-resolving). Player 1 has no
        // creatures, so only player 0 is asked.
        let keeper = make_creature(&mut state, PlayerId(0), "Keeper Bear", 2, 2);

        // ChooseAndSacrificeRest: each player keeps one creature, sacrifices the
        // rest. Player 0 keeps the keeper → Young Wolf is sacrificed via the
        // `CategoryChoice` resolution handler (issue #423 Correction 1: the
        // reworked handler must still fire the resulting dies-trigger).
        let ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseAndSacrificeRest {
                categories: vec![CoreType::Creature],
                chooser_scope: crate::types::ability::CategoryChooserScope::EachPlayerSelf,
                choose_filter: crate::types::ability::TargetFilter::Typed(
                    crate::types::ability::TypedFilter::permanent(),
                ),
                sacrifice_filter: crate::types::ability::TargetFilter::Typed(
                    crate::types::ability::TypedFilter::permanent(),
                ),
            },
        );
        let trigger = TriggerDefinition::new(TriggerMode::Phase).execute(ability);
        push_pending_trigger_to_stack(
            &mut state,
            PendingTrigger {
                source_id: source,
                controller: PlayerId(0),
                condition: trigger.condition,
                ability: crate::game::ability_utils::build_resolved_from_def(
                    trigger.execute.as_deref().unwrap(),
                    source,
                    PlayerId(0),
                ),
                timestamp: 0,
                target_constraints: Vec::new(),
                distribute: None,
                trigger_event: None,
                modal: None,
                mode_abilities: vec![],
                description: None,
                may_trigger_origin: None,
                subject_match_count: None,
            },
            &mut Vec::new(),
        );

        resolve_stack_until_paused(&mut state);
        assert!(
            matches!(state.waiting_for, WaitingFor::CategoryChoice { .. }),
            "expected CategoryChoice, got {:?}",
            state.waiting_for
        );

        // Keep the keeper; Young Wolf is left unchosen → sacrificed.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCategoryPermanents {
                choices: vec![Some(keeper)],
            },
        )
        .expect("keep the keeper");
        resolve_stack_until_paused(&mut state);

        let wolf = state.objects.get(&young_wolf).expect("wolf tracked");
        assert_eq!(
            wolf.zone,
            Zone::Battlefield,
            "Undying must fire when the creature is sacrificed via ChooseAndSacrificeRest \
             (issue #423 Correction 1)"
        );
    }

    /// CR 603.2 + issue #423 (Step 5, class-level): a resolution-choice
    /// `Effect::Sacrifice` of a creature with NO Undying still fires a generic
    /// "whenever a creature dies" observer (Blood Artist-class) — the observer
    /// reaches the stack via the `deferred_triggers` flush.
    #[test]
    fn issue_423_generic_dies_observer_fires_from_resolution_choice() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.players[0].life = 20;

        let source = make_artifact_source(&mut state, PlayerId(0), "Grist Stand-In");
        let victim = make_creature(&mut state, PlayerId(0), "Plain Bear", 2, 2);
        let _decoy = make_creature(&mut state, PlayerId(0), "Decoy Bear", 2, 2);

        // Blood Artist-class observer: whenever a creature dies, its
        // controller gains 1 life.
        let observer = make_creature(&mut state, PlayerId(0), "Blood Artist Stand-In", 0, 1);
        let observer_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .origin(Zone::Battlefield)
            .destination(Zone::Graveyard)
            .valid_card(TargetFilter::Typed(
                TypedFilter::default().with_type(TypeFilter::Creature),
            ))
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: GainLifePlayer::Controller,
                },
            ));
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.trigger_definitions.push(observer_trigger.clone());
            std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(observer_trigger);
        }

        let ability = sacrifice_then_when_you_do(reflexive_destroy_self());
        let trigger = TriggerDefinition::new(TriggerMode::Phase).execute(ability);
        push_pending_trigger_to_stack(
            &mut state,
            PendingTrigger {
                source_id: source,
                controller: PlayerId(0),
                condition: trigger.condition,
                ability: crate::game::ability_utils::build_resolved_from_def(
                    trigger.execute.as_deref().unwrap(),
                    source,
                    PlayerId(0),
                ),
                timestamp: 0,
                target_constraints: Vec::new(),
                distribute: None,
                trigger_event: None,
                modal: None,
                mode_abilities: vec![],
                description: None,
                may_trigger_origin: None,
                subject_match_count: None,
            },
            &mut Vec::new(),
        );

        resolve_stack_until_paused(&mut state);
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![victim],
            },
        )
        .expect("select Plain Bear to sacrifice");
        resolve_stack_until_paused(&mut state);

        assert_eq!(
            state.players[0].life, 21,
            "the Blood Artist-class dies-observer must resolve from the \
             deferred-trigger flush (issue #423)"
        );
    }

    /// A "whenever a creature dies" observer (Blood Artist class) for tests.
    fn add_dies_observer(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let observer = make_creature(state, owner, "Blood Artist Stand-In", 0, 1);
        let observer_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .origin(Zone::Battlefield)
            .destination(Zone::Graveyard)
            .valid_card(TargetFilter::Typed(
                TypedFilter::default().with_type(TypeFilter::Creature),
            ))
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: GainLifePlayer::Controller,
                },
            ));
        let obj = state.objects.get_mut(&observer).unwrap();
        obj.trigger_definitions.push(observer_trigger.clone());
        std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(observer_trigger);
        observer
    }

    /// CR 603.10a: a dies-observer (Blood Artist) that dies in the SAME
    /// simultaneous event as other creatures triggers once per creature that
    /// died, including itself. This drives the real producer authority — a
    /// single state-based-action check destroying every creature with lethal
    /// damage at once (CR 704.7) — which stamps the simultaneity group onto each
    /// `ZoneChangeRecord.co_departed`. Before the fix the observer fired only for
    /// its own departure and missed the co-dying creatures.
    #[test]
    fn dies_observer_killed_in_same_sba_batch_fires_for_each_simultaneous_death() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let observer = add_dies_observer(&mut state, PlayerId(0));
        let bear_a = make_creature(&mut state, PlayerId(0), "Bear A", 2, 2);
        let bear_b = make_creature(&mut state, PlayerId(1), "Bear B", 2, 2);

        // Lethal damage marked on all three (as a board sweeper like Pyroclasm
        // would) so one SBA check destroys them simultaneously.
        state.objects.get_mut(&observer).unwrap().damage_marked = 1;
        state.objects.get_mut(&bear_a).unwrap().damage_marked = 2;
        state.objects.get_mut(&bear_b).unwrap().damage_marked = 2;

        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);

        let pending = collect_pending_triggers(&mut state, &events);
        let observer_fires = pending
            .iter()
            .filter(|p| p.pending.source_id == observer)
            .count();
        assert_eq!(
            observer_fires, 3,
            "dies-observer must fire once per creature that died simultaneously \
             (itself + 2 others)"
        );
    }

    /// CR 603.10a regression guard (PR #1449 review): a dies-observer that leaves
    /// the battlefield in one instruction must NOT observe a creature that leaves
    /// in a SEPARATE, sequential instruction of the same resolution. Simultaneity
    /// is established by the producer (`co_departed`), not by two ZoneChanged
    /// events happening to share the accumulated event vector — so without a
    /// producer grouping them, the observer fires only for its own death.
    #[test]
    fn dies_observer_does_not_observe_sequential_departure() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let observer = add_dies_observer(&mut state, PlayerId(0));
        let later = make_creature(&mut state, PlayerId(0), "Later Bear", 2, 2);

        // Two separate, sequential departures (e.g. "sacrifice ~, then destroy
        // target creature"): no producer marks them simultaneous, so co_departed
        // stays empty on both events.
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, observer, Zone::Graveyard, &mut events);
        crate::game::zones::move_to_zone(&mut state, later, Zone::Graveyard, &mut events);

        let pending = collect_pending_triggers(&mut state, &events);
        let observer_fires = pending
            .iter()
            .filter(|p| p.pending.source_id == observer)
            .count();
        assert_eq!(
            observer_fires, 1,
            "observer that left earlier must NOT observe a later, non-simultaneous \
             departure — only its own death fires"
        );
    }

    /// CR 603.10a: a generic "whenever a permanent you control leaves the
    /// battlefield" observer (Blood Artist / Elas il-Kor class). Matches a
    /// battlefield departure to ANY destination (no destination filter), so the
    /// same observer covers bounce (to hand), exile, and graveyard alike.
    fn add_ltb_observer(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let observer = make_creature(state, owner, "LTB Observer Stand-In", 0, 1);
        let observer_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .origin(Zone::Battlefield)
            .valid_card(TargetFilter::Typed(
                TypedFilter::default().with_type(TypeFilter::Creature),
            ))
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: GainLifePlayer::Controller,
                },
            ));
        let obj = state.objects.get_mut(&observer).unwrap();
        obj.trigger_definitions.push(observer_trigger.clone());
        std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(observer_trigger);
        observer
    }

    /// Count how many times `observer`'s trigger fired against `events`.
    fn observer_fire_count(
        state: &mut GameState,
        events: &[GameEvent],
        observer: ObjectId,
    ) -> usize {
        collect_pending_triggers(state, events)
            .iter()
            .filter(|p| p.pending.source_id == observer)
            .count()
    }

    /// CR 603.10a (STEP 1): `bounce::resolve_all` with no count clause. A
    /// leaves-the-battlefield observer among the bounced group observes every
    /// co-bounced creature (itself + the others). FAILS without the STEP 1 stamp.
    #[test]
    fn ltb_observer_fires_for_each_co_bounced_creature() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let observer = add_ltb_observer(&mut state, PlayerId(0));
        let _b1 = make_creature(&mut state, PlayerId(0), "Bear 1", 2, 2);
        let _b2 = make_creature(&mut state, PlayerId(1), "Bear 2", 2, 2);

        let ability = ResolvedAbility::new(
            Effect::BounceAll {
                target: TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature)),
                destination: Some(Zone::Hand),
                count: None,
            },
            Vec::new(),
            ObjectId(9001),
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::bounce::resolve_all(&mut state, &ability, &mut events)
            .expect("mass bounce resolves");

        assert_eq!(
            observer_fire_count(&mut state, &events, observer),
            3,
            "LTB observer must fire once per co-bounced creature (itself + 2 others)"
        );
    }

    /// CR 603.10a (STEP 2): `change_zone::resolve_all` exiling all creatures.
    /// The LTB observer among the exiled group observes every co-exiled creature.
    /// FAILS without the STEP 2 stamp.
    #[test]
    fn ltb_observer_fires_for_each_co_exiled_creature() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let observer = add_ltb_observer(&mut state, PlayerId(0));
        let _b1 = make_creature(&mut state, PlayerId(0), "Bear 1", 2, 2);
        let _b2 = make_creature(&mut state, PlayerId(1), "Bear 2", 2, 2);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature)),
                enter_tapped: false,
            },
            Vec::new(),
            ObjectId(9002),
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::change_zone::resolve_all(&mut state, &ability, &mut events)
            .expect("mass exile resolves");

        assert_eq!(
            observer_fire_count(&mut state, &events, observer),
            3,
            "LTB observer must fire once per co-exiled creature (itself + 2 others)"
        );
    }

    /// CR 603.10a + CR 701.19a/b (STEP 3): `destroy::resolve_all` (DestroyAll)
    /// with a regeneration shield on one non-observer creature. The shielded
    /// creature stays on the battlefield, so the observer fires N-1 times (once
    /// per creature that actually died, including itself) and the regenerated
    /// creature is excluded from `co_departed`. FAILS without the
    /// `departed_subset` precision filter at the STEP 3 stamp.
    #[test]
    fn ltb_observer_skips_regenerated_creature_in_destroy_all() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        let observer = add_ltb_observer(&mut state, PlayerId(0));
        let _doomed = make_creature(&mut state, PlayerId(1), "Doomed Bear", 2, 2);

        // A creature with a regeneration shield survives the board wipe.
        let regen = make_creature(&mut state, PlayerId(0), "Regen Bear", 2, 2);
        {
            let shield = crate::types::ability::ReplacementDefinition::new(
                crate::types::replacements::ReplacementEvent::Destroy,
            )
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
            state
                .objects
                .get_mut(&regen)
                .unwrap()
                .replacement_definitions
                .push(shield);
        }

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature)),
                cant_regenerate: false,
            },
            Vec::new(),
            ObjectId(9003),
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::destroy::resolve_all(&mut state, &ability, &mut events)
            .expect("destroy all resolves");

        // observer + doomed died (2); regen stayed on the battlefield.
        assert_eq!(
            observer_fire_count(&mut state, &events, observer),
            2,
            "LTB observer fires once per creature that actually died — the \
             regenerated creature is excluded from co_departed"
        );
        assert_eq!(
            state.objects.get(&regen).unwrap().zone,
            Zone::Battlefield,
            "regenerated creature must remain on the battlefield"
        );
        // The regenerated creature's own departure event must not exist, so its
        // own LTB-style observation cannot fire (it never left).
        let regen_departed = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), .. }
                    if *object_id == regen
            )
        });
        assert!(
            !regen_departed,
            "regenerated creature must not have a battlefield-departure event"
        );
        // And it must not appear in any survivor's co_departed group.
        let regen_in_codeparted = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::ZoneChanged { record, .. } if record.co_departed.contains(&regen)
            )
        });
        assert!(
            !regen_in_codeparted,
            "regenerated creature must not appear in any co_departed group"
        );
    }

    /// CR 701.21a + CR 603.10a (STEP 4): mandatory "each player sacrifices all
    /// creatures" fast path. The LTB observer among the sacrificed group observes
    /// every co-sacrificed creature; a CantBeSacrificed creature is excluded.
    /// FAILS without the STEP 4 stamp.
    #[test]
    fn ltb_observer_fires_for_each_co_sacrificed_creature() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let observer = add_ltb_observer(&mut state, PlayerId(0));
        // The mandatory-all sacrifice fast path scopes the eligible pool to the
        // controller's own creatures (no controller-ref filter => "you sacrifice"
        // default per CR 701.21a), so keep all members under PlayerId(0).
        let _b1 = make_creature(&mut state, PlayerId(0), "Bear 1", 2, 2);
        let _b2 = make_creature(&mut state, PlayerId(0), "Bear 2", 2, 2);

        // A CantBeSacrificed creature must be excluded from the sacrificed group.
        // `.affected(SelfRef)` scopes the prohibition to this object only (an
        // unscoped `affected: None` would make it global and block all sacrifices).
        let protected = make_creature(&mut state, PlayerId(0), "Sigarda Stand-In", 2, 2);
        {
            let def = crate::types::ability::StaticDefinition::new(
                crate::types::statics::StaticMode::Other("CantBeSacrificed".to_string()),
            )
            .affected(TargetFilter::SelfRef);
            state
                .objects
                .get_mut(&protected)
                .unwrap()
                .static_definitions
                .push(def);
        }

        // "Each player sacrifices all creatures": count huge so the mandatory-all
        // fast path runs (eligible.len() <= count).
        let ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature)),
                count: QuantityExpr::Fixed { value: 99 },
                min_count: 0,
            },
            Vec::new(),
            ObjectId(9004),
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::sacrifice::resolve(&mut state, &ability, &mut events)
            .expect("mandatory-all sacrifice resolves");

        // observer + 2 bears sacrificed (3); the protected creature is excluded.
        assert_eq!(
            observer_fire_count(&mut state, &events, observer),
            3,
            "LTB observer fires once per co-sacrificed creature (itself + 2 others)"
        );
        assert_eq!(
            state.objects.get(&protected).unwrap().zone,
            Zone::Battlefield,
            "CantBeSacrificed creature must not be sacrificed"
        );
    }

    /// CR 603.10a (STEP 4a): a Blood Artist-class LTB observer among a chosen
    /// `EffectZoneChoice` sacrifice group observes every co-sacrificed creature.
    /// Driven through `apply(SelectCards)` (the real resolution-choice handler)
    /// so the observer's GainLife trigger reaches the stack and resolves; the
    /// observed count is read from the controller's life total. FAILS without the
    /// STEP 4a sub-slice stamp.
    #[test]
    fn ltb_observer_fires_per_co_sacrificed_in_effect_zone_choice() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.players[0].life = 20;

        let source = make_artifact_source(&mut state, PlayerId(0), "Sacrifice Source");
        let observer = add_ltb_observer(&mut state, PlayerId(0));
        let b1 = make_creature(&mut state, PlayerId(0), "Bear 1", 2, 2);
        let b2 = make_creature(&mut state, PlayerId(0), "Bear 2", 2, 2);

        // Sacrifice exactly these three (count == pool) via the interactive
        // selection handler.
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![observer, b1, b2],
            count: 3,
            min_count: 3,
            up_to: false,
            source_id: source,
            effect_kind: crate::types::ability::EffectKind::Sacrifice,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: false,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            count_param: 0,
        };

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![observer, b1, b2],
            },
        )
        .expect("select all three to sacrifice");
        // The three co-departed observer triggers (same controller) require an
        // explicit ordering; drain the prompt with identity order, then resolve.
        drain_order_triggers_with_identity(&mut state);
        resolve_stack_until_paused(&mut state);

        // The LTB observer gains 1 life once per co-sacrificed creature
        // (itself + 2 others = 3).
        assert_eq!(
            state.players[0].life, 23,
            "LTB observer must fire once per co-sacrificed creature in the \
             EffectZoneChoice handler (20 + 3 = 23)"
        );
    }

    /// CR 603.10a (STEP 6): a Blood Artist-class LTB observer among the
    /// keep-one-sacrifice-rest group (Cataclysm / Tragic Arrogance) observes
    /// every co-sacrificed permanent. Driven through
    /// `apply(SelectCategoryPermanents)` keeping a non-observer, so the observer
    /// (and the other unkept creature) are sacrificed together. FAILS without the
    /// STEP 6a handler-slice stamp.
    #[test]
    fn ltb_observer_fires_per_co_sacrificed_in_choose_and_sacrifice_rest() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.players[0].life = 20;

        let source = make_artifact_source(&mut state, PlayerId(0), "Cataclysm Source");
        let observer = add_ltb_observer(&mut state, PlayerId(0));
        let keeper = make_creature(&mut state, PlayerId(0), "Keeper Bear", 2, 2);
        let other = make_creature(&mut state, PlayerId(0), "Other Bear", 2, 2);

        // Single Creature category; eligible pool has all three. Keeping `keeper`
        // sacrifices observer + other together as one event.
        state.waiting_for = WaitingFor::CategoryChoice {
            player: PlayerId(0),
            target_player: PlayerId(0),
            categories: vec![CoreType::Creature],
            chooser_scope: crate::types::ability::CategoryChooserScope::EachPlayerSelf,
            choose_filter: crate::types::ability::TargetFilter::Typed(
                crate::types::ability::TypedFilter::permanent(),
            ),
            sacrifice_filter: crate::types::ability::TargetFilter::Typed(
                crate::types::ability::TypedFilter::permanent(),
            ),
            source_controller: PlayerId(0),
            eligible_per_category: vec![vec![observer, keeper, other]],
            source_id: source,
            remaining_players: vec![],
            all_kept: vec![],
            scoped_players: vec![PlayerId(0)],
        };

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCategoryPermanents {
                choices: vec![Some(keeper)],
            },
        )
        .expect("keep the keeper, sacrifice the rest");
        // observer + other co-depart => two same-controller triggers need ordering.
        drain_order_triggers_with_identity(&mut state);
        resolve_stack_until_paused(&mut state);

        // observer + other are co-sacrificed; the observer fires once per
        // co-departed permanent (itself + other = 2), so 20 + 2 = 22.
        assert_eq!(
            state.players[0].life, 22,
            "LTB observer must fire once per co-sacrificed permanent in \
             ChooseAndSacrificeRest (20 + 2 = 22)"
        );
        assert_eq!(
            state.objects.get(&keeper).unwrap().zone,
            Zone::Battlefield,
            "the kept creature must remain on the battlefield"
        );
    }

    /// CR 603.10a (DEFERRED cross-pause residual): when a mass
    /// `ChangeZone` battlefield→hand batch pauses mid-batch on a per-permanent
    /// `MayCost { Moved }` replacement choice, the pre-pause-moved members and
    /// the post-pause-moved members are stamped as separate co-departed groups
    /// (one `mark_simultaneous_departures` call per segment slice). An LTB
    /// observer that left in the pre-pause segment therefore observes only its
    /// own-segment co-departers, NOT the members that left after the pause —
    /// because the pre-pause `ZoneChanged` events were already emitted in an
    /// earlier `apply_action` (and its co-departed observer already collected
    /// against the partial group), and the complete group is unknowable until
    /// settle, when those events are gone.
    ///
    /// This is the SAME cross-action consumption gap as the Unit A
    /// kicker-paused sub-case. This test drives the real cross-pause topology
    /// and asserts the CURRENT (wrong) outcome so it flips into a regression
    /// sentinel once the seam lands.
    ///
    /// # Unit B redesign sketch (next attempt starts here)
    ///
    /// 1. **Carrier**: add `accumulated_departures: Vec<GameEvent>`
    ///    (`#[serde(default, skip_serializing_if = "Vec::is_empty")]`) to
    ///    `PendingChangeZoneIteration`. Seed/extend it at all three constructor
    ///    sites (`change_zone.rs`, `engine_resolution_choices.rs`,
    ///    `effects/mod.rs`) with this segment's battlefield-origin `ZoneChanged`
    ///    events; add it to the destructure and the serde roundtrip test.
    ///    (Full events, not just IDs — `collect_matching_triggers` needs concrete
    ///    events to run the `ChangesZone` matcher per co-departer at settle.)
    /// 2. **Suppress co-departed collection per-segment**: change the per-segment
    ///    `collect_triggers_into_deferred` calls to a co-departed-SUPPRESSED
    ///    variant (`collect_pending_triggers_excluding_co_departed`, or a
    ///    `CollectScope` parameter), preserving the issue-#423 dies/ETB collection
    ///    while leaving co-departed observers for the settle pass.
    /// 3. **Settle-only complete-group collection** (in
    ///    `drain_pending_change_zone_iteration`'s loop-completed branch, when
    ///    `paused == false && waiting_for == Priority`): append the final
    ///    segment's departures, `mark_simultaneous_departures` over
    ///    `accumulated_departures` against the COMPLETE group, then a new
    ///    `triggers::collect_co_departed_observers_only` runs ONLY the
    ///    co-departed block over the accumulated events, pushing observer
    ///    `PendingTriggerContext`s into `state.deferred_triggers` exactly once.
    ///    The existing `drain_deferred_trigger_queue` then dispatches both.
    /// 4. **Apply the SAME seam to Unit A's kicker-paused sub-case**: route the
    ///    cost-sacrifice events through this accumulated-departures + settle
    ///    collection seam when the cast pauses before Priority.
    /// 5. **Verify against the CR 603.2 differential invariant** and the
    ///    Scute-Swarm throughput benchmark after the `collect_pending_triggers`
    ///    fork; verify issue-#423 and all shipped co-departed tests still pass.
    #[test]
    #[ignore = "DEFERRED: cross-pause co-departed observation requires accumulating \
                the per-segment ZoneChanged departure events on PendingChangeZoneIteration \
                and a settle-only co-departed-observer collection pass (forking \
                collect_pending_triggers to suppress the co-departed block per-segment). \
                Multi-day; touches the CR 603.2 differential-scan invariant + issue-#423 \
                deferred-queue contract. See this test's redesign sketch / plan Unit B."]
    fn ltb_observer_cross_pause_co_departed_deferred() {
        use crate::types::ability::{ReplacementDefinition, ReplacementMode};
        use crate::types::replacements::ReplacementEvent;

        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.players[0].life = 20;

        // Three P0 battlefield permanents: the LTB observer (GainLife 1 on a
        // creature leaving the battlefield) plus two members. The observer and
        // member_b each carry an interactive `Optional { Moved }` replacement (the
        // issue-#535 "may" pattern on a battlefield permanent — no life cost, so
        // the player's life total reflects ONLY the observer's GainLife fires) so
        // the battlefield→hand mass move pauses on each, splitting the
        // co-departing group across pause segments.
        let observer = add_ltb_observer(&mut state, PlayerId(0));
        let member_a = make_creature(&mut state, PlayerId(0), "Member A", 2, 2);
        let member_b = make_creature(&mut state, PlayerId(0), "Member B", 2, 2);

        for id in [observer, member_b] {
            let shield = ReplacementDefinition::new(ReplacementEvent::Moved)
                .valid_card(TargetFilter::SelfRef)
                .mode(ReplacementMode::Optional { decline: None })
                .description("May replace as it leaves".to_string());
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .replacement_definitions
                .push(shield);
        }

        // Mass ChangeZone battlefield→hand over all three creatures.
        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Battlefield),
                destination: Zone::Hand,
                target: TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature)),
                enter_tapped: false,
            },
            Vec::new(),
            ObjectId(9100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::change_zone::resolve_all(&mut state, &ability, &mut events)
            .expect("mass change-zone resolves (pausing on the first MayCost member)");

        // Resolve each MayCost replacement choice (accept, index 0) until the
        // batch settles. Each accept resumes `drain_pending_change_zone_iteration`,
        // which stamps the resumed segment as its own co-departed group.
        let mut guard = 0;
        while matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) && guard < 10 {
            crate::game::engine::apply_as_current(
                &mut state,
                GameAction::ChooseReplacement { index: 0 },
            )
            .expect("accept the MayCost replacement");
            guard += 1;
        }
        // Order any same-controller co-departed observer triggers, then resolve.
        drain_order_triggers_with_identity(&mut state);
        resolve_stack_until_paused(&mut state);

        let _ = (member_a, member_b);
        // CURRENT (wrong) outcome: the observer left in the pre-pause segment and
        // observed only itself (life 20 + 1 = 21); the post-pause members were
        // stamped into a separate co-departed group it never saw.
        //
        // Once the cross-pause seam lands (redesign sketch above), flip this to
        // assert the observer fires once per co-departed member across the whole
        // batch (itself + member_a + member_b = 3, life 20 + 3 = 23).
        assert_eq!(
            state.players[0].life, 21,
            "CURRENT (wrong) outcome: cross-pause batch under-observes — the \
             pre-pause observer fires only for itself (life 21); expected 23 once \
             the cross-pause co-departed seam lands"
        );
    }

    /// CR 603.2 performance benchmark: replay the production Scute Swarm
    /// snapshot (619 permanents, 2886 batched landfall triggers queued on
    /// the stack) and report frames/sec. Baseline before the TriggerIndex
    /// was ~5 frames/sec; target with the index is ≥ 50 frames/sec.
    ///
    /// Requires `/tmp/gamestate.json` to be present (the captured snapshot
    /// is not committed). Skips cleanly when absent.
    #[test]
    #[ignore = "perf benchmark; run with `cargo test -p engine scute_swarm_throughput -- --ignored --nocapture`"]
    fn scute_swarm_throughput() {
        let path = "/tmp/gamestate.json";
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => {
                eprintln!("snapshot {path} not present; skipping benchmark");
                return;
            }
        };
        // Snapshot is wrapped as `{"gameState": {...}}`; unwrap to GameState.
        let wrapper: serde_json::Value = serde_json::from_str(&raw).expect("snapshot is JSON");
        let inner = wrapper.get("gameState").cloned().unwrap_or(wrapper);
        let mut state: GameState =
            serde_json::from_value(inner).expect("snapshot parses as GameState");
        let start = std::time::Instant::now();
        let mut frames = 0u32;
        while !state.stack.is_empty() && frames < 200 {
            let actor = state.priority_player;
            let _ = crate::game::engine::apply(&mut state, actor, GameAction::PassPriority);
            frames += 1;
        }
        let elapsed = start.elapsed();
        let fps = frames as f64 / elapsed.as_secs_f64().max(f64::EPSILON);
        eprintln!(
            "Resolved {} frames in {:?} ({:.1} frames/sec)",
            frames, elapsed, fps
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
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{GameState, WaitingFor, ZoneChangeRecord};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    fn make_creature(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        id
    }

    /// Build a minimal `Draw 1` triggered ability that matches a given mode.
    fn draw_one_trigger(mode: TriggerMode) -> TriggerDefinition {
        TriggerDefinition::new(mode)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
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
        // CR 603.3b (#531): drain the per-controller ordering prompt.
        super::drain_order_triggers_with_identity(&mut state);
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
        // CR 603.3b (#531): drain the per-controller ordering prompt.
        super::drain_order_triggers_with_identity(&mut state);
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
        // CR 603.3b (#531): drain the per-controller ordering prompt.
        super::drain_order_triggers_with_identity(&mut state);
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
        // CR 603.3b (#531): drain the per-controller ordering prompt.
        super::drain_order_triggers_with_identity(&mut state);
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
        // CR 603.3b (#531): doubled triggers fire as 2 in the same controller's
        // group, prompting OrderTriggers. Drain with identity to recover the
        // pre-#531 deterministic stack-placement that this assertion expects.
        super::drain_order_triggers_with_identity(&mut state);
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
        // CR 603.3b (#531): drain the per-controller ordering prompt.
        super::drain_order_triggers_with_identity(&mut state);
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
        // CR 603.3b (#531): drain the per-controller ordering prompt.
        super::drain_order_triggers_with_identity(&mut state);
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
        // CR 603.3b (#531): drain the per-controller ordering prompt.
        super::drain_order_triggers_with_identity(&mut state);
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

    /// Helper: install a source-restricted `DoubleTriggers` static
    /// (Splinter-class) — cause `Any`, narrowed by an `affected` source filter —
    /// controlled by PlayerId(0).
    fn install_source_restricted_doubler(
        state: &mut GameState,
        affected: TargetFilter,
    ) -> ObjectId {
        use crate::types::statics::{StaticMode, TriggerCause};
        let id = create_object(
            state,
            CardId(101),
            PlayerId(0),
            "Splinter, Radical Rat".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.static_definitions.push(
            crate::types::ability::StaticDefinition::new(StaticMode::DoubleTriggers {
                cause: TriggerCause::Any,
            })
            .affected(affected),
        );
        id
    }

    /// CR 603.2d: Splinter's source filter ("a Ninja creature you control")
    /// doubles a Ninja source's trigger to 2 instances.
    #[test]
    fn splinter_doubles_ninja_source_trigger() {
        use crate::types::ability::{ControllerRef, TypedFilter};

        let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Ninja".to_string());
        }
        let _splinter = install_source_restricted_doubler(
            &mut state,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .subtype("Ninja".to_string())
                    .controller(ControllerRef::You),
            ),
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
        super::drain_order_triggers_with_identity(&mut state);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 2,
            "Splinter must double a Ninja source's trigger to 2 instances"
        );
    }

    /// CR 603.2d: Splinter's source filter must NOT double a non-Ninja source's
    /// trigger — this is the reported bug (all triggers doubling). With the
    /// `affected` filter populated, a non-Ninja creature's trigger stays at 1.
    #[test]
    fn splinter_does_not_double_non_ninja_source_trigger() {
        use crate::types::ability::{ControllerRef, TypedFilter};

        let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
        // Observer is a creature, but NOT a Ninja.
        state
            .objects
            .get_mut(&observer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let _splinter = install_source_restricted_doubler(
            &mut state,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .subtype("Ninja".to_string())
                    .controller(ControllerRef::You),
            ),
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
        super::drain_order_triggers_with_identity(&mut state);
        let observer_triggers = state
            .stack
            .iter()
            .filter(|e| e.source_id == observer)
            .count();
        assert_eq!(
            observer_triggers, 1,
            "Splinter must NOT double a non-Ninja source's trigger — only Ninja sources qualify"
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
        // CR 603.3b (#531): drain the per-controller ordering prompt.
        super::drain_order_triggers_with_identity(&mut state);
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
        // CR 603.3b (#531): drain the per-controller ordering prompt.
        super::drain_order_triggers_with_identity(&mut state);
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
    /// turn" evaluates against the per-turn discard counts. Verifies both the
    /// positive (opponent discarded → condition met) and negative (no opponent
    /// discarded → condition unmet, as well as only-controller-discarded →
    /// condition unmet) paths for Tinybones, Trinket Thief.
    #[test]
    fn intervening_if_opponent_discarded_this_turn_gates_trigger() {
        use crate::types::ability::{
            AggregateFunction, Comparator, PlayerScope, QuantityExpr, QuantityRef, TriggerCondition,
        };

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let opponent = PlayerId(1);

        let condition = TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::CardsDiscardedThisTurn {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Sum,
                    },
                },
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
        crate::game::restrictions::record_discard(&mut state, controller);
        assert!(
            !check_trigger_condition(&state, &condition, controller, None, None),
            "self-discard must not satisfy 'an opponent discarded a card this turn'"
        );

        // Opponent discarded → condition met.
        crate::game::restrictions::record_discard(&mut state, opponent);
        assert!(
            check_trigger_condition(&state, &condition, controller, None, None),
            "opponent-discard must satisfy 'an opponent discarded a card this turn'"
        );
    }

    /// Issue #451 — RUNTIME PIPELINE TEST. CR 603.4 + CR 701.21: A who-controls
    /// sacrifice trigger ("Whenever an opponent who controls an artifact
    /// sacrifices a permanent, ...") must parse the relative clause into an
    /// `ObjectCount >= 1` intervening-if and gate the trigger correctly at
    /// runtime.
    ///
    /// This drives the real pipeline: the parser produces the `TriggerMode`
    /// and `TriggerDefinition.condition`, then `check_trigger_condition` (the
    /// exact evaluator `apply` uses for intervening-ifs) is run against a real
    /// `GameState`. The triggering player (the sacrificer) is bound from a
    /// `PermanentSacrificed` event. NOT a shape test — the condition under test
    /// is the parser's actual output, evaluated by the runtime evaluator.
    #[test]
    fn issue_451_who_controls_sacrifice_trigger_gates_at_runtime() {
        let mut ctx = crate::parser::oracle_ir::context::ParseContext::default();
        let (mode, def) = crate::parser::oracle_trigger::parse_trigger_condition(
            "Whenever an opponent who controls an artifact sacrifices a permanent",
            &mut ctx,
        );
        assert_eq!(
            mode,
            TriggerMode::Sacrificed,
            "who-controls sacrifice line must parse to Sacrificed (not Unknown)",
        );
        let condition = def
            .condition
            .expect("the who-controls clause must be lifted into def.condition");

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0); // the trigger source's controller
        let sacrificer = PlayerId(1); // the opponent who sacrifices

        // Sacrifice event — the triggering player is the sacrificer (P1).
        let sac_event = GameEvent::PermanentSacrificed {
            object_id: ObjectId(777),
            player_id: sacrificer,
        };

        // No one controls an artifact → the who-controls intervening-if fails.
        assert!(
            !check_trigger_condition(&state, &condition, controller, None, Some(&sac_event)),
            "with no artifact in play the who-controls clause must fail the trigger",
        );

        // The CONTROLLER (P0) controls an artifact, but the triggering player
        // is P1 → the clause (scoped to TriggeringPlayer) still fails.
        let p0_artifact = create_object(
            &mut state,
            CardId(300),
            controller,
            "Some Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&p0_artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        assert!(
            !check_trigger_condition(&state, &condition, controller, None, Some(&sac_event)),
            "an artifact controlled by the trigger's controller (not the \
             sacrificer) must NOT satisfy 'who controls an artifact'",
        );

        // The SACRIFICER (P1, the triggering player) controls an artifact →
        // the who-controls clause is satisfied and the trigger fires.
        let p1_artifact = create_object(
            &mut state,
            CardId(301),
            sacrificer,
            "Some Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&p1_artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        assert!(
            check_trigger_condition(&state, &condition, controller, None, Some(&sac_event)),
            "an artifact controlled by the sacrificing (triggering) player \
             must satisfy 'who controls an artifact' and fire the trigger",
        );
    }

    #[test]
    fn defending_player_life_quantity_reads_attack_event_player_target() {
        use crate::game::combat::AttackTarget;
        use crate::types::ability::{
            AggregateFunction, Comparator, PlayerScope, QuantityExpr, QuantityRef, TriggerCondition,
        };
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        let controller = PlayerId(0);
        let attacked_player = PlayerId(1);
        let other_opponent = PlayerId(2);
        let attacker = create_object(
            &mut state,
            CardId(1),
            controller,
            "Commander".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        state.players[0].life = 40;
        state.players[1].life = 35;
        state.players[2].life = 40;

        let condition = TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Opponent {
                        aggregate: AggregateFunction::Max,
                    },
                },
            },
            comparator: Comparator::LE,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::DefendingPlayer,
                },
            },
        };
        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker],
            defending_player: attacked_player,
            attacks: vec![(attacker, AttackTarget::Player(attacked_player))],
        };

        assert!(
            !check_trigger_condition(&state, &condition, controller, Some(attacker), Some(&event)),
            "another opponent with more life than the attacked player must fail Guild Artisan's intervening-if"
        );

        state
            .players
            .iter_mut()
            .find(|p| p.id == other_opponent)
            .unwrap()
            .life = 35;
        assert!(
            check_trigger_condition(&state, &condition, controller, Some(attacker), Some(&event)),
            "condition must pass when no opponent has more life than the attacked player"
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

    // ── CR 603.3b — Trigger-order choice for simultaneous triggers (issue #531) ──

    /// Helper: install a permanent with a `TriggerMode::Phase` trigger whose
    /// effect draws `n` cards for the controller (no targets, no input). Used
    /// by the simultaneous-trigger ordering tests.
    fn make_phase_trigger_source(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        draw_count: i32,
    ) -> ObjectId {
        let id = make_creature(state, owner, name, 1, 1);
        let trig_def = TriggerDefinition::new(TriggerMode::Phase)
            .phase(Phase::Upkeep)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: draw_count },
                    target: TargetFilter::Controller,
                },
            ))
            .description(format!("{name}: at the beginning of upkeep, draw a card."));
        let obj = state.objects.get_mut(&id).unwrap();
        obj.trigger_definitions.push(trig_def.clone());
        std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trig_def);
        id
    }

    /// Read the source IDs of the current stack entries in stack-bottom-to-top
    /// order. Each `StackEntry::source_id` lets the test discriminate which
    /// trigger ended up where.
    fn stack_source_ids(state: &GameState) -> Vec<ObjectId> {
        state.stack.iter().map(|e| e.source_id).collect()
    }

    /// CR 603.3b: When the active player controls two simultaneously-firing
    /// triggers, `process_triggers` must surface `WaitingFor::OrderTriggers`
    /// rather than placing them on the stack in a fixed deterministic order.
    /// **Discriminator**: submitting two different permutations produces two
    /// different stacks. A deterministic-ordering engine would yield the same
    /// stack for both inputs and fail this test.
    #[test]
    fn order_triggers_two_distinct_orders_produce_distinct_stacks() {
        let run = |order: Vec<usize>| -> Vec<ObjectId> {
            let mut state = setup();
            state.active_player = PlayerId(0);
            state.priority_player = PlayerId(0);
            state.phase = Phase::Upkeep;
            let src_a = make_phase_trigger_source(&mut state, PlayerId(0), "Source A", 1);
            let src_b = make_phase_trigger_source(&mut state, PlayerId(0), "Source B", 1);
            // Pre-stamp entered timestamps so collect_pending_triggers has a
            // deterministic placement seed.
            state
                .objects
                .get_mut(&src_a)
                .unwrap()
                .entered_battlefield_turn = Some(1);
            state
                .objects
                .get_mut(&src_b)
                .unwrap()
                .entered_battlefield_turn = Some(2);

            let event = GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
            };
            process_triggers(&mut state, &[event]);

            // The active player must be prompted to order the two triggers.
            let WaitingFor::OrderTriggers { player, triggers } = state.waiting_for.clone() else {
                panic!(
                    "expected WaitingFor::OrderTriggers, got {:?}",
                    state.waiting_for
                );
            };
            assert_eq!(player, PlayerId(0));
            assert_eq!(triggers.len(), 2, "both triggers must be in the prompt");

            crate::game::engine::apply_as_current(&mut state, GameAction::OrderTriggers { order })
                .expect("submit chosen order");

            stack_source_ids(&state)
        };

        let stack_identity = run(vec![0, 1]);
        let stack_reversed = run(vec![1, 0]);
        assert_eq!(stack_identity.len(), 2);
        assert_eq!(stack_reversed.len(), 2);
        assert_ne!(
            stack_identity, stack_reversed,
            "different OrderTriggers permutations must yield distinct stack orderings — \
             a deterministic engine (no player choice) would produce identical stacks"
        );
        // And the reversed input is literally the identity's reverse.
        let mut expected = stack_identity.clone();
        expected.reverse();
        assert_eq!(
            stack_reversed, expected,
            "stack-bottom-to-top ordering must mirror the submitted permutation"
        );
    }

    /// CR 603.3b: A player with exactly one trigger needs no ordering choice.
    /// `process_triggers` must NOT emit `WaitingFor::OrderTriggers`; the
    /// trigger goes straight to the stack via the existing dispatch loop.
    #[test]
    fn order_triggers_single_trigger_does_not_prompt() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::Upkeep;
        let _src = make_phase_trigger_source(&mut state, PlayerId(0), "Solo Source", 1);

        process_triggers(
            &mut state,
            &[GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
            }],
        );

        assert!(
            !matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }),
            "single trigger must not prompt for ordering; got {:?}",
            state.waiting_for
        );
        assert!(
            state.pending_trigger_order.is_none(),
            "no in-flight ordering state for a single trigger"
        );
        assert_eq!(
            state.stack.len(),
            1,
            "the single trigger reaches the stack directly"
        );
    }

    /// CR 603.3b + CR 101.4: With the active player NOT in seat 0, two
    /// non-active players' simultaneous triggers must be placed in turn order
    /// from the active player — not by timestamp. Regression for the binary
    /// active/non-active sort key that lumped every non-active player into one
    /// timestamp-ordered bucket: here P0's source is older than P2's, so the old
    /// key placed P0 before P2 by timestamp, but turn order from active P1 is
    /// P1, P2, P0, so P2 must be lower on the stack than P0.
    #[test]
    fn order_triggers_apnap_two_nonactive_players_use_turn_order() {
        let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 3, 123);
        // Active player is P1 (seat 1) — the case the binary key gets wrong.
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(1);
        state.phase = Phase::Upkeep;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        // One trigger each for two non-active players, so neither is prompted to
        // order and both reach the stack directly. P0's source is OLDER than
        // P2's, so a timestamp-based NAP ordering would place P0 first.
        let p2 = make_phase_trigger_source(&mut state, PlayerId(2), "P2 Source", 1);
        let p0 = make_phase_trigger_source(&mut state, PlayerId(0), "P0 Source", 1);
        state.objects.get_mut(&p0).unwrap().entered_battlefield_turn = Some(1);
        state.objects.get_mut(&p2).unwrap().entered_battlefield_turn = Some(2);

        process_triggers(
            &mut state,
            &[GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
            }],
        );

        // Neither player controls 2+ triggers, so there is no ordering prompt.
        assert!(
            !matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }),
            "single trigger per player must not prompt; got {:?}",
            state.waiting_for
        );

        // Turn order from active P1 is P1, P2, P0. The engine stores the stack
        // bottom-to-top, so P2 is lower and P0 is above it. The old binary key
        // ordered the two NAPs by timestamp instead, yielding [P0, P2].
        let stack_sources = stack_source_ids(&state);
        assert_eq!(stack_sources.len(), 2, "both triggers reach the stack");
        assert_eq!(
            stack_sources,
            vec![p2, p0],
            "non-active players must be placed by turn order (P2 below P0), not timestamp"
        );
    }

    /// CR 603.3b + CR 101.4 + CR 405.3: In a 3-player game with both AP and
    /// NAP controlling 2 simultaneous triggers each, the active player is
    /// prompted FIRST (CR 101.4 — APNAP choice order), then each NAP in turn
    /// order. The final stack reflects the placement order (AP first = bottom
    /// of stack) per CR 405.3.
    #[test]
    fn order_triggers_apnap_three_players() {
        let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 3, 123);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::Upkeep;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let p0_a = make_phase_trigger_source(&mut state, PlayerId(0), "P0 Source A", 1);
        let p0_b = make_phase_trigger_source(&mut state, PlayerId(0), "P0 Source B", 1);
        let p1_a = make_phase_trigger_source(&mut state, PlayerId(1), "P1 Source A", 1);
        let p1_b = make_phase_trigger_source(&mut state, PlayerId(1), "P1 Source B", 1);
        for (i, id) in [p0_a, p0_b, p1_a, p1_b].iter().enumerate() {
            state.objects.get_mut(id).unwrap().entered_battlefield_turn = Some(i as u32 + 1);
        }

        process_triggers(
            &mut state,
            &[GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
            }],
        );

        // CR 101.4: active player (P0) is prompted FIRST.
        let WaitingFor::OrderTriggers { player, .. } = state.waiting_for.clone() else {
            panic!(
                "expected OrderTriggers for P0 first, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(player, PlayerId(0), "AP must choose before NAPs (CR 101.4)");

        // P0 submits identity order.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::OrderTriggers { order: vec![0, 1] },
        )
        .expect("P0 submits");

        // Next prompt: P1 (next NAP in turn order).
        let WaitingFor::OrderTriggers { player, .. } = state.waiting_for.clone() else {
            panic!(
                "expected OrderTriggers for P1 after P0, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(player, PlayerId(1));

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::OrderTriggers { order: vec![0, 1] },
        )
        .expect("P1 submits");

        // Now all four triggers must be on the stack; AP's pair must be placed
        // FIRST (bottom of stack per CR 405.3 + 603.3b APNAP).
        let stack_sources = stack_source_ids(&state);
        assert_eq!(stack_sources.len(), 4, "four triggers on the stack");
        // Bottom two are the AP (P0)'s pair; top two are the NAP (P1)'s pair.
        let p1_ids = [p1_a, p1_b];
        let p0_ids = [p0_a, p0_b];
        for id in &stack_sources[0..2] {
            assert!(
                p0_ids.contains(id),
                "stack bottom must contain AP triggers (CR 405.3 + 603.3b)"
            );
        }
        for id in &stack_sources[2..4] {
            assert!(
                p1_ids.contains(id),
                "stack top must contain NAP triggers (CR 405.3 + 603.3b)"
            );
        }
    }
}

#[cfg(test)]
mod devour_runtime_tests {
    //! CR 702.82a + CR 614.1c + CR 614.12a runtime integration: a
    //! Devour-bearing creature's Hand→Battlefield ZoneChange routes through
    //! the synthesized `Moved` replacement, whose `Effect::Sacrifice` execute
    //! is non-modifier work — the pipeline stashes it as a
    //! `PostReplacementContinuation` and drains it after the move completes,
    //! raising a ranged sacrifice `EffectZoneChoice`. The Sacrifice
    //! completion stamps `state.last_effect_count`, which the chained
    //! `PutCounter` sub-ability's `QuantityRef::EventContextAmount` reads via
    //! its `.or(last_effect_count)` fallback.
    //!
    //! Lives in `game/triggers.rs` rather than `database/synthesis.rs::tests`
    //! so it can reach the `pub(super)` post-replacement-continuation drain
    //! API (`apply_pending_post_replacement_effect`) — the same call
    //! `stack.rs:575` makes during normal spell resolution.

    use crate::database::synthesis::synthesize_all;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::zones::{create_object, move_to_zone};
    use crate::types::ability::{EffectKind, PtValue, TargetFilter};
    use crate::types::actions::GameAction;
    use crate::types::card::CardFace;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::{GameState, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    /// Build a creature face carrying `Keyword::Devour(n)` and run the full
    /// synthesis pipeline. `CardFace::default()` leaves the mana cost zero
    /// and no other abilities so the runtime test exercises only Devour.
    fn devour_face(name: &str, n: u32) -> CardFace {
        let mut face = CardFace {
            name: name.to_string(),
            power: Some(PtValue::Fixed(3)),
            toughness: Some(PtValue::Fixed(3)),
            keywords: vec![Keyword::Devour(n)],
            ..CardFace::default()
        };
        face.card_type.core_types.push(CoreType::Creature);
        synthesize_all(&mut face);
        face
    }

    fn setup_state_with_priority(controller: PlayerId) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = controller;
        state.priority_player = controller;
        state.waiting_for = WaitingFor::Priority { player: controller };
        state
    }

    /// Place a plain vanilla 2/2 creature on the battlefield under `controller`.
    fn battlefield_creature(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(
            state,
            card_id,
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        id
    }

    fn p1p1(state: &GameState, id: ObjectId) -> u32 {
        state
            .objects
            .get(&id)
            .expect("object present")
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    }

    /// Drive a Devour creature's Hand→Battlefield ZoneChange through the
    /// replacement pipeline, then drain the post-replacement continuation —
    /// the same call `stack.rs:575` makes during real spell resolution.
    /// Returns the parked state on the Sacrifice `EffectZoneChoice`.
    ///
    /// `fodder` plain vanilla creatures are pre-placed under `controller` so
    /// they form the eligible sacrifice pool.
    fn drive_devour_etb_to_sacrifice_choice(
        face: &CardFace,
        controller: PlayerId,
        fodder: usize,
    ) -> (GameState, ObjectId) {
        // Sanity-check the synthesizer wired a Devour replacement onto the
        // face — a misfire would otherwise surface as a generic "prompt
        // never fired" downstream.
        assert!(
            face.replacements
                .iter()
                .any(|r| matches!(r.event, ReplacementEvent::Moved)
                    && matches!(r.valid_card, Some(TargetFilter::SelfRef))),
            "test fixture must carry a synthesized Devour ETB replacement; \
             got replacements={:?}",
            face.replacements
        );

        let mut state = setup_state_with_priority(controller);
        for i in 0..fodder {
            battlefield_creature(&mut state, controller, &format!("Sac Fodder {i}"));
        }
        let next_card = CardId(state.next_object_id);
        let obj_id = create_object(
            &mut state,
            next_card,
            controller,
            face.name.clone(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            apply_card_face_to_object(obj, face);
        }

        let proposed = crate::types::proposed_event::ProposedEvent::zone_change(
            obj_id,
            Zone::Hand,
            Zone::Battlefield,
            None,
        );
        let mut events = Vec::new();
        let result = crate::game::replacement::replace_event(&mut state, proposed, &mut events);
        let crate::game::replacement::ReplacementResult::Execute(event) = result else {
            panic!("Devour ETB pipeline must return Execute, got {result:?}");
        };
        let crate::types::proposed_event::ProposedEvent::ZoneChange { object_id, to, .. } = event
        else {
            panic!("pipeline must yield a ZoneChange execute event");
        };
        move_to_zone(&mut state, object_id, to, &mut events);

        assert!(
            state.post_replacement_continuation.is_some(),
            "Devour's non-modifier execute (Effect::Sacrifice) must be \
             stashed as a post-replacement continuation by the pipeline"
        );
        state.post_replacement_source = None;
        let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
            &mut state,
            Some(obj_id),
            None,
            Some(ReplacementEvent::Moved),
            &mut events,
        );

        (state, obj_id)
    }

    /// CR 702.82a + CR 614.12a: a Devour creature's ETB raises a ranged
    /// sacrifice prompt over the controller's creatures. With Devour
    /// unwired (before this fix) NO prompt fires — this assertion is the
    /// observable "as-enters sacrifice prompt never fires" bug from #532.
    #[test]
    fn devour_etb_raises_ranged_sacrifice_prompt() {
        let face = devour_face("Gorger Wurm", 1);
        let (state, _devour) = drive_devour_etb_to_sacrifice_choice(&face, PlayerId(0), 2);

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                min_count,
                up_to,
                effect_kind,
                ..
            } => {
                assert_eq!(
                    *player,
                    PlayerId(0),
                    "the sacrifice choice is the controller's"
                );
                assert_eq!(*min_count, 0, "CR 702.82a: an empty sacrifice is legal");
                assert!(
                    *up_to,
                    "Devour offers a ranged 'sacrifice any number' choice"
                );
                assert_eq!(
                    *effect_kind,
                    EffectKind::Sacrifice,
                    "the Devour prompt is a Sacrifice choice"
                );
            }
            other => panic!("expected an EffectZoneChoice, got {other:?}"),
        }
    }

    /// PRIMARY DISCRIMINATOR for the counter-count linkage bug. Sacrificing
    /// two creatures to Devour 1 places exactly two +1/+1 counters on the
    /// entering permanent. Under v1's `PreviousEffectAmount` route this would
    /// resolve to 0 (the ranged Sacrifice never stamps `last_effect_amount`);
    /// under v2's `EventContextAmount` it reads `last_effect_count = 2`.
    #[test]
    fn devour_1_full_sacrifice_places_one_counter_per_creature() {
        let face = devour_face("Gorger Wurm", 1);
        let (mut state, devour) = drive_devour_etb_to_sacrifice_choice(&face, PlayerId(0), 2);

        let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
            panic!("expected the Devour sacrifice choice");
        };
        assert!(
            cards.len() >= 2,
            "two pre-placed creatures must be eligible Devour sacrifices, got {cards:?}"
        );
        let to_sacrifice: Vec<ObjectId> = cards.iter().copied().take(2).collect();

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: to_sacrifice.clone(),
            },
        )
        .unwrap();

        assert_eq!(
            state.objects.get(&devour).unwrap().zone,
            Zone::Battlefield,
            "the Devour creature must end up on the battlefield"
        );
        assert_eq!(
            p1p1(&state, devour),
            2,
            "Devour 1 + two creatures sacrificed → 2 +1/+1 counters (CR 702.82a)"
        );
        for sac in &to_sacrifice {
            assert_eq!(
                state.objects.get(sac).unwrap().zone,
                Zone::Graveyard,
                "each sacrificed creature must be in the graveyard"
            );
        }
    }

    /// CR 702.82a: an empty sacrifice is legal — the Devour creature enters
    /// with 0 counters. NOTE: this case alone does NOT discriminate the v1
    /// linkage bug (both `PreviousEffectAmount` and `EventContextAmount`
    /// resolve to 0 here). It is paired with the full-sacrifice test above —
    /// that test is the true linkage-bug discriminator.
    #[test]
    fn devour_1_empty_sacrifice_enters_with_zero_counters() {
        let face = devour_face("Gorger Wurm", 1);
        let (mut state, devour) = drive_devour_etb_to_sacrifice_choice(&face, PlayerId(0), 2);

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards { cards: vec![] },
        )
        .unwrap();

        assert_eq!(
            state.objects.get(&devour).unwrap().zone,
            Zone::Battlefield,
            "the Devour creature still enters when nothing is sacrificed"
        );
        assert_eq!(
            p1p1(&state, devour),
            0,
            "an empty Devour sacrifice places 0 counters (CR 702.82a)"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "no further sacrifice prompt should remain after the empty choice"
        );
    }

    /// CR 702.82a: Devour 2 places N=2 counters per creature sacrificed.
    /// One sacrifice → 2 counters, via the synthesizer's
    /// `QuantityExpr::Multiply { factor: 2, .. }` wrapping
    /// `EventContextAmount`.
    #[test]
    fn devour_2_one_sacrifice_places_two_counters() {
        let face = devour_face("Mycoloth", 2);
        let (mut state, devour) = drive_devour_etb_to_sacrifice_choice(&face, PlayerId(0), 2);

        let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
            panic!("expected the Devour sacrifice choice");
        };
        let one = vec![*cards.first().expect("at least one eligible creature")];

        crate::game::engine::apply_as_current(&mut state, GameAction::SelectCards { cards: one })
            .unwrap();

        assert_eq!(
            p1p1(&state, devour),
            2,
            "Devour 2 + one creature sacrificed → 2 +1/+1 counters (N per sacrifice)"
        );
    }
}

// ===================================================================
// CR 603.3c + CR 603.3d "Push first, choose second" contract tests
// ===================================================================
//
// These tests verify the invariant established by the trigger-stack-push
// refactor: a triggered ability that requires player input (mode choice,
// target selection, or division-among) is pushed to `state.stack` BEFORE
// the prompt is opened, in a mid-construction state. `pending_trigger_entry`
// identifies the in-construction entry; the resolver (`stack::resolve_top`)
// refuses to fire entries identified by this cursor. This is the behaviour
// fix for Lulu, Stern Guardian and the broader class of triggers whose UI
// prompt previously appeared with no stack entry for context.
#[cfg(test)]
mod push_first_contract_tests {
    use super::process_triggers;
    use crate::game::effects::resolve_ability_chain;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityDefinition, AbilityKind, ControllerRef, Effect, PaymentCost,
        QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TriggerDefinition, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{GameState, StackEntryKind, WaitingFor, ZoneChangeRecord};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    fn make_creature(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.power = Some(2);
        obj.toughness = Some(2);
        id
    }

    fn zone_changed_event(object_id: ObjectId, from: Zone, to: Zone) -> GameEvent {
        GameEvent::ZoneChanged {
            object_id,
            from: Some(from),
            to,
            record: Box::new(ZoneChangeRecord {
                name: "Test Source".to_string(),
                core_types: vec![CoreType::Enchantment],
                subtypes: vec![],
                ..ZoneChangeRecord::test_minimal(object_id, Some(from), to)
            }),
        }
    }

    fn build_exile_target_opponent_creature_trigger() -> TriggerDefinition {
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
                    enters_under: None,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
    }

    fn make_source_with_trigger(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(0),
            "Test Source".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.entered_battlefield_turn = Some(1);
        obj.trigger_definitions
            .push(build_exile_target_opponent_creature_trigger());
        id
    }

    /// Test #1 (Lulu / blocker-validating): a target-requiring trigger pushes
    /// to the stack BEFORE prompting the controller. This test must FAIL on
    /// the pre-refactor codebase and PASS on the post-refactor codebase.
    #[test]
    fn push_first_target_trigger_appears_on_stack_during_prompt() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Two legal opponent creatures so target choice cannot auto-resolve.
        let target1 = make_creature(&mut state, PlayerId(1), "Opp 1");
        let _target2 = make_creature(&mut state, PlayerId(1), "Opp 2");
        let source = make_source_with_trigger(&mut state);

        process_triggers(
            &mut state,
            &[zone_changed_event(source, Zone::Hand, Zone::Battlefield)],
        );

        // CR 603.3c + CR 603.3d "Push first": the trigger entry MUST be on the
        // stack already, identified by `pending_trigger_entry`. This is the
        // structural change that fails on the pre-refactor codebase, where
        // `process_triggers` would set `pending_trigger` without pushing.
        assert_eq!(
            state.stack.len(),
            1,
            "trigger entry must be on the stack while target prompt is pending",
        );
        let entry_id = state
            .pending_trigger_entry
            .expect("pending_trigger_entry must mark the in-construction entry");
        assert_eq!(state.stack.back().map(|e| e.id), Some(entry_id));
        let entry = state.stack.back().unwrap();
        assert_eq!(entry.source_id, source);
        assert!(matches!(
            entry.kind,
            StackEntryKind::TriggeredAbility { .. }
        ));
        assert!(state.pending_trigger.is_some());

        // Drive the engine pipeline forward — `begin_pending_trigger_target_selection`
        // translates the pending state into `WaitingFor::TriggerTargetSelection`,
        // matching what the action dispatcher does in production.
        let wf = crate::game::engine::begin_pending_trigger_target_selection(&mut state)
            .expect("begin target selection")
            .expect("target prompt required (two legal targets)");
        state.waiting_for = wf;
        assert!(matches!(
            state.waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ));

        // Complete the choice: entry stays on the stack, fully constructed,
        // with targets populated. Cursor cleared.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target1)),
            },
        )
        .expect("choose target succeeds");
        assert_eq!(state.stack.len(), 1, "entry remains on stack post-choice");
        assert!(
            state.pending_trigger_entry.is_none(),
            "construction complete -> cursor cleared",
        );
        let entry = state.stack.back().unwrap();
        if let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind {
            assert_eq!(ability.targets, vec![TargetRef::Object(target1)]);
        } else {
            panic!("expected TriggeredAbility on stack");
        }
    }

    /// Test #5 (resolver-refusal): `stack::resolve_top` must NOT fire the top
    /// entry while `pending_trigger_entry` identifies it. This is the
    /// invariant gate that prevents the in-construction entry from resolving.
    #[test]
    fn push_first_resolver_refuses_in_construction_entry() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Set up a pending-target trigger via the production pipeline (so the
        // entry is genuinely in-construction, not synthesized by hand).
        let _t1 = make_creature(&mut state, PlayerId(1), "Opp 1");
        let _t2 = make_creature(&mut state, PlayerId(1), "Opp 2");
        let source = make_source_with_trigger(&mut state);
        process_triggers(
            &mut state,
            &[zone_changed_event(source, Zone::Hand, Zone::Battlefield)],
        );

        // Confirm pre-conditions: top is the in-construction entry.
        let in_construction_id = state.pending_trigger_entry.expect("entry set");
        let stack_len_before = state.stack.len();
        assert_eq!(state.stack.back().map(|e| e.id), Some(in_construction_id));

        // Call resolve_top directly: it must refuse to act on the entry.
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        assert_eq!(
            state.stack.len(),
            stack_len_before,
            "resolve_top must not pop the in-construction entry",
        );
        assert_eq!(
            state.stack.back().map(|e| e.id),
            Some(in_construction_id),
            "in-construction entry stays on top",
        );
        assert!(
            events.is_empty(),
            "no StackResolved event for refused resolution, got {events:?}",
        );
        assert_eq!(
            state.pending_trigger_entry,
            Some(in_construction_id),
            "cursor preserved on refusal",
        );
    }

    /// Test #7 (CR 603.3d → CR 601.2c no-legal-targets removal): a
    /// target-requiring trigger with zero legal targets is dropped without
    /// pushing to the stack or leaving a cursor.
    #[test]
    fn push_first_no_legal_targets_drops_trigger_silently() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Opponent controls NO creatures; the trigger requires a target
        // opponent creature.
        let source = make_source_with_trigger(&mut state);
        let stack_before = state.stack.len();
        process_triggers(
            &mut state,
            &[zone_changed_event(source, Zone::Hand, Zone::Battlefield)],
        );

        assert_eq!(
            state.stack.len(),
            stack_before,
            "no-legal-target trigger must not be pushed to the stack",
        );
        assert!(
            state.pending_trigger_entry.is_none(),
            "no-legal-target trigger must not leave a cursor",
        );
        assert!(state.pending_trigger.is_none());
    }

    /// Test #10 (reflexive WhenYouDo trigger): the push-first contract holds
    /// at the OTHER pause-path site, `effects/mod.rs::resolve_chain_body`
    /// (line ~3654). A reflexive `WhenYouDo` sub-ability with empty `targets`
    /// and a non-empty target-slot set must push the entry to the stack
    /// BEFORE entering `WaitingFor::TriggerTargetSelection`. Structurally
    /// identical to the main dispatch path but lives in a different function;
    /// a regression here would not fail the other discriminating tests.
    #[test]
    fn push_first_reflexive_when_you_do_pushes_to_stack_during_prompt() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        // Cost-payment must succeed so the WhenYouDo gate fires (CR 603.12 +
        // issue #418): controller has exactly enough energy.
        state.players[0].energy = 3;

        // Two legal target candidates (own creatures) so the reflexive's
        // PutCounter target slot has multiple legal choices, forcing the
        // player-choice path through `begin_target_selection_for_ability`.
        let candidate1 = make_creature(&mut state, PlayerId(0), "Candidate 1");
        let _candidate2 = make_creature(&mut state, PlayerId(0), "Candidate 2");

        // Reflexive sub-ability: PutCounter on a chosen creature you control.
        // `targets` is empty so `effects/mod.rs:3585-3667` enters the
        // push-first path; `TargetFilter::Typed` resolves to a target slot
        // when `build_target_slots` runs.
        let source_id = ObjectId(state.next_object_id);
        let sub = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
            vec![],
            source_id,
            PlayerId(0),
        )
        .condition(AbilityCondition::WhenYouDo);

        // Parent: pay {E}{E}{E}. On success the reflexive `WhenYouDo` fires.
        let parent = ResolvedAbility::new(
            Effect::PayCost {
                cost: PaymentCost::Energy {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )
        .sub_ability(sub);

        let stack_before = state.stack.len();
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &parent, &mut events, 0).expect("resolve parent chain");

        // CR 603.3c + CR 603.3d: The reflexive trigger entry MUST be on the
        // stack now (push-first contract holds at the reflexive site too).
        assert_eq!(
            state.stack.len(),
            stack_before + 1,
            "reflexive WhenYouDo trigger must be on the stack while its target prompt is open",
        );
        let entry_id = state
            .pending_trigger_entry
            .expect("pending_trigger_entry must mark the in-construction entry");
        assert_eq!(state.stack.back().map(|e| e.id), Some(entry_id));
        let entry = state.stack.back().unwrap();
        assert!(matches!(
            entry.kind,
            StackEntryKind::TriggeredAbility { .. }
        ));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ));

        // Complete the target choice: entry stays on stack, fully constructed.
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(candidate1)),
            },
        )
        .expect("choose target succeeds");
        assert!(
            state.pending_trigger_entry.is_none(),
            "reflexive construction complete -> cursor cleared",
        );
    }

    /// Test #6 (CR 603.3c no-legal-modes modal early-drop): a modal trigger
    /// where every mode's target is illegal must be dropped at the modal
    /// pre-filter (`triggers.rs::dispatch_pending_trigger_context` line ~2235)
    /// BEFORE any `StackPushed` event is emitted. Exercises the new pre-push
    /// logic (`compute_unavailable_modes` + `filter_modes_by_target_legality`)
    /// that is otherwise structurally unverified by the non-modal Err-branch
    /// tests.
    #[test]
    fn push_first_no_legal_modes_modal_trigger_dropped_silently() {
        use crate::types::ability::{ModalChoice, PlayerFilter};

        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // No opponent creatures exist. Both modes target an opponent
        // creature, so every mode pre-filters as illegal at modal-pause time.
        let source_id = ObjectId(state.next_object_id);
        let opponent_creature_target =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
        let mode_a = AbilityDefinition::new(
            AbilityKind::Database,
            Effect::Destroy {
                target: opponent_creature_target.clone(),
                cant_regenerate: false,
            },
        );
        let mode_b = AbilityDefinition::new(
            AbilityKind::Database,
            Effect::Destroy {
                target: opponent_creature_target.clone(),
                cant_regenerate: false,
            },
        );
        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            mode_descriptions: vec!["A".to_string(), "B".to_string()],
            allow_repeat_modes: false,
            constraints: vec![],
            mode_costs: vec![],
            entwine_cost: None,
            chooser: PlayerFilter::Controller,
        };
        let modal_ability = AbilityDefinition::new(
            AbilityKind::Database,
            // Inner effect is not actually executed (a mode replaces it); pick
            // a placeholder that resolves cleanly if it were ever to fire.
            Effect::Destroy {
                target: TargetFilter::None,
                cant_regenerate: false,
            },
        )
        .with_modal(modal, vec![mode_a, mode_b]);

        // Construct a PendingTrigger directly and dispatch it through the
        // public pipeline. Modal trigger context with no legal mode reaches
        // the early-drop branch at `triggers.rs::dispatch_pending_trigger_context`.
        let trigger = super::PendingTrigger {
            source_id,
            controller: PlayerId(0),
            condition: None,
            ability: super::super::ability_utils::build_resolved_from_def(
                &modal_ability,
                source_id,
                PlayerId(0),
            ),
            timestamp: state.turn_number,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: modal_ability.modal.clone(),
            mode_abilities: modal_ability.mode_abilities.clone(),
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
        };

        let stack_before = state.stack.len();
        let mut events = Vec::new();
        let paused = super::dispatch_pending_trigger_context(
            &mut state,
            super::PendingTriggerContext::single(trigger),
            &mut events,
        );

        // CR 603.3c "If no mode can be chosen, the ability is removed from
        // the stack": dispatcher reports no pause; nothing pushed; no cursor.
        assert!(
            !paused,
            "modal trigger with no legal mode must not pause on player input",
        );
        assert_eq!(
            state.stack.len(),
            stack_before,
            "no-legal-mode modal trigger must NOT be pushed to the stack",
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::StackPushed { .. })),
            "no StackPushed event must be emitted for a dropped no-legal-mode modal trigger",
        );
        assert!(
            state.pending_trigger_entry.is_none(),
            "no-legal-mode modal trigger must not leave a cursor",
        );
        assert!(
            state.pending_trigger.is_none(),
            "no-legal-mode modal trigger must not leave a stashed pending_trigger",
        );
    }
}
