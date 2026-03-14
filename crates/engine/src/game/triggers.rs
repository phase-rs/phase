use std::collections::HashMap;

use crate::types::ability::{
    AbilityDefinition, Effect, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
    TriggerCondition, TriggerDefinition, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::stack;
use super::targeting;

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
    pub trigger_def: TriggerDefinition,
    pub ability: ResolvedAbility,
    pub timestamp: u32,
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
                    if !check_trigger_condition(state, condition, controller) {
                        continue;
                    }
                }
                let ability = build_triggered_ability(trig_def, obj_id, controller);
                pending.push(PendingTrigger {
                    source_id: obj_id,
                    controller,
                    trigger_def: trig_def.clone(),
                    ability,
                    timestamp,
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
                            trigger_def: prowess_trig_def,
                            ability: prowess_ability,
                            timestamp,
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
    // LIFO means AP triggers resolve last, which is correct per MTG 603.3b.
    pending.reverse();

    let mut events_out = Vec::new();
    for trigger in pending {
        // Check if this trigger's ability has targeting requirements
        if let Some(target_filter) = extract_target_filter_from_effect(&trigger.ability.effect) {
            let optional = trigger.ability.optional_targeting;
            let legal = targeting::find_legal_targets(
                state,
                target_filter,
                trigger.controller,
                trigger.source_id,
            );
            if legal.is_empty() {
                if optional {
                    // "Up to one" with no legal targets — put on stack with 0 targets
                    let entry_id = ObjectId(state.next_object_id);
                    state.next_object_id += 1;
                    let condition = trigger.trigger_def.condition.clone();
                    let entry = StackEntry {
                        id: entry_id,
                        source_id: trigger.source_id,
                        controller: trigger.controller,
                        kind: StackEntryKind::TriggeredAbility {
                            source_id: trigger.source_id,
                            ability: trigger.ability,
                            condition,
                        },
                    };
                    stack::push_to_stack(state, entry, &mut events_out);
                } else {
                    // No legal targets -- skip this trigger entirely
                    continue;
                }
            } else if legal.len() == 1 && !optional {
                // Auto-target: set the target and push to stack
                let mut ability = trigger.ability;
                ability.targets = legal;
                super::casting::emit_targeting_events(
                    state,
                    &ability.targets,
                    trigger.source_id,
                    trigger.controller,
                    &mut events_out,
                );
                let entry_id = ObjectId(state.next_object_id);
                state.next_object_id += 1;
                let condition = trigger.trigger_def.condition.clone();
                let entry = StackEntry {
                    id: entry_id,
                    source_id: trigger.source_id,
                    controller: trigger.controller,
                    kind: StackEntryKind::TriggeredAbility {
                        source_id: trigger.source_id,
                        ability,
                        condition,
                    },
                };
                stack::push_to_stack(state, entry, &mut events_out);
            } else {
                // Multiple targets or optional targeting -- prompt player for choice
                state.pending_trigger = Some(trigger);
                return;
            }
        } else {
            // No targeting needed -- push to stack as normal
            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;
            let condition = trigger.trigger_def.condition.clone();
            let entry = StackEntry {
                id: entry_id,
                source_id: trigger.source_id,
                controller: trigger.controller,
                kind: StackEntryKind::TriggeredAbility {
                    source_id: trigger.source_id,
                    ability: trigger.ability,
                    condition,
                },
            };
            stack::push_to_stack(state, entry, &mut events_out);
        }
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
    }
}

/// Check whether an intervening-if condition is satisfied.
/// Used both at fire-time and resolution-time.
pub(crate) fn check_trigger_condition(
    state: &GameState,
    condition: &TriggerCondition,
    controller: PlayerId,
) -> bool {
    match condition {
        TriggerCondition::LifeGainedThisTurn { minimum } => state
            .players
            .iter()
            .find(|p| p.id == controller)
            .map(|p| p.life_gained_this_turn >= *minimum)
            .unwrap_or(false),
        TriggerCondition::DescendedThisTurn => state
            .players
            .iter()
            .find(|p| p.id == controller)
            .map(|p| p.descended_this_turn)
            .unwrap_or(false),
        TriggerCondition::ControlCreatureCount { minimum } => {
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
    }
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
        TriggerConstraint::OnlyDuringYourTurn => {
            // No tracking needed — checked at fire time via active_player
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

/// Recursively build a ResolvedAbility from an AbilityDefinition.
fn build_resolved_from_def(
    def: &AbilityDefinition,
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    let mut resolved = ResolvedAbility::new(def.effect.clone(), Vec::new(), source_id, controller);
    if let Some(sub) = &def.sub_ability {
        resolved = resolved.sub_ability(build_resolved_from_def(sub, source_id, controller));
    }
    if let Some(d) = def.duration.clone() {
        resolved = resolved.duration(d);
    }
    if let Some(c) = def.condition.clone() {
        resolved = resolved.condition(c);
    }
    resolved.optional_targeting = def.optional_targeting;
    resolved
}

/// Extract the TargetFilter from an effect, if it has targeting requirements.
/// Returns None for effects with no targeting (Draw, GainLife, etc.) or
/// effects targeting self/controller (which don't need player selection).
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
        | Effect::RevealHand { target, .. } => {
            if matches!(
                target,
                TargetFilter::None | TargetFilter::SelfRef | TargetFilter::Controller
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
                TargetFilter::None | TargetFilter::SelfRef | TargetFilter::Controller
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
    r.insert(TriggerMode::DamageDoneOnceByController, match_damage_done);
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
        TriggerMode::ClassLevelGained,
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
        TriggerMode::CaseSolved,
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
                    FilterProp::Tapped => {
                        if !obj.tapped {
                            return false;
                        }
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
                    FilterProp::WithKeyword { value } => {
                        if !obj.keywords.iter().any(|k| format!("{:?}", k) == *value) {
                            return false;
                        }
                    }
                    FilterProp::Another => {
                        if object_id == source_id {
                            return false;
                        }
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
                    FilterProp::PowerLE { value } => {
                        if obj.power.unwrap_or(0) > *value {
                            return false;
                        }
                    }
                    FilterProp::PowerGE { value } => {
                        if obj.power.unwrap_or(0) < *value {
                            return false;
                        }
                    }
                    FilterProp::Multicolored => {
                        if obj.color.len() <= 1 {
                            return false;
                        }
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
        // StackAbility targeting is handled directly in find_legal_targets
        TargetFilter::StackAbility => false,
        TargetFilter::SpecificObject(target_id) => object_id == *target_id,
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
    } = event
    {
        // Check if trigger requires damage from a specific source
        if !valid_source_matches(trigger, state, *dmg_source, source_id) {
            return false;
        }
        // Check combat_damage flag
        if trigger.combat_damage {
            // For combat damage filtering, we'd need combat state.
            // For now, allow all damage events when combat_damage is set.
        }
        true
    } else {
        false
    }
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
        // Check valid_target as controller filter (ValidActivatingPlayer equivalent)
        if let Some(ref vt) = trigger.valid_target {
            let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
            match vt {
                TargetFilter::Controller => {
                    if trigger_controller != Some(*controller) {
                        return false;
                    }
                }
                TargetFilter::Typed(TypedFilter {
                    controller: Some(crate::types::ability::ControllerRef::You),
                    ..
                }) => {
                    if trigger_controller != Some(*controller) {
                        return false;
                    }
                }
                TargetFilter::Typed(TypedFilter {
                    controller: Some(crate::types::ability::ControllerRef::Opponent),
                    ..
                }) => {
                    if trigger_controller == Some(*controller) {
                        return false;
                    }
                }
                _ => {}
            }
        }
        true
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
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    if let GameEvent::BlockersDeclared { assignments } = event {
        // Blocks trigger: source creature is among blockers
        assignments.iter().any(|(blocker, _)| *blocker == source_id)
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
        counter_type: _,
        ..
    } = event
    {
        // Counter type filtering would use typed fields in future
        // Check valid_card filter
        if !valid_card_matches(trigger, state, *object_id, source_id) {
            return false;
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
        // Check valid_target as player filter
        if let Some(ref vt) = trigger.valid_target {
            let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
            match vt {
                TargetFilter::Controller => trigger_controller == Some(*player_id),
                _ => true,
            }
        } else {
            true
        }
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
        if let Some(ref vt) = trigger.valid_target {
            let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
            match vt {
                TargetFilter::Controller => trigger_controller == Some(*player_id),
                _ => true,
            }
        } else {
            true
        }
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
        if let Some(ref vt) = trigger.valid_target {
            let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
            match vt {
                TargetFilter::Controller => trigger_controller == Some(*player_id),
                _ => true,
            }
        } else {
            true
        }
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

fn match_becomes_target(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::BecomesTarget { object_id, .. } = event {
        if trigger.valid_card.is_some() {
            valid_card_matches(trigger, state, *object_id, source_id)
        } else {
            *object_id == source_id
        }
    } else {
        false
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

/// Cycled: fires on discard events that are part of cycling.
fn match_cycled(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::Discarded { .. })
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
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    if let GameEvent::ManaAdded {
        source_id: mana_source,
        ..
    } = event
    {
        *mana_source == source_id
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
    if let GameEvent::DamageDealt { target, .. } = event {
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
        AbilityKind, ControllerRef, FilterProp, GainLifePlayer, LifeAmount, TargetFilter,
        TriggerDefinition,
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
        };
        assert!(match_damage_done(&event, &trigger, ObjectId(1), &state));
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
                        Effect::Draw { count: 1 },
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
                        Effect::Draw { count: 1 },
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
                        Effect::Draw { count: 1 },
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
                        Effect::Draw { count: 1 },
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
                        Effect::Draw { count: 1 },
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
                        Effect::Draw { count: 1 },
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
            AbilityDefinition::new(AbilityKind::Database, Effect::Draw { count: 2 }).sub_ability(
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: LifeAmount::Fixed(3),
                        player: GainLifePlayer::Controller,
                    },
                ),
            ),
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
                        Effect::Draw { count: 1 },
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
}
