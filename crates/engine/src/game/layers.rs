use std::sync::Arc;

use crate::game::arithmetic::saturating_pt_add;
use crate::game::devotion::count_devotion;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::printed_cards::{apply_copiable_values, intrinsic_copiable_values};
use crate::game::speed::{effective_speed, has_max_speed};
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, BasicLandType, CastingPermission,
    ContinuousModification, CopiableValues, Duration, Effect, ManaContribution, ManaProduction,
    QuantityExpr, StaticCondition, StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::card_type::{is_land_subtype, CoreType};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::layers::{ActiveContinuousEffect, Layer};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;

/// Remove transient effects that have expired based on their duration.
/// Called during cleanup (end of turn) to prune `UntilEndOfTurn` effects.
/// CR 514.2: End-of-turn continuous effects expire at cleanup.
pub fn prune_end_of_turn_effects(state: &mut GameState) {
    let before = state.transient_continuous_effects.len();
    state
        .transient_continuous_effects
        .retain(|e| e.duration != Duration::UntilEndOfTurn);
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty = true;
    }
}

/// Remove transient effects that expire at end of combat.
/// Called during the EndCombat phase transition per CR 514.2.
pub fn prune_end_of_combat_effects(state: &mut GameState) {
    let before = state.transient_continuous_effects.len();
    state
        .transient_continuous_effects
        .retain(|e| e.duration != Duration::UntilEndOfCombat);
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty = true;
    }
}

/// CR 514.2 + CR 611.2a: Remove `PlayFromExile` casting permissions whose
/// `Duration::UntilEndOfTurn` expires at cleanup. Called from the cleanup step
/// alongside `prune_end_of_turn_effects`.
///
/// Only `PlayFromExile` is durational. Other casting-permission variants
/// (`AdventureCreature`, `ExileWithAltCost`, `ExileWithEnergyCost`, `WarpExile`)
/// persist until the object leaves exile (handled by `zones::apply_zone_exit_cleanup`).
pub fn prune_end_of_turn_casting_permissions(state: &mut GameState) {
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        obj.casting_permissions.retain(|p| match p {
            CastingPermission::PlayFromExile {
                duration: Duration::UntilEndOfTurn,
                ..
            } => false,
            // CR 514.2: UntilEndOfCombat should have been pruned at end of combat,
            // but if it leaked to cleanup, prune it here defensively.
            CastingPermission::PlayFromExile {
                duration: Duration::UntilEndOfCombat,
                ..
            } => false,
            CastingPermission::PlayFromExile {
                duration: Duration::UntilYourNextTurn | Duration::Permanent,
                ..
            } => true,
            // UntilHostLeavesPlay / ForAsLongAs / UntilControllerNextUntapStep:
            // these are pruned by their own systems (zone-exit cleanup, condition
            // re-evaluation, untap step). Retain here — they are not end-of-turn.
            CastingPermission::PlayFromExile { .. } => true,
            CastingPermission::AdventureCreature
            | CastingPermission::ExileWithAltCost { .. }
            | CastingPermission::ExileWithAltAbilityCost { .. }
            | CastingPermission::ExileWithEnergyCost
            | CastingPermission::WarpExile { .. }
            // CR 702.170d: Plotted persists across turns (that is the whole
            // point of Plot — cast "on a later turn"); never pruned at cleanup.
            | CastingPermission::Plotted { .. } => true,
        });
    }
}

/// CR 514.2 + CR 611.2a/b: Remove `PlayFromExile` permissions granted to
/// `active_player` whose `Duration::UntilYourNextTurn` expires at that
/// player's untap step. Called from the untap step alongside
/// `prune_until_next_turn_effects`.
pub fn prune_until_next_turn_casting_permissions(state: &mut GameState, active_player: PlayerId) {
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        obj.casting_permissions.retain(|p| match p {
            CastingPermission::PlayFromExile {
                duration: Duration::UntilYourNextTurn,
                granted_to,
            } => *granted_to != active_player,
            CastingPermission::PlayFromExile { .. }
            | CastingPermission::AdventureCreature
            | CastingPermission::ExileWithAltCost { .. }
            | CastingPermission::ExileWithAltAbilityCost { .. }
            | CastingPermission::ExileWithEnergyCost
            | CastingPermission::WarpExile { .. }
            // CR 702.170d: Plotted persists across turns; never pruned at the
            // untap step. Retention is zone-scoped (see zones::apply_zone_exit_cleanup).
            | CastingPermission::Plotted { .. } => true,
        });
    }
}

/// Remove transient `UntilYourNextTurn` effects whose controller's turn is starting.
/// Called at the start of the active player's turn (untap step) per CR 514.2.
///
/// Also clears `goaded_by` entries for the active player on all battlefield objects,
/// per CR 701.15a: goad expires at the beginning of the goading player's next turn.
pub fn prune_until_next_turn_effects(state: &mut GameState, active_player: PlayerId) {
    let before = state.transient_continuous_effects.len();
    state
        .transient_continuous_effects
        .retain(|e| !(e.duration == Duration::UntilYourNextTurn && e.controller == active_player));
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty = true;
    }

    // CR 701.15a: Goad expires at the goading player's next turn. Clear goaded_by entries
    // for the active player on all battlefield objects.
    // CR 701.35a: Detain expires at the detaining player's next turn. Clear detained_by
    // entries for the active player on all battlefield objects.
    for obj_id in state.battlefield.clone() {
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            obj.goaded_by.remove(&active_player);
            obj.detained_by.remove(&active_player);
        }
    }
}

/// CR 502.3: Prune "until controller's next untap step" transient effects
/// for permanents controlled by the active player. Called during the untap step
/// AFTER enforcing the CantUntap restriction (so the permanent skips exactly one untap).
pub fn prune_controller_untap_step_effects(state: &mut GameState, active_player: PlayerId) {
    let before = state.transient_continuous_effects.len();
    state.transient_continuous_effects.retain(|e| {
        if e.duration != Duration::UntilControllerNextUntapStep {
            return true;
        }
        // The effect applies to specific objects — check if the affected object
        // is controlled by the active player (whose untap step is happening).
        match &e.affected {
            TargetFilter::SpecificObject { id } => {
                let is_active_controlled = state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.controller == active_player);
                // Keep the effect if NOT controlled by active player (not their untap step yet)
                !is_active_controlled
            }
            _ => true,
        }
    });
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty = true;
    }
}

/// Remove transient effects whose source has left the battlefield.
/// Called when an object leaves the battlefield.
pub fn prune_host_left_effects(state: &mut GameState, departed_id: ObjectId) {
    let before = state.transient_continuous_effects.len();
    state
        .transient_continuous_effects
        .retain(|e| !(e.duration == Duration::UntilHostLeavesPlay && e.source_id == departed_id));
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty = true;
    }
}

/// Remove transient effects bound to a specific affected object that has left the battlefield.
pub fn prune_affected_object_left_effects(state: &mut GameState, departed_id: ObjectId) {
    let before = state.transient_continuous_effects.len();
    state.transient_continuous_effects.retain(|effect| {
        !matches!(effect.affected, TargetFilter::SpecificObject { id } if id == departed_id)
    });
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty = true;
    }
}

/// Evaluate a `StaticCondition` for the given controller.
/// Returns `true` if the condition is met (effect should apply), `false` otherwise.
///
/// Used by both intrinsic (permanent-based) and transient (state-level) continuous
/// effects so that condition evaluation is consistent regardless of effect origin.
/// Evaluate a `StaticCondition` for the given controller and source object.
/// Returns `true` if the condition is met (effect should apply), `false` otherwise.
///
/// Used by both intrinsic (permanent-based) and transient (state-level) continuous
/// effects so that condition evaluation is consistent regardless of effect origin.
pub(crate) fn evaluate_condition(
    state: &GameState,
    condition: &StaticCondition,
    controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    match condition {
        StaticCondition::DevotionGE { colors, threshold } => {
            count_devotion(state, controller, colors) >= *threshold
        }
        StaticCondition::IsPresent { filter } => match filter {
            Some(f) => {
                let ctx = FilterContext::from_source(state, source_id);
                state
                    .objects
                    .keys()
                    .any(|&id| matches_target_filter(state, id, f, &ctx))
            }
            None => true,
        },
        StaticCondition::ChosenColorIs { color } => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.chosen_color())
            .is_some_and(|chosen| &chosen == color),
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => {
            let resolve = |expr: &QuantityExpr| -> i32 {
                crate::game::quantity::resolve_quantity(state, expr, controller, source_id)
            };
            comparator.evaluate(resolve(lhs), resolve(rhs))
        }
        StaticCondition::HasMaxSpeed => has_max_speed(state, controller),
        StaticCondition::SpeedGE { threshold } => effective_speed(state, controller) >= *threshold,
        StaticCondition::And { conditions } => conditions
            .iter()
            .all(|c| evaluate_condition(state, c, controller, source_id)),
        StaticCondition::Or { conditions } => conditions
            .iter()
            .any(|c| evaluate_condition(state, c, controller, source_id)),
        StaticCondition::Not { condition } => {
            !evaluate_condition(state, condition, controller, source_id)
        }
        // CR 122.1: Check counters on the source object, with optional maximum.
        // `CounterMatch::Any` sums across every counter type (for bare "a counter on
        // it" text); `CounterMatch::OfType(ct)` matches a specific counter type.
        StaticCondition::HasCounters {
            counters,
            minimum,
            maximum,
        } => state
            .objects
            .get(&source_id)
            .map(|obj| {
                let count: u32 = match counters {
                    CounterMatch::Any => obj.counters.values().sum(),
                    CounterMatch::OfType(ct) => obj.counters.get(ct).copied().unwrap_or(0),
                };
                count >= *minimum && maximum.is_none_or(|max| count <= max)
            })
            .unwrap_or(false),
        // CR 716.3: Level abilities are active at or above the specified level.
        StaticCondition::ClassLevelGE { level } => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.class_level)
            .is_some_and(|current| current >= *level),
        StaticCondition::Unrecognized { .. } => true,
        StaticCondition::DuringYourTurn => state.active_player == controller,
        // CR 400.7: True when the source permanent entered the battlefield this turn.
        StaticCondition::SourceEnteredThisTurn => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.entered_battlefield_turn == Some(state.turn_number)),
        // CR 701.54a: True when this creature is the ring-bearer for its controller.
        StaticCondition::IsRingBearer => state
            .ring_bearer
            .get(&controller)
            .is_some_and(|bearer| *bearer == Some(source_id)),
        // CR 701.54c: True when the controller's ring level is at least this value.
        StaticCondition::RingLevelAtLeast { level } => {
            state.ring_level.get(&controller).copied().unwrap_or(0) >= *level
        }
        // CR 611.2b: True when the source object is tapped.
        StaticCondition::SourceIsTapped => {
            state.objects.get(&source_id).is_some_and(|obj| obj.tapped)
        }
        // CR 702.62a + CR 611.2b: True when the source object's current controller
        // equals the stored player. Drives the Suspend haste duration: when a
        // suspended creature spell resolves, a transient continuous effect with
        // `Duration::ForAsLongAs { SourceControllerEquals { resolution_controller } }`
        // grants haste; a Threaten / Mind Control swap moves controller and
        // this predicate flips false, naturally lapsing the static.
        StaticCondition::SourceControllerEquals { player } => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.controller == *player),
        // CR 301.5a: True when at least one Equipment is attached to the source object.
        // Mirrors the attacher-is-equipment subtype check from `effects/attach.rs:64-67`.
        // CR 301.5: Equipment can only attach to objects, so any non-Object host
        // is rejected by `as_object`.
        StaticCondition::SourceIsEquipped => state.objects.values().any(|obj| {
            obj.attached_to.and_then(|t| t.as_object()) == Some(source_id)
                && obj.card_types.subtypes.iter().any(|s| s == "Equipment")
        }),
        // CR 701.37: True when the source permanent is monstrous.
        StaticCondition::SourceIsMonstrous => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.monstrous),
        // CR 301.5 + CR 303.4: True when source Aura/Equipment is attached to a
        // creature. A Player host (CR 303.4 + CR 702.5d) is never a creature, so
        // we filter to Object hosts via `as_object`.
        StaticCondition::SourceAttachedToCreature => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.attached_to)
            .and_then(|t| t.as_object())
            .and_then(|target_id| state.objects.get(&target_id))
            .is_some_and(|target| {
                target
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Creature)
            }),
        // CR 113.6b: True when the source card is in the specified zone.
        StaticCondition::SourceInZone { zone } => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.zone == *zone),
        // CR 708.2 + CR 707.2: True when the creature this Aura/Equipment is attached to
        // is face-down. Traverses `attached_to` to the target object and reads its
        // `face_down` status (false if source is not attached, or attached to a
        // player — players have no face-down state).
        StaticCondition::EnchantedIsFaceDown => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.attached_to)
            .and_then(|t| t.as_object())
            .and_then(|target_id| state.objects.get(&target_id))
            .is_some_and(|target| target.face_down),
        // CR 608.2c: Check if the source object matches a type filter (leveler gates).
        StaticCondition::SourceMatchesFilter { filter } => matches_target_filter(
            state,
            source_id,
            filter,
            &FilterContext::from_source(state, source_id),
        ),
        // CR 509.1b: True when the defending player controls a permanent matching the filter.
        // Only meaningful during combat — finds the defending player from the source's
        // attacker info in the CombatState.
        StaticCondition::DefendingPlayerControls { filter } => state
            .combat
            .as_ref()
            .and_then(|combat| {
                combat
                    .attackers
                    .iter()
                    .find(|a| a.object_id == source_id)
                    .map(|a| a.defending_player)
            })
            .is_some_and(|defending| {
                let ctx = FilterContext::from_source(state, source_id);
                state.objects.values().any(|obj| {
                    obj.controller == defending
                        && matches_target_filter(state, obj.id, filter, &ctx)
                })
            }),
        // CR 506.5: True when the source creature is the only attacking creature.
        StaticCondition::SourceAttackingAlone => state.combat.as_ref().is_some_and(|combat| {
            combat.attackers.len() == 1
                && combat
                    .attackers
                    .first()
                    .is_some_and(|a| a.object_id == source_id)
        }),
        // CR 508.1k: Source creature is currently an attacker.
        StaticCondition::SourceIsAttacking => state
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == source_id)),
        // CR 509.1g: Source creature is currently a blocker.
        StaticCondition::SourceIsBlocking => state
            .combat
            .as_ref()
            .is_some_and(|combat| combat.blocker_to_attacker.contains_key(&source_id)),
        // CR 509.1h: Source creature has been blocked this combat (sticky flag).
        StaticCondition::SourceIsBlocked => state.combat.as_ref().is_some_and(|combat| {
            combat
                .attackers
                .iter()
                .find(|a| a.object_id == source_id)
                .is_some_and(|a| a.blocked)
        }),
        // CR 725.1: True when the controller is the monarch.
        StaticCondition::IsMonarch => state.monarch == Some(controller),
        // CR 702.131a: True when the controller has the city's blessing.
        StaticCondition::HasCityBlessing => state.city_blessing.contains(&controller),
        StaticCondition::OpponentPoisonAtLeast { count } => state
            .players
            .iter()
            .filter(|player| player.id != controller)
            .any(|player| player.poison_counters >= *count),
        // CR 118.12a: "unless pays" conditions evaluate as false (restriction active).
        // This is a conservative but rules-correct default for cards like Ghostly
        // Prison: absent a per-attacker/per-blocker optional cost payment round-trip
        // (WaitingFor::PayAttackTax / PayBlockTax), the player has not paid, so the
        // restriction remains active. Making the payment optional is a full
        // interactive feature tracked separately from the static-stub cleanup.
        StaticCondition::UnlessPay { .. } => false,
        StaticCondition::None => true,
        // CR 309.7: True when the controller has completed at least one dungeon.
        StaticCondition::CompletedADungeon => state
            .dungeon_progress
            .get(&controller)
            .is_some_and(|p| !p.completed.is_empty()),
        // CR 903.3: True when the controller controls at least one of their commanders.
        StaticCondition::ControlsCommander => state.battlefield.iter().any(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.controller == controller && obj.is_commander)
        }),
    }
}

/// Test-only wrapper to expose `evaluate_condition` for unit tests in other modules.
#[cfg(test)]
pub fn evaluate_condition_for_test(
    state: &GameState,
    condition: &StaticCondition,
    controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    evaluate_condition(state, condition, controller, source_id)
}

/// Evaluate all continuous effects through the seven-layer system.
///
/// 1. Reset computed characteristics to base values.
/// 2. Gather all active continuous effects from battlefield permanents.
/// 3. For each layer, filter/order effects and apply them.
/// 4. Apply counter-based P/T modifications (layer 7e).
/// 5. Clear the layers_dirty flag.
///
/// CR 613.1: Evaluate all continuous effects in layer order (1–7e).
pub fn evaluate_layers(state: &mut GameState) {
    // CR 302.6 + CR 613.1b + CR 702.26c: Snapshot effective controllers for
    // phased-in permanents BEFORE the Step 1 reset below wipes them. The
    // post-pass diff at the end of this function compares against this
    // snapshot to detect effective-controller transitions (Layer 2 control-
    // changing effect start/end, exchange-control, gain-control expiry) and
    // re-applies summoning sickness per CR 302.6 ("continuously under that
    // player's control since that player's most recent turn began").
    // Phased-out permanents are excluded per CR 702.26c.
    let prev_controllers: Vec<(ObjectId, PlayerId)> = state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter_map(|id| state.objects.get(&id).map(|o| (id, o.controller)))
        .collect();

    // Step 1: Reset computed characteristics to base values.
    // Only reset fields where base values were explicitly set; objects without
    // base values (e.g., from older test helpers) retain their current values.
    let bf_ids: Vec<ObjectId> = state.battlefield.iter().copied().collect();
    for &id in &bf_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.sync_missing_base_characteristics();
            obj.name = obj.base_name.clone();
            obj.power = obj.base_power;
            obj.toughness = obj.base_toughness;
            obj.loyalty = obj.base_loyalty;
            obj.card_types = obj.base_card_types.clone();
            obj.mana_cost = obj.base_mana_cost.clone();
            obj.keywords = obj.base_keywords.clone();
            // CR 613.1: Reset live ability fields to the printed-card baseline.
            // Post Commit 2 of Arc-share migration: both sides are `Arc<Vec<T>>`
            // (via `Definitions<T>`-holds-`Arc`), so this reset is a refcount
            // bump — no deep copy of ability data per layer pass per permanent.
            // Subsequent layer effects that mutate `obj.abilities` / definitions
            // trigger copy-on-write via `Arc::make_mut`.
            obj.abilities = Arc::clone(&obj.base_abilities);
            obj.trigger_definitions = Arc::clone(&obj.base_trigger_definitions).into();
            obj.replacement_definitions = Arc::clone(&obj.base_replacement_definitions).into();
            obj.static_definitions = Arc::clone(&obj.base_static_definitions).into();
            obj.color = obj.base_color.clone();
            // CR 613.1b: Reset controller to owner; Layer 2 re-applies control-changing effects.
            obj.controller = obj.owner;
            // CR 613: Reset damage-from-toughness flag; re-applied by continuous effects.
            obj.assigns_damage_from_toughness = false;
            // CR 510.1c: Reset unblocked-assignment flag; re-applied by continuous effects.
            obj.assigns_damage_as_though_unblocked = false;
            // CR 510.1a: Reset no-combat-damage flag; re-applied by continuous effects.
            obj.assigns_no_combat_damage = false;
        }
    }
    // CR 702.94a + CR 400.3: Hand-zone continuous effects (Lorehold-style
    // "Each [filter] card in your hand has [keyword]") grant keywords to hand
    // objects. Reset those hand objects' keywords to their base set each layers
    // pass so hand-zone grants don't accumulate across evaluations. Scoped
    // narrowly to `keywords` because A6 only supports keyword grants to hand
    // objects; other characteristics (P/T, types, abilities) are not granted to
    // hand objects by any currently-supported static. Extend this reset set
    // before landing a static that modifies them.
    let hand_ids: Vec<ObjectId> = state
        .players
        .iter()
        .flat_map(|p| p.hand.iter().copied())
        .collect();
    for id in hand_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.sync_missing_base_characteristics();
            obj.keywords = obj.base_keywords.clone();
        }
    }

    // Step 2: Apply copy effects first so copied static abilities exist before later layers.
    let copy_effects = gather_active_effects_for_layer(state, Layer::Copy);
    let ordered_copy = order_active_continuous_effects(Layer::Copy, &copy_effects, state);
    for effect in &ordered_copy {
        apply_continuous_effect(state, effect);
    }

    // Step 3: Gather active continuous effects after layer 1 is applied.
    let effects_by_layer = gather_active_continuous_effects(state);

    // Step 4: Process each remaining layer in order
    for (layer, layer_bucket) in &effects_by_layer {
        if matches!(*layer, Layer::Copy | Layer::CounterPT) {
            // Copy handled above; Counter-based P/T handled separately below.
            continue;
        }

        if layer_bucket.is_empty() {
            continue;
        }
        let layer_effects: Vec<&ActiveContinuousEffect> = layer_bucket.iter().collect();

        let ordered = if layer.has_dependency_ordering() {
            order_with_dependencies(&layer_effects, state)
        } else {
            order_by_timestamp(&layer_effects)
        };

        for effect in &ordered {
            apply_continuous_effect(state, effect);
        }
    }

    // CR 702.73a: Changeling — object has all creature types.
    // Step 3b: Changeling post-fixup — if Changeling was granted via AddKeyword
    // in Layer 6 (Ability), the CDA in Layer 4 (Type) was already processed.
    // Expand creature types for any object that now has Changeling but wasn't
    // covered by its own CDA static definition.
    if !state.all_creature_types.is_empty() {
        for &id in &bf_ids {
            let has_changeling = state
                .objects
                .get(&id)
                .is_some_and(|o| o.has_keyword(&Keyword::Changeling));
            if has_changeling {
                let all_types = state.all_creature_types.clone();
                if let Some(obj) = state.objects.get_mut(&id) {
                    for subtype in &all_types {
                        if !obj.card_types.subtypes.iter().any(|s| s == subtype) {
                            obj.card_types.subtypes.push(subtype.clone());
                        }
                    }
                }
            }
        }
    }

    // CR 122.1b + CR 613.1f: Keyword counters grant their keyword at layer 6.
    // The CR enumerates the keyword counters a permanent can gain a keyword from:
    // flying, first strike, double strike, deathtouch, decayed, exalted, haste,
    // hexproof, indestructible, lifelink, menace, reach, shadow, trample, and
    // vigilance (and variants). Anything outside this list is a flavor counter
    // without a runtime effect — e.g. a "charge" counter doesn't grant Charge.
    for &id in &bf_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            let granted: Vec<Keyword> = obj
                .counters
                .keys()
                .filter_map(|ct| match ct {
                    CounterType::Keyword(kind) => Keyword::promote_keyword_kind(*kind),
                    _ => None,
                })
                .collect();
            for keyword in granted {
                if !obj.has_keyword(&keyword) {
                    obj.keywords.push(keyword);
                }
            }
        }
    }

    // CR 613.4c: +1/+1 and -1/-1 counters modify P/T in layer 7c.
    for &id in &bf_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            let plus = crate::game::arithmetic::u32_to_i32_saturating(
                *obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0),
            );
            let minus = crate::game::arithmetic::u32_to_i32_saturating(
                *obj.counters.get(&CounterType::Minus1Minus1).unwrap_or(&0),
            );
            let delta = plus.saturating_sub(minus);
            if delta != 0 {
                if let Some(ref mut p) = obj.power {
                    *p = saturating_pt_add(*p, delta);
                }
                if let Some(ref mut t) = obj.toughness {
                    *t = saturating_pt_add(*t, delta);
                }
            }

            // CR 306.5b: Loyalty is tracked via loyalty counters. After the layer reset
            // reverts obj.loyalty to base_loyalty, re-derive it from the actual counter.
            if let Some(&loyalty_counters) = obj.counters.get(&CounterType::Loyalty) {
                obj.loyalty = Some(loyalty_counters);
            }
        }
    }

    // CR 302.6: Re-apply summoning sickness for any permanent whose effective
    // controller changed during this evaluation. The diff is taken against
    // `prev_controllers` snapshotted at the top of the function. Layer 2
    // (CR 613.1b) is the single authority for post-ETB control changes, so
    // every relevant transition — Act of Treason / Threaten cast and expiry,
    // Control Magic / Mind Control on/off, exchange-control, "until end of
    // combat" duration termination — produces a diff here. Newly-ETB'd
    // permanents are absent from the snapshot and therefore unaffected
    // (their `summoning_sick` was set true upstream by
    // `GameObject::reset_for_battlefield_entry`). Clearing back to false is
    // the sole responsibility of `turns::start_next_turn` for the active
    // player's permanents.
    for (id, prev) in prev_controllers {
        if let Some(obj) = state.objects.get_mut(&id) {
            if obj.controller != prev {
                obj.summoning_sick = true;
            }
        }
    }

    // Step 5: Clear dirty flag
    state.layers_dirty = false;
}

fn gather_active_effects_for_layer(state: &GameState, layer: Layer) -> Vec<ActiveContinuousEffect> {
    collect_shared_active_continuous_effects(state)
        .into_iter()
        .filter(|effect| effect.layer == layer)
        .collect()
}

/// Collect all active continuous effects from permanents on the battlefield.
/// CR 613.1: Gather all active continuous effects for layer evaluation.
fn gather_active_continuous_effects(
    state: &GameState,
) -> Vec<(Layer, Vec<ActiveContinuousEffect>)> {
    let mut effects: Vec<(Layer, Vec<ActiveContinuousEffect>)> = Layer::all()
        .iter()
        .map(|&layer| (layer, Vec::new()))
        .collect();

    for effect in collect_shared_active_continuous_effects(state) {
        push_effect(&mut effects, effect.layer, effect);
    }

    effects
}

pub(crate) fn collect_shared_active_continuous_effects(
    state: &GameState,
) -> Vec<ActiveContinuousEffect> {
    let mut effects = Vec::new();

    // CR 702.26e: Continuous effects generated by phased-out permanents don't
    // include anything in their set of affected objects — effectively, a
    // phased-out permanent contributes no continuous effects during layer
    // evaluation. Skip phased-out sources here rather than filtering later.
    for &id in &state.battlefield {
        if state
            .objects
            .get(&id)
            .is_some_and(|obj| obj.is_phased_out())
        {
            continue;
        }
        effects.extend(active_continuous_effects_from_source_id(state, id));
    }

    // CR 114.3: Emblems in the command zone have static abilities that affect the game.
    for &id in &state.command_zone {
        let Some(obj) = state.objects.get(&id) else {
            continue;
        };
        if obj.is_emblem {
            effects.extend(active_continuous_effects_from_static_source(state, obj));
        }
    }

    // CR 113.6 + CR 113.6b: Statics that opt into non-battlefield functional
    // zones (Incarnation cycle — Anger/Filth/Brawn/Wonder/Valor — "as long as
    // this card is in your graveyard, ...") must be collected from wherever the
    // source currently lives. `active_continuous_effects_from_static_definitions`
    // applies the zone-of-function gate per-static, so scanning every object
    // outside the battlefield / command-zone passes already covered above is
    // safe: battlefield-default statics filter themselves out.
    for obj in state.objects.values() {
        // Battlefield objects were already processed above (phased-out gate
        // included). Command-zone emblems were handled above; non-emblem
        // command-zone objects never function (CR 114.4).
        match obj.zone {
            crate::types::zones::Zone::Battlefield | crate::types::zones::Zone::Command => continue,
            _ => {}
        }
        if obj.is_phased_out() {
            continue;
        }
        // Cheap pre-check: only scan objects that carry at least one
        // opt-in-zone static. Avoids iterating libraries/hands full of
        // ordinary cards on every layer recomputation.
        if !obj
            .static_definitions
            .iter_all()
            .any(|def| !def.active_zones.is_empty())
        {
            continue;
        }
        effects.extend(active_continuous_effects_from_static_source(state, obj));
    }

    gather_transient_continuous_effects(state, &mut effects);
    effects
}

pub(crate) fn active_continuous_effects_from_source_id(
    state: &GameState,
    source_id: ObjectId,
) -> Vec<ActiveContinuousEffect> {
    state
        .objects
        .get(&source_id)
        .map(|obj| active_continuous_effects_from_static_source(state, obj))
        .unwrap_or_default()
}

pub(crate) fn active_continuous_effects_from_static_source(
    state: &GameState,
    source: &crate::game::game_object::GameObject,
) -> Vec<ActiveContinuousEffect> {
    active_continuous_effects_from_static_definitions(
        state,
        source.id,
        source.controller,
        source.timestamp,
        source.static_definitions.as_slice(),
    )
}

pub(crate) fn active_continuous_effects_from_base_static_source(
    state: &GameState,
    source: &crate::game::game_object::GameObject,
) -> Vec<ActiveContinuousEffect> {
    active_continuous_effects_from_static_definitions(
        state,
        source.id,
        source.controller,
        source.timestamp,
        &source.base_static_definitions,
    )
}

fn active_continuous_effects_from_static_definitions(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    timestamp: u64,
    static_definitions: &[StaticDefinition],
) -> Vec<ActiveContinuousEffect> {
    let mut effects = Vec::new();
    // CR 113.6 + CR 113.6b: A static's functional zone is the battlefield by
    // default (empty `active_zones`). A non-empty `active_zones` lists the
    // non-battlefield zones in which the static functions (e.g., Incarnation
    // cycle: "as long as this card is in your graveyard, ..."). If the source
    // is currently outside every declared zone, the static contributes no
    // effects.
    let source_zone = state.objects.get(&source_id).map(|o| o.zone);
    for (def_idx, def) in static_definitions.iter().enumerate() {
        if def.mode != StaticMode::Continuous {
            continue;
        }

        // CR 113.6 + CR 113.6b: Zone-of-function gate.
        if !def.active_zones.is_empty() {
            let Some(zone) = source_zone else { continue };
            if !def.active_zones.contains(&zone) {
                continue;
            }
        }

        if let Some(ref condition) = def.condition {
            if !evaluate_condition(state, condition, controller, source_id) {
                continue;
            }
        }

        let affected_filter = def.affected.clone().unwrap_or(TargetFilter::Any);
        for modification in &def.modifications {
            effects.push(ActiveContinuousEffect {
                source_id,
                controller,
                def_index: Some(def_idx),
                layer: modification.layer(),
                timestamp,
                modification: modification.clone(),
                affected_filter: affected_filter.clone(),
                mode: def.mode.clone(),
                characteristic_defining: def.characteristic_defining,
            });
        }
    }

    effects
}

/// Collect active transient effects, filtering out expired host-bound effects.
pub(crate) fn gather_transient_continuous_effects(
    state: &GameState,
    effects: &mut Vec<ActiveContinuousEffect>,
) {
    for tce in &state.transient_continuous_effects {
        // UntilHostLeavesPlay: skip if source is no longer on the battlefield
        if tce.duration == Duration::UntilHostLeavesPlay
            && !state
                .objects
                .get(&tce.source_id)
                .is_some_and(|obj| obj.zone == crate::types::zones::Zone::Battlefield)
        {
            continue;
        }

        // CR 611.2b: ForAsLongAs durations embed a condition that must hold each layer cycle.
        if let Duration::ForAsLongAs { ref condition } = tce.duration {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }

        // Evaluate condition using the shared evaluator
        if let Some(ref condition) = tce.condition {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }

        for modification in &tce.modifications {
            effects.push(ActiveContinuousEffect {
                source_id: tce.source_id,
                controller: tce.controller,
                def_index: None,
                layer: modification.layer(),
                timestamp: tce.timestamp,
                modification: modification.clone(),
                affected_filter: tce.affected.clone(),
                mode: StaticMode::Continuous,
                characteristic_defining: false,
            });
        }
    }
}

#[allow(clippy::ptr_arg)]
fn push_effect(
    effects: &mut Vec<(Layer, Vec<ActiveContinuousEffect>)>,
    layer: Layer,
    effect: ActiveContinuousEffect,
) {
    if let Some((_, bucket)) = effects
        .iter_mut()
        .find(|(bucket_layer, _)| *bucket_layer == layer)
    {
        bucket.push(effect);
    } else {
        effects.push((layer, vec![effect]));
    }
}

/// Order effects using dependency-aware topological sort.
/// CR 613.8: Dependency ordering for continuous effects.
fn order_with_dependencies(
    effects: &[&ActiveContinuousEffect],
    state: &GameState,
) -> Vec<ActiveContinuousEffect> {
    if effects.len() <= 1 {
        return effects.iter().map(|e| (*e).clone()).collect();
    }

    // CR 613.7a: Effects in the same layer apply in timestamp order.
    // CR 613.3: Within layers 2-6, apply effects from CDAs first (see CR 604.3), then others in timestamp order.
    let mut sorted: Vec<&ActiveContinuousEffect> = effects.to_vec();
    sorted.sort_by_key(|e| {
        (
            !e.characteristic_defining,
            e.timestamp,
            e.source_id.0,
            e.def_index,
        )
    });

    let mut dependencies: Vec<Vec<usize>> = vec![Vec::new(); sorted.len()];
    let mut in_degree = vec![0usize; sorted.len()];
    for i in 0..sorted.len() {
        for j in 0..sorted.len() {
            if i == j {
                continue;
            }
            if depends_on(sorted[i], sorted[j], state) {
                dependencies[j].push(i);
                in_degree[i] += 1;
            }
        }
    }

    let mut ordered = Vec::with_capacity(sorted.len());
    let mut processed = vec![false; sorted.len()];

    while ordered.len() < sorted.len() {
        let Some(next) = (0..sorted.len()).find(|&idx| !processed[idx] && in_degree[idx] == 0)
        else {
            // CR 613.8b: Dependency cycle — fall back to timestamp ordering.
            return sorted.iter().map(|effect| (*effect).clone()).collect();
        };

        processed[next] = true;
        ordered.push(sorted[next].clone());
        for &dependent in &dependencies[next] {
            in_degree[dependent] = in_degree[dependent].saturating_sub(1);
        }
    }

    ordered
}

pub(crate) fn order_active_continuous_effects(
    layer: Layer,
    effects: &[ActiveContinuousEffect],
    state: &GameState,
) -> Vec<ActiveContinuousEffect> {
    let effect_refs: Vec<&ActiveContinuousEffect> = effects.iter().collect();
    if layer.has_dependency_ordering() {
        order_with_dependencies(&effect_refs, state)
    } else {
        order_by_timestamp(&effect_refs)
    }
}

/// Check if effect `a` depends on effect `b`.
/// If `b` changes types and `a`'s filter is type-based, `a` depends on `b`.
fn depends_on(a: &ActiveContinuousEffect, b: &ActiveContinuousEffect, _state: &GameState) -> bool {
    if matches!(b.modification, ContinuousModification::CopyValues { .. }) {
        return true;
    }

    // If b changes types (AddType/RemoveType) and a's filter references a type
    let b_changes_types = matches!(
        &b.modification,
        ContinuousModification::AddType { .. }
            | ContinuousModification::RemoveType { .. }
            | ContinuousModification::AddSubtype { .. }
            | ContinuousModification::RemoveSubtype { .. }
            | ContinuousModification::AddSupertype { .. }
            | ContinuousModification::RemoveSupertype { .. }
            | ContinuousModification::AddAllCreatureTypes
            | ContinuousModification::AddAllBasicLandTypes
            | ContinuousModification::AddChosenSubtype { .. }
            | ContinuousModification::SetBasicLandType { .. }
    );

    if b_changes_types && filter_references_type(&a.affected_filter) {
        return true;
    }

    // If b adds/removes abilities and a's filter checks for abilities
    let b_changes_abilities = matches!(
        &b.modification,
        ContinuousModification::AddKeyword { .. }
            | ContinuousModification::RemoveKeyword { .. }
            | ContinuousModification::AddDynamicKeyword { .. }
            | ContinuousModification::GrantAbility { .. }
            | ContinuousModification::GrantTrigger { .. }
            | ContinuousModification::RemoveAllAbilities
            | ContinuousModification::AddStaticMode { .. }
            | ContinuousModification::RetainPrintedTriggerFromSource { .. }
    );

    if b_changes_abilities && filter_references_ability(&a.affected_filter) {
        return true;
    }

    false
}

/// Check if a TargetFilter references a card type (used for dependency ordering).
fn filter_references_type(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter { type_filters, .. }) => !type_filters.is_empty(),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_references_type)
        }
        TargetFilter::Not { filter } => filter_references_type(filter),
        _ => false,
    }
}

/// Check if a TargetFilter references abilities/keywords (used for dependency ordering).
fn filter_references_ability(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => properties.iter().any(|p| {
            matches!(
                p,
                crate::types::ability::FilterProp::WithKeyword { .. }
                    | crate::types::ability::FilterProp::HasKeywordKind { .. }
                    | crate::types::ability::FilterProp::WithoutKeyword { .. }
                    | crate::types::ability::FilterProp::WithoutKeywordKind { .. }
            )
        }),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_references_ability)
        }
        TargetFilter::Not { filter } => filter_references_ability(filter),
        _ => false,
    }
}

/// Order effects by timestamp (deterministic fallback). CDAs sort first per CR 604.3.
fn order_by_timestamp(effects: &[&ActiveContinuousEffect]) -> Vec<ActiveContinuousEffect> {
    let mut sorted: Vec<ActiveContinuousEffect> = effects.iter().map(|e| (*e).clone()).collect();
    sorted.sort_by_key(|e| {
        (
            !e.characteristic_defining,
            e.timestamp,
            e.source_id.0,
            e.def_index,
        )
    });
    sorted
}

/// Apply a single continuous effect to all affected objects.
///
/// CR 400.3 + CR 702.94a: The filter's `InZone` property (via
/// `TargetFilter::extract_in_zone`) selects which zone's objects are scanned.
/// Absence of `InZone` defaults to the battlefield (current behavior). This
/// supports non-battlefield grant statics like Lorehold's "Each instant and
/// sorcery card in your hand has miracle {2}", whose filter carries
/// `InZone { zone: Hand }`.
/// CR 613.4c: True when the QuantityExpr's filter tree contains
/// `FilterProp::AttachedToRecipient` — the per-recipient pronoun "it" referent
/// in "for each X attached to it" clauses on Aura/Equipment statics. When
/// true, dynamic P/T resolution must run inside the per-recipient loop so
/// the count is computed against each affected creature, not against the
/// static's source object.
fn quantity_expr_uses_recipient(expr: &crate::types::ability::QuantityExpr) -> bool {
    use crate::types::ability::{QuantityExpr, QuantityRef};
    match expr {
        QuantityExpr::Fixed { .. } => false,
        QuantityExpr::Ref { qty } => match qty {
            QuantityRef::ObjectCount { filter }
            | QuantityRef::ObjectCountDistinctNames { filter } => filter_uses_recipient(filter),
            _ => false,
        },
        QuantityExpr::HalfRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_expr_uses_recipient(inner),
        QuantityExpr::Sum { exprs } => exprs.iter().any(quantity_expr_uses_recipient),
    }
}

/// Recursively check whether a `TargetFilter` carries
/// `FilterProp::AttachedToRecipient` anywhere in its property tree. Mirrors
/// `filter_contains_other_than_trigger_object` in `quantity.rs`.
fn filter_uses_recipient(filter: &crate::types::ability::TargetFilter) -> bool {
    use crate::types::ability::{FilterProp, TargetFilter};
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::AttachedToRecipient)),
        TargetFilter::Not { filter: inner } => filter_uses_recipient(inner),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_uses_recipient)
        }
        _ => false,
    }
}

fn apply_continuous_effect(state: &mut GameState, effect: &ActiveContinuousEffect) {
    let scan_zone = effect
        .affected_filter
        .extract_in_zone()
        .unwrap_or(crate::types::zones::Zone::Battlefield);
    let scan_ids = super::targeting::zone_object_ids(state, scan_zone);
    let ctx = FilterContext::from_source(state, effect.source_id);
    let affected_ids: Vec<ObjectId> = scan_ids
        .iter()
        .filter(|&&id| matches_target_filter(state, id, &effect.affected_filter, &ctx))
        .copied()
        .collect();

    // Pre-read chosen subtype from source (avoids borrow conflict in the loop)
    let chosen_subtype =
        if let ContinuousModification::AddChosenSubtype { ref kind } = effect.modification {
            state
                .objects
                .get(&effect.source_id)
                .and_then(|src| src.chosen_subtype_str(kind))
        } else {
            None
        };

    // Pre-read chosen color from source (avoids borrow conflict in the loop)
    let chosen_color = if matches!(effect.modification, ContinuousModification::AddChosenColor) {
        state
            .objects
            .get(&effect.source_id)
            .and_then(|src| src.chosen_color())
    } else {
        None
    };

    // CR 613.1b: For Layer 2 ChangeController, the new controller is the effect's
    // own `controller` field — set authoritatively by the effect that queued the
    // continuous modification (e.g. gain_control passes `ability.controller`,
    // exchange_control passes the swapped controller per slot). Reading it from
    // `state.objects.get(effect.source_id).controller` would be wrong for any
    // case where source ≠ recipient (e.g. Switcheroo: both transient effects
    // share one source, but each slot needs the opposite controller).

    // Pre-compute dynamic P/T values (avoids borrow conflict in the loop).
    //
    // CR 613.4c: Most dynamic modifications resolve to a single value shared
    // across every affected object — the static's source is the natural
    // referent. The exception is the "<subject> gets +N/+M for each X
    // attached to it" family (Strong Back, Mantle of the Ancients, Bruenor
    // Battlehammer, etc.), where the pronoun "it" is anaphoric on the
    // recipient creature, not the source. For that family we defer
    // resolution into the per-recipient loop below.
    let dynamic_pt_expr = match &effect.modification {
        ContinuousModification::SetDynamicPower { value }
        | ContinuousModification::SetDynamicToughness { value }
        | ContinuousModification::SetPowerDynamic { value }
        | ContinuousModification::SetToughnessDynamic { value }
        | ContinuousModification::AddDynamicPower { value }
        | ContinuousModification::AddDynamicToughness { value }
        | ContinuousModification::AddDynamicKeyword { value, .. } => Some(value),
        _ => None,
    };
    let effect_controller = state
        .objects
        .get(&effect.source_id)
        .map(|o| o.controller)
        .unwrap_or(PlayerId(0));
    let dynamic_uses_recipient = dynamic_pt_expr.is_some_and(quantity_expr_uses_recipient);
    let dynamic_pt_shared = match (dynamic_pt_expr, dynamic_uses_recipient) {
        (Some(value), false) => Some(crate::game::quantity::resolve_quantity(
            state,
            value,
            effect_controller,
            effect.source_id,
        )),
        _ => None,
    };

    // CR 707.9a: Pre-read the printed trigger to retain from the source object's
    // `base_trigger_definitions`. Reads here (immutable) before we take the
    // per-object mutable borrow inside the loop. Cloning out the trigger keeps
    // the dispatch arm's mutation site free of nested borrows.
    let retained_printed_trigger = if let ContinuousModification::RetainPrintedTriggerFromSource {
        source_trigger_index,
    } = &effect.modification
    {
        state.objects.get(&effect.source_id).and_then(|src| {
            src.base_trigger_definitions
                .get(*source_trigger_index)
                .cloned()
        })
    } else {
        None
    };

    for id in affected_ids {
        // CR 613.4c: When the dynamic modification's QuantityExpr counts
        // attachments on the recipient, resolve here under a recipient-bound
        // FilterContext. The immutable read finishes before the mutable
        // borrow of `obj` below.
        let dynamic_pt = if dynamic_uses_recipient {
            dynamic_pt_expr.map(|value| {
                crate::game::quantity::resolve_quantity_with_recipient(
                    state,
                    value,
                    effect_controller,
                    effect.source_id,
                    id,
                )
            })
        } else {
            dynamic_pt_shared
        };

        let obj = match state.objects.get_mut(&id) {
            Some(o) => o,
            None => continue,
        };

        match &effect.modification {
            ContinuousModification::CopyValues { values } => {
                apply_copiable_values(obj, values);
            }
            // CR 707.9b + CR 707.2: Name override is a copiable-value override
            // applied at Layer 1 after the base CopyValues (ordered by timestamp
            // within the layer, so the override in `additional_modifications`
            // follows `CopyValues` in `add_transient_continuous_effect`).
            ContinuousModification::SetName { name } => {
                obj.name = name.clone();
            }
            ContinuousModification::AddPower { value } => {
                if let Some(ref mut p) = obj.power {
                    *p = saturating_pt_add(*p, *value);
                }
            }
            ContinuousModification::AddToughness { value } => {
                if let Some(ref mut t) = obj.toughness {
                    *t = saturating_pt_add(*t, *value);
                }
            }
            ContinuousModification::SetPower { value } => {
                obj.power = Some(*value);
            }
            ContinuousModification::SetToughness { value } => {
                obj.toughness = Some(*value);
            }
            // CR 702.16g: "Protection from [A] and from [B]" behaves as two
            // separate protection abilities. Parameterized keywords like
            // `Protection(ColorWhite)` and `Protection(ColorBlue)` share an
            // enum discriminant, so a discriminant-based "already has" check
            // (`has_keyword`, see `keywords.rs::has_keyword`) would drop the
            // second grant. Use `Vec::contains` (PartialEq, exact match) so
            // each distinct parameter value is preserved on the keyword list.
            // CR 613.1f: This deduplication runs in Layer 6 alongside other
            // keyword-granting effects. Same shape applies to `Ward(_)`,
            // `Annihilator(_)`, `Cumulative Upkeep(_)`, and any other
            // parameterized keyword variant.
            ContinuousModification::AddKeyword { keyword } => {
                if !obj.keywords.contains(keyword) {
                    obj.keywords.push(keyword.clone());
                }
            }
            // Asymmetric on purpose: `RemoveKeyword` strips every keyword that
            // shares the same discriminant (e.g. "lose all flying"). The
            // current Oracle parser only emits unparameterized variants here,
            // so discriminant matching gives the intended "lose this kind of
            // ability" scope. If a future card requires "lose protection from
            // white but keep protection from blue," this arm needs to switch
            // to PartialEq alongside a new typed parser shape.
            ContinuousModification::RemoveKeyword { keyword } => {
                obj.keywords
                    .retain(|k| std::mem::discriminant(k) != std::mem::discriminant(keyword));
            }
            ContinuousModification::RemoveAllAbilities => {
                Arc::make_mut(&mut obj.abilities).clear();
                obj.trigger_definitions.clear();
                obj.replacement_definitions.clear();
                obj.static_definitions.clear();
                obj.keywords.clear();
            }
            ContinuousModification::AddType { core_type } => {
                if !obj.card_types.core_types.contains(core_type) {
                    obj.card_types.core_types.push(*core_type);
                }
            }
            ContinuousModification::RemoveType { core_type } => {
                obj.card_types.core_types.retain(|t| t != core_type);
            }
            ContinuousModification::SetColor { colors } => {
                obj.color = colors.clone();
            }
            ContinuousModification::AddColor { color } => {
                if !obj.color.contains(color) {
                    obj.color.push(*color);
                }
            }
            ContinuousModification::AddSubtype { ref subtype } => {
                if !obj.card_types.subtypes.iter().any(|s| s == subtype) {
                    obj.card_types.subtypes.push(subtype.clone());
                }
                // CR 305.6: A land that gains a basic land type also gains the
                // appropriate mana ability for that type. When a type is ADDED
                // (as opposed to replaced via SetBasicLandType), the land keeps
                // its existing abilities AND gains the basic mana ability.
                inject_basic_mana_ability_for_subtype(obj, subtype);
            }
            ContinuousModification::RemoveSubtype { ref subtype } => {
                obj.card_types.subtypes.retain(|s| s != subtype);
            }
            // CR 205.4 + CR 707.9d: "in addition to its other types" — append
            // the supertype if absent. Idempotent.
            ContinuousModification::AddSupertype { supertype } => {
                if !obj.card_types.supertypes.contains(supertype) {
                    obj.card_types.supertypes.push(*supertype);
                }
            }
            // CR 205.4 + CR 707.9b: "isn't legendary" / "isn't basic" copy
            // exception. Strip the supertype from the layered view.
            ContinuousModification::RemoveSupertype { supertype } => {
                obj.card_types.supertypes.retain(|s| s != supertype);
            }
            // CR 122.1 + CR 614.1c: One-shot counter placement is consumed at
            // copy resolution by token_copy::resolve / become_copy::resolve.
            // Reaching this arm means a wiring bug.
            ContinuousModification::AddCounterOnEnter { .. } => {
                debug_assert!(
                    false,
                    "AddCounterOnEnter must be consumed at resolution time, \
                     not via apply_continuous_effect"
                );
            }
            ContinuousModification::AddAllCreatureTypes => {
                for subtype in &state.all_creature_types {
                    if !obj.card_types.subtypes.iter().any(|s| s == subtype) {
                        obj.card_types.subtypes.push(subtype.clone());
                    }
                }
            }
            // CR 305.6 + CR 305.7: Add all five basic land types (additive).
            ContinuousModification::AddAllBasicLandTypes => {
                for land_type in BasicLandType::all() {
                    let subtype = land_type.as_subtype_str().to_string();
                    if !obj.card_types.subtypes.iter().any(|s| s == &subtype) {
                        obj.card_types.subtypes.push(subtype.clone());
                    }
                    // CR 305.6: Inject the basic mana ability for each basic
                    // land type that was added.
                    inject_basic_mana_ability_for_subtype(obj, &subtype);
                }
            }
            ContinuousModification::AddChosenSubtype { .. } => {
                if let Some(ref subtype) = chosen_subtype {
                    if !obj.card_types.subtypes.iter().any(|s| s == subtype) {
                        obj.card_types.subtypes.push(subtype.clone());
                    }
                    // CR 305.6: A chosen basic land type grants its mana ability.
                    inject_basic_mana_ability_for_subtype(obj, subtype);
                }
            }
            // CR 105.3: Set the object's color to the chosen color.
            ContinuousModification::AddChosenColor => {
                if let Some(color) = chosen_color {
                    obj.color = vec![color];
                }
            }
            ContinuousModification::SetDynamicPower { .. } => {
                if let Some(val) = dynamic_pt {
                    obj.power = Some(val);
                }
            }
            ContinuousModification::SetDynamicToughness { .. } => {
                if let Some(val) = dynamic_pt {
                    obj.toughness = Some(val);
                }
            }
            // CR 613.4b: Layer 7b — set base power to dynamic value (e.g., Biomass Mutation).
            ContinuousModification::SetPowerDynamic { .. } => {
                if let Some(val) = dynamic_pt {
                    obj.power = Some(val);
                }
            }
            // CR 613.4b: Layer 7b — set base toughness to dynamic value.
            ContinuousModification::SetToughnessDynamic { .. } => {
                if let Some(val) = dynamic_pt {
                    obj.toughness = Some(val);
                }
            }
            // CR 613.4c: Additive dynamic P/T modification (layer 7c).
            ContinuousModification::AddDynamicPower { .. } => {
                if let (Some(ref mut p), Some(val)) = (&mut obj.power, dynamic_pt) {
                    *p += val;
                }
            }
            ContinuousModification::AddDynamicToughness { .. } => {
                if let (Some(ref mut t), Some(val)) = (&mut obj.toughness, dynamic_pt) {
                    *t += val;
                }
            }
            ContinuousModification::AddDynamicKeyword { kind, .. } => {
                if let Some(val) = dynamic_pt {
                    let keyword = kind.with_value(val.max(0) as u32);
                    if !obj
                        .keywords
                        .iter()
                        .any(|k| std::mem::discriminant(k) == std::mem::discriminant(&keyword))
                    {
                        obj.keywords.push(keyword);
                    }
                }
            }
            // CR 613.1f: Layer 6 ability-granting effects are applied fresh
            // each layer pass (obj.abilities was reset to base_abilities at the
            // start of the pass). Within a single pass, a duplicate
            // GrantAbility — whether from a single static with repeated
            // modifications (e.g., Ragost parses the "have ..." clause twice)
            // or from multiple sources granting the same ability — must not
            // stack. Structural equality dedup keeps the grant idempotent.
            ContinuousModification::GrantAbility { definition } => {
                if !obj.abilities.iter().any(|a| a == definition.as_ref()) {
                    Arc::make_mut(&mut obj.abilities).push(*definition.clone());
                }
            }
            // CR 604.1: Push granted trigger to trigger_definitions so
            // the trigger's event matching and condition metadata is preserved.
            ContinuousModification::GrantTrigger { trigger } => {
                if !obj
                    .trigger_definitions
                    .iter_all()
                    .any(|t| t == trigger.as_ref())
                {
                    obj.trigger_definitions.push(*trigger.clone());
                }
            }
            ContinuousModification::AddStaticMode { mode } => {
                let def = StaticDefinition::new(mode.clone()).affected(TargetFilter::SelfRef);
                if !obj.static_definitions.iter_all().any(|sd| sd.mode == *mode) {
                    obj.static_definitions.push(def);
                }
            }
            // CR 613.4d: Switch power and toughness values.
            ContinuousModification::SwitchPowerToughness => {
                let (p, t) = (obj.power, obj.toughness);
                obj.power = t;
                obj.toughness = p;
            }
            // CR 613: Mark creature as assigning combat damage from toughness.
            ContinuousModification::AssignDamageFromToughness => {
                obj.assigns_damage_from_toughness = true;
            }
            // CR 510.1c: Mark creature as assigning combat damage as though unblocked.
            ContinuousModification::AssignDamageAsThoughUnblocked => {
                obj.assigns_damage_as_though_unblocked = true;
            }
            // CR 510.1a: Mark creature as assigning no combat damage.
            ContinuousModification::AssignNoCombatDamage => {
                obj.assigns_no_combat_damage = true;
            }
            // CR 613.1b: Change controller to the effect's own controller field.
            // See pre-loop comment for why we trust effect.controller (single
            // authority) rather than re-deriving from the source object.
            ContinuousModification::ChangeController => {
                obj.controller = effect.controller;
            }
            // CR 305.7: Setting a land's subtype removes all old land subtypes
            // (CR 205.3i) and all abilities generated from its rules text. Non-land
            // subtypes (e.g., creature subtypes on Land Creatures) are preserved.
            // Abilities granted by other effects are re-added in Layer 6.
            // Intrinsic mana abilities are derived from subtypes in mana_sources.rs.
            ContinuousModification::SetBasicLandType { land_type } => {
                obj.card_types.subtypes.retain(|s| !is_land_subtype(s));
                obj.card_types
                    .subtypes
                    .push(land_type.as_subtype_str().to_string());
                Arc::make_mut(&mut obj.abilities).clear();
                obj.trigger_definitions.clear();
                obj.replacement_definitions.clear();
                obj.static_definitions.clear();
                obj.keywords.clear();
            }
            // CR 707.9a: Retain the source's printed trigger on the copy.
            // After `CopyValues` overwrote `obj.trigger_definitions` with the
            // copied values, push the source's printed trigger back so the
            // copy retains "this ability". Idempotent — duplicate retain calls
            // (same trigger structurally) collapse into one.
            ContinuousModification::RetainPrintedTriggerFromSource { .. } => {
                if let Some(trigger) = retained_printed_trigger.clone() {
                    if !obj.trigger_definitions.iter_all().any(|t| t == &trigger) {
                        obj.trigger_definitions.push(trigger);
                    }
                }
            }
        }
    }
}

/// CR 305.6: Inject the intrinsic basic-land mana ability for `subtype` when it
/// is added additively to an object. No-op unless the object is a land and the
/// subtype names a basic land type. Idempotent: if the object already has an
/// activated mana ability producing only the matching color via a plain `{T}`
/// cost (whether printed or previously injected by another layer pass), the
/// injection is skipped so double-Urborg or Urborg + basic Swamp does not
/// duplicate the ability.
fn inject_basic_mana_ability_for_subtype(
    obj: &mut crate::game::game_object::GameObject,
    subtype: &str,
) {
    if !obj.card_types.core_types.contains(&CoreType::Land) {
        return;
    }
    let Ok(land_type) = subtype.parse::<BasicLandType>() else {
        return;
    };
    let color = land_type.mana_color();

    // Idempotent: skip if an existing `{T}: Add {color}` ability is already
    // present (base printed ability or a prior injection).
    let already_present = obj.abilities.iter().any(|ability| {
        if ability.kind != AbilityKind::Activated {
            return false;
        }
        if !matches!(ability.cost, Some(AbilityCost::Tap)) {
            return false;
        }
        matches!(
            &*ability.effect,
            Effect::Mana {
                produced: ManaProduction::Fixed { colors, .. },
                ..
            } if colors.as_slice() == [color]
        )
    });
    if already_present {
        return;
    }

    Arc::make_mut(&mut obj.abilities).push(
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![color],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap),
    );
}

pub(crate) fn compute_current_copiable_values(
    state: &GameState,
    object_id: ObjectId,
) -> Option<CopiableValues> {
    let obj = state.objects.get(&object_id)?;
    let mut values = intrinsic_copiable_values(obj);
    let mut copy_effects: Vec<ActiveContinuousEffect> =
        gather_active_effects_for_layer(state, Layer::Copy)
            .into_iter()
            .filter(|effect| {
                matches_target_filter(
                    state,
                    object_id,
                    &effect.affected_filter,
                    &FilterContext::from_source(state, effect.source_id),
                )
            })
            .collect();
    copy_effects = order_active_continuous_effects(Layer::Copy, &copy_effects, state);
    for effect in &copy_effects {
        match &effect.modification {
            ContinuousModification::CopyValues {
                values: effect_values,
            } => {
                values = (**effect_values).clone();
            }
            // CR 707.9b: Name overrides from "except its name is X" clauses
            // become part of the copiable values of the copy. A subsequent
            // copy of this object must see the overridden name, not the
            // source's name.
            ContinuousModification::SetName { name } => {
                values.name = name.clone();
            }
            // CR 707.9a: A copy effect that grants/retains an ability ("…
            // and it has this ability") makes that ability part of the
            // copiable values of the copy. A subsequent copy of this object
            // must see the retained trigger as one of its copiable triggers.
            // Read the printed trigger from the *effect's source object*
            // (the original printer of the retain modification — Irma) by
            // index, mirroring the write-side application in
            // `apply_continuous_effect`. Idempotent under stacking: a
            // structurally-equal trigger already present is not duplicated.
            ContinuousModification::RetainPrintedTriggerFromSource {
                source_trigger_index,
            } => {
                if let Some(trigger) = state.objects.get(&effect.source_id).and_then(|src| {
                    src.base_trigger_definitions
                        .get(*source_trigger_index)
                        .cloned()
                }) {
                    let triggers = Arc::make_mut(&mut values.trigger_definitions);
                    if !triggers.iter().any(|t| t == &trigger) {
                        triggers.push(trigger);
                    }
                }
            }
            _ => {}
        }
    }
    Some(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::GameScenario;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, BasicLandType, ChosenSubtypeKind, ContinuousModification,
        ControllerRef, CountScope, Duration, Effect, FilterProp, GainLifePlayer, QuantityExpr,
        QuantityRef, StaticCondition, StaticDefinition, TargetFilter, TypeFilter, ZoneRef,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::TransientContinuousEffect;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;
    use std::sync::Arc;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    fn make_creature(
        state: &mut GameState,
        name: &str,
        power: i32,
        toughness: i32,
        player: PlayerId,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(0),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.timestamp = ts;
        id
    }

    /// CR 613.4c + CR 704.5f: A runaway `+X/+X` chain (e.g. from a `ObjectCount`
    /// quantity resolving against an extremely large collection) must clamp at
    /// `i32::MAX` rather than wrapping to negative. If it wrapped, the creature's
    /// toughness would become `i32::MIN + delta`, state-based actions would see
    /// toughness ≤ 0, and the creature would die — a silent rules violation.
    #[test]
    fn saturating_pt_prevents_overflow_death_cascade() {
        let mut state = setup();
        let id = make_creature(&mut state, "Big Guy", 5, 5, PlayerId(0));

        // Stack two huge boosts whose naive sum overflows `i32`.
        let boost_a = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![
                ContinuousModification::AddPower {
                    value: i32::MAX - 2,
                },
                ContinuousModification::AddToughness {
                    value: i32::MAX - 2,
                },
            ]);
        let boost_b = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![
                ContinuousModification::AddPower { value: 100 },
                ContinuousModification::AddToughness { value: 100 },
            ]);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            Arc::make_mut(&mut obj.base_static_definitions).push(boost_a.clone());
            obj.static_definitions.push(boost_a);
            Arc::make_mut(&mut obj.base_static_definitions).push(boost_b.clone());
            obj.static_definitions.push(boost_b);
        }

        evaluate_layers(&mut state);

        let obj = &state.objects[&id];
        assert_eq!(
            obj.power,
            Some(i32::MAX),
            "power must saturate at i32::MAX rather than wrapping"
        );
        assert_eq!(
            obj.toughness,
            Some(i32::MAX),
            "toughness must saturate at i32::MAX rather than wrapping"
        );
        assert!(
            obj.toughness.unwrap() > 0,
            "toughness must stay positive so CR 704.5f SBAs don't kill the creature",
        );
    }

    /// CR 613.4c: A +1/+1 counter stack that overflows `u32 → i32` conversion
    /// must saturate; the resulting P/T must remain positive.
    #[test]
    fn saturating_counter_conversion_keeps_creature_alive() {
        let mut state = setup();
        let id = make_creature(&mut state, "Counter Pile", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.counters.insert(CounterType::Plus1Plus1, u32::MAX);
        }

        evaluate_layers(&mut state);

        let obj = &state.objects[&id];
        assert_eq!(obj.power, Some(i32::MAX));
        assert_eq!(obj.toughness, Some(i32::MAX));
        assert!(obj.toughness.unwrap() > 0);
    }

    #[test]
    fn assign_damage_as_though_unblocked_flag_resets_with_layers() {
        let mut state = setup();
        let id = make_creature(&mut state, "Thorn Elemental", 5, 5, PlayerId(0));
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AssignDamageAsThoughUnblocked]);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            Arc::make_mut(&mut obj.base_static_definitions).push(static_def.clone());
            obj.static_definitions.push(static_def);
        }

        evaluate_layers(&mut state);
        assert!(state.objects[&id].assigns_damage_as_though_unblocked);

        {
            let obj = state.objects.get_mut(&id).unwrap();
            Arc::make_mut(&mut obj.base_static_definitions).clear();
            obj.static_definitions.clear();
        }

        evaluate_layers(&mut state);
        assert!(!state.objects[&id].assigns_damage_as_though_unblocked);
    }

    /// Helper: creatures you control filter
    fn creature_you_ctrl() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
    }

    fn add_lord_static(
        state: &mut GameState,
        lord_id: ObjectId,
        filter: TargetFilter,
        add_power: i32,
        add_toughness: i32,
    ) {
        let def = StaticDefinition::continuous()
            .affected(filter)
            .modifications(vec![
                ContinuousModification::AddPower { value: add_power },
                ContinuousModification::AddToughness {
                    value: add_toughness,
                },
            ]);
        state
            .objects
            .get_mut(&lord_id)
            .unwrap()
            .static_definitions
            .push(def);
    }

    #[test]
    fn conditional_life_more_than_starting_applies_only_above_threshold() {
        let mut state = setup();
        state.format_config.starting_life = 20;
        state.players[0].life = 26;

        let leyline = make_creature(&mut state, "Leyline Source", 0, 0, PlayerId(0));
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        let def = StaticDefinition::continuous()
            .affected(creature_you_ctrl())
            .modifications(vec![
                ContinuousModification::AddPower { value: 2 },
                ContinuousModification::AddToughness { value: 2 },
            ])
            .condition(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeAboveStarting,
                },
                comparator: crate::types::ability::Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 7 },
            });
        state
            .objects
            .get_mut(&leyline)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);
        assert_eq!(state.objects[&bear].power, Some(2));
        assert_eq!(state.objects[&bear].toughness, Some(2));

        state.players[0].life = 27;
        state.layers_dirty = true;
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&bear].power, Some(4));
        assert_eq!(state.objects[&bear].toughness, Some(4));
    }

    #[test]
    fn test_lord_buff_modifies_computed_not_base() {
        let mut state = setup();
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        add_lord_static(&mut state, lord, creature_you_ctrl(), 1, 1);

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert_eq!(
            bear_obj.power,
            Some(3),
            "Bear computed power should be 2+1=3"
        );
        assert_eq!(
            bear_obj.toughness,
            Some(3),
            "Bear computed toughness should be 2+1=3"
        );
        assert_eq!(bear_obj.base_power, Some(2), "Bear base power unchanged");
        assert_eq!(
            bear_obj.base_toughness,
            Some(2),
            "Bear base toughness unchanged"
        );
    }

    #[test]
    fn test_layer_order_type_before_pt() {
        let mut state = setup();

        // A non-creature artifact
        let artifact = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        let art_ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(0);
            obj.toughness = Some(0);
            obj.base_power = Some(0);
            obj.base_toughness = Some(0);
            obj.timestamp = art_ts;
        }

        // Effect that makes artifacts into creatures (layer 4 - Type)
        let animator = make_creature(&mut state, "Animator", 1, 1, PlayerId(0));
        {
            let artifact_you_ctrl = TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
            );
            let def = StaticDefinition::continuous()
                .affected(artifact_you_ctrl)
                .modifications(vec![ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                }]);
            state
                .objects
                .get_mut(&animator)
                .unwrap()
                .static_definitions
                .push(def);
        }

        // Effect that buffs creatures (layer 7c - ModifyPT)
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        add_lord_static(&mut state, lord, creature_you_ctrl(), 1, 1);

        evaluate_layers(&mut state);

        let art_obj = state.objects.get(&artifact).unwrap();
        // The artifact should now be a creature (type change layer 4) and get the buff (layer 7c)
        assert!(art_obj.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!(art_obj.power, Some(1), "Artifact+creature gets +1/+1");
        assert_eq!(art_obj.toughness, Some(1), "Artifact+creature gets +1/+1");
    }

    #[test]
    fn test_timestamp_ordering_within_layer() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        // Two lords with different timestamps, both +1/+1
        let lord1 = make_creature(&mut state, "Lord1", 2, 2, PlayerId(0));
        add_lord_static(&mut state, lord1, creature_you_ctrl(), 1, 1);

        let lord2 = make_creature(&mut state, "Lord2", 2, 2, PlayerId(0));
        add_lord_static(&mut state, lord2, creature_you_ctrl(), 1, 1);

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        // Both lords apply: 2 + 1 + 1 = 4
        assert_eq!(bear_obj.power, Some(4));
        assert_eq!(bear_obj.toughness, Some(4));
    }

    #[test]
    fn test_dependency_ordering_overrides_timestamp() {
        let mut state = setup();

        // A non-creature artifact (will gain creature type from effect B)
        let artifact = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        let art_ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(0);
            obj.toughness = Some(0);
            obj.base_power = Some(0);
            obj.base_toughness = Some(0);
            obj.timestamp = art_ts;
        }

        // Effect A: Buffs creatures, timestamp 5 (created first, older)
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        {
            let obj = state.objects.get_mut(&lord).unwrap();
            obj.timestamp = 5;
        }
        add_lord_static(&mut state, lord, creature_you_ctrl(), 2, 2);

        // Effect B: Adds creature type to artifacts, timestamp 10 (created later, newer)
        let animator = make_creature(&mut state, "Animator", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&animator).unwrap();
            obj.timestamp = 10;
        }
        {
            let artifact_you_ctrl = TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
            );
            let def = StaticDefinition::continuous()
                .affected(artifact_you_ctrl)
                .modifications(vec![ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                }]);
            state
                .objects
                .get_mut(&animator)
                .unwrap()
                .static_definitions
                .push(def);
        }

        evaluate_layers(&mut state);

        let art_obj = state.objects.get(&artifact).unwrap();
        // Type change (layer 4) makes artifact a creature
        assert!(art_obj.card_types.core_types.contains(&CoreType::Creature));
        // ModifyPT (layer 7c) gives it +2/+2
        assert_eq!(art_obj.power, Some(2));
        assert_eq!(art_obj.toughness, Some(2));
    }

    #[test]
    fn test_counter_pt_layer_7e() {
        let mut state = setup();
        let creature = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&creature).unwrap();
        assert_eq!(obj.power, Some(4), "2 base + 2 counters = 4");
        assert_eq!(obj.toughness, Some(4), "2 base + 2 counters = 4");
    }

    #[test]
    fn test_layers_dirty_flag_cleared() {
        let mut state = setup();
        assert!(state.layers_dirty);

        evaluate_layers(&mut state);

        assert!(!state.layers_dirty);
    }

    #[test]
    fn test_aura_static_only_affects_enchanted_creature() {
        let mut state = setup();
        let bear_a = make_creature(&mut state, "Bear A", 2, 2, PlayerId(0));
        let bear_b = make_creature(&mut state, "Bear B", 2, 2, PlayerId(0));

        // Create an aura with Rancor-like static: +2/+0 and trample to EnchantedBy
        let aura = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Rancor".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Enchantment);
            obj.attached_to = Some(bear_a.into());
            obj.timestamp = ts;

            let enchanted_creature = TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            );
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(enchanted_creature)
                    .modifications(vec![
                        ContinuousModification::AddPower { value: 2 },
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Trample,
                        },
                    ]),
            );
        }
        state
            .objects
            .get_mut(&bear_a)
            .unwrap()
            .attachments
            .push(aura);

        evaluate_layers(&mut state);

        let a = state.objects.get(&bear_a).unwrap();
        assert_eq!(a.power, Some(4), "Enchanted bear: 2 base + 2 from aura");
        assert_eq!(a.toughness, Some(2), "Aura adds no toughness");
        assert!(
            a.has_keyword(&Keyword::Trample),
            "Enchanted bear gets trample"
        );

        let b = state.objects.get(&bear_b).unwrap();
        assert_eq!(b.power, Some(2), "Non-enchanted bear unchanged");
        assert_eq!(b.toughness, Some(2), "Non-enchanted bear unchanged");
        assert!(
            !b.has_keyword(&Keyword::Trample),
            "Non-enchanted bear has no trample"
        );
    }

    /// CR 301.5 + CR 303.4 + CR 613.4c: End-to-end runtime confirmation of
    /// the Strong Back / Mantle of the Ancients class — "Enchanted creature
    /// gets +N/+N for each Aura and Equipment attached to it." The pronoun
    /// "it" must resolve against each layer-evaluated *recipient* (the
    /// enchanted creature), not against the static's source (the Aura), so a
    /// non-Background, non-attached enchantment elsewhere on the battlefield
    /// must not contribute to the count.
    #[test]
    fn strong_back_per_recipient_dynamic_boost_counts_only_attachments_on_recipient() {
        use crate::types::ability::{
            FilterProp, QuantityRef, TargetFilter, TypeFilter, TypedFilter,
        };

        let mut state = setup();
        // Recipient: the bear Strong Back is enchanting.
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        // Bystander: an unrelated creature elsewhere on the battlefield.
        let other = make_creature(&mut state, "Other Bear", 2, 2, PlayerId(0));

        // Strong Back itself — the Aura source of the static.
        let strong_back = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Strong Back".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&strong_back).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".into());
            obj.attached_to = Some(bear.into());
            obj.timestamp = ts;

            // The "Enchanted creature gets +2/+2 for each Aura and Equipment
            // attached to it" continuous static — the lowering produced by
            // `parse_static_line`.
            let attached_to_recipient_filter = TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::AnyOf(vec![
                    TypeFilter::Subtype("Aura".into()),
                    TypeFilter::Subtype("Equipment".into()),
                ])],
                controller: None,
                properties: vec![FilterProp::AttachedToRecipient],
            });
            let qty = QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: attached_to_recipient_filter,
                    },
                }),
            };
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .modifications(vec![
                        ContinuousModification::AddDynamicPower { value: qty.clone() },
                        ContinuousModification::AddDynamicToughness { value: qty },
                    ]),
            );
        }
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .attachments
            .push(strong_back);

        // A second Aura attached to the recipient bear (counts).
        let recipient_aura = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Bear Umbra".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&recipient_aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".into());
            obj.attached_to = Some(bear.into());
            obj.timestamp = ts;
        }
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .attachments
            .push(recipient_aura);

        // A bystander Aura (Wild Growth) attached to OTHER creature — must
        // NOT count toward the bear's boost. This is the legacy bug class.
        let bystander_aura = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Wild Growth".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&bystander_aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".into());
            obj.attached_to = Some(other.into());
            obj.timestamp = ts;
        }
        state
            .objects
            .get_mut(&other)
            .unwrap()
            .attachments
            .push(bystander_aura);

        evaluate_layers(&mut state);

        // Two Auras attached to the bear (Strong Back + Bear Umbra) →
        // +2/+2 × 2 = +4/+4 over base 2/2 → final 6/6.
        let final_bear = state.objects.get(&bear).unwrap();
        assert_eq!(
            final_bear.power,
            Some(6),
            "expected 2 base + (2 attachments × 2) = 6 power; got {:?}",
            final_bear.power
        );
        assert_eq!(
            final_bear.toughness,
            Some(6),
            "expected 2 base + (2 attachments × 2) = 6 toughness; got {:?}",
            final_bear.toughness
        );

        // The other bear has its own attachment but is not the static's
        // recipient (it isn't enchanted by Strong Back) — it must remain at
        // base 2/2.
        let final_other = state.objects.get(&other).unwrap();
        assert_eq!(final_other.power, Some(2));
        assert_eq!(final_other.toughness, Some(2));
    }

    /// CR 301.5 + CR 303.4: Negative regression — Wild Growth on a different
    /// permanent must not seep into the boost count for the enchanted
    /// creature. This is the symptom users reported (Strong Back boost
    /// scaling with every battlefield enchantment).
    #[test]
    fn strong_back_unrelated_enchantment_does_not_inflate_boost() {
        use crate::types::ability::{
            FilterProp, QuantityRef, TargetFilter, TypeFilter, TypedFilter,
        };

        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let land_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let strong_back = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Strong Back".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&strong_back).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".into());
            obj.attached_to = Some(bear.into());
            obj.timestamp = ts;

            let qty = QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::AnyOf(vec![
                                TypeFilter::Subtype("Aura".into()),
                                TypeFilter::Subtype("Equipment".into()),
                            ])],
                            controller: None,
                            properties: vec![FilterProp::AttachedToRecipient],
                        }),
                    },
                }),
            };
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .modifications(vec![
                        ContinuousModification::AddDynamicPower { value: qty.clone() },
                        ContinuousModification::AddDynamicToughness { value: qty },
                    ]),
            );
        }
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .attachments
            .push(strong_back);

        // Wild Growth on the FOREST — this enchants a land, not the bear.
        let wild_growth = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Wild Growth".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&wild_growth).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".into());
            obj.attached_to = Some(land_id.into());
            obj.timestamp = ts;
        }
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .attachments
            .push(wild_growth);

        evaluate_layers(&mut state);

        // Only Strong Back itself is attached to the bear → +2/+2 once.
        let final_bear = state.objects.get(&bear).unwrap();
        assert_eq!(
            final_bear.power,
            Some(4),
            "Wild Growth on a land must not contribute to the bear's boost"
        );
        assert_eq!(final_bear.toughness, Some(4));
    }

    #[test]
    fn test_keyword_filtered_lord_uses_source_controller() {
        let mut state = setup();
        let winds = create_object(
            &mut state,
            CardId(0),
            PlayerId(1),
            "Favorable Winds".to_string(),
            Zone::Battlefield,
        );
        let winds_ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&winds).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.timestamp = winds_ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::WithKeyword {
                                value: Keyword::Flying,
                            }]),
                    ))
                    .modifications(vec![
                        ContinuousModification::AddPower { value: 1 },
                        ContinuousModification::AddToughness { value: 1 },
                    ]),
            );
        }

        let opponent_flyer = make_creature(&mut state, "Opponent Flyer", 2, 2, PlayerId(1));
        state
            .objects
            .get_mut(&opponent_flyer)
            .unwrap()
            .base_keywords
            .push(Keyword::Flying);
        state.objects.get_mut(&opponent_flyer).unwrap().keywords = vec![Keyword::Flying];

        let my_flyer = make_creature(&mut state, "My Flyer", 2, 2, PlayerId(0));
        state
            .objects
            .get_mut(&my_flyer)
            .unwrap()
            .base_keywords
            .push(Keyword::Flying);
        state.objects.get_mut(&my_flyer).unwrap().keywords = vec![Keyword::Flying];

        let opponent_ground = make_creature(&mut state, "Opponent Ground", 2, 2, PlayerId(1));

        evaluate_layers(&mut state);

        let opponent_flyer_obj = state.objects.get(&opponent_flyer).unwrap();
        assert_eq!(opponent_flyer_obj.power, Some(3));
        assert_eq!(opponent_flyer_obj.toughness, Some(3));

        let my_flyer_obj = state.objects.get(&my_flyer).unwrap();
        assert_eq!(my_flyer_obj.power, Some(2));
        assert_eq!(my_flyer_obj.toughness, Some(2));

        let opponent_ground_obj = state.objects.get(&opponent_ground).unwrap();
        assert_eq!(opponent_ground_obj.power, Some(2));
        assert_eq!(opponent_ground_obj.toughness, Some(2));
    }

    #[test]
    fn test_multi_layer_effect_does_not_double_apply() {
        // Regression: an effect with AddPower + AddKeyword spans two layers
        // (ModifyPT and Ability). AddPower must only be applied once.
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 3, 3, PlayerId(0));

        // Create a static with both AddPower and AddKeyword
        let source = make_creature(&mut state, "Source", 1, 1, PlayerId(0));
        {
            let def = StaticDefinition::continuous()
                .affected(creature_you_ctrl())
                .modifications(vec![
                    ContinuousModification::AddPower { value: 2 },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Trample,
                    },
                ]);
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .static_definitions
                .push(def);
        }

        evaluate_layers(&mut state);

        let obj = state.objects.get(&bear).unwrap();
        assert_eq!(
            obj.power,
            Some(5),
            "3 base + 2 from effect = 5, NOT 7 (double-applied)"
        );
        assert!(obj.has_keyword(&Keyword::Trample));
    }

    #[test]
    fn test_source_leaves_battlefield_effect_stops() {
        let mut state = setup();
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        add_lord_static(&mut state, lord, creature_you_ctrl(), 1, 1);

        evaluate_layers(&mut state);
        assert_eq!(state.objects.get(&bear).unwrap().power, Some(3));

        // Remove lord from battlefield
        state.battlefield.retain(|&id| id != lord);

        // Re-evaluate
        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert_eq!(
            bear_obj.power,
            Some(2),
            "Bear returns to base P/T after lord leaves"
        );
        assert_eq!(bear_obj.toughness, Some(2));
    }

    #[test]
    fn test_remove_all_abilities_clears_all_computed_ability_buckets() {
        let mut scenario = GameScenario::new();
        let target = {
            let mut card = scenario.add_creature(PlayerId(0), "Target", 2, 2);
            card.flying()
                .with_ability_definition(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: GainLifePlayer::Controller,
                    },
                ))
                .with_trigger(TriggerMode::Attacks)
                .with_replacement(ReplacementEvent::GainLife)
                .with_static(StaticMode::CantAttack);
            card.id()
        };
        {
            let mut card = scenario.add_creature(PlayerId(0), "Suppressor", 1, 1);
            card.with_static_definition(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target })
                    .modifications(vec![ContinuousModification::RemoveAllAbilities]),
            );
        }
        let mut state = scenario.build().state().clone();

        evaluate_layers(&mut state);

        let obj = state.objects.get(&target).unwrap();
        assert!(obj.keywords.is_empty());
        assert!(obj.abilities.is_empty());
        assert!(obj.trigger_definitions.is_empty());
        assert!(obj.replacement_definitions.is_empty());
        assert!(obj.static_definitions.is_empty());
    }

    #[test]
    fn test_remove_all_abilities_reverts_to_base_when_source_leaves() {
        let mut scenario = GameScenario::new();
        let target = {
            let mut card = scenario.add_creature(PlayerId(0), "Target", 2, 2);
            card.flying()
                .with_ability_definition(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: GainLifePlayer::Controller,
                    },
                ))
                .with_trigger(TriggerMode::Attacks)
                .with_replacement(ReplacementEvent::GainLife)
                .with_static(StaticMode::CantAttack);
            card.id()
        };
        let suppressor = {
            let mut card = scenario.add_creature(PlayerId(0), "Suppressor", 1, 1);
            card.with_static_definition(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target })
                    .modifications(vec![ContinuousModification::RemoveAllAbilities]),
            );
            card.id()
        };
        let mut state = scenario.build().state().clone();

        evaluate_layers(&mut state);
        state.battlefield.retain(|&id| id != suppressor);
        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&target).unwrap();
        assert_eq!(obj.keywords, vec![Keyword::Flying]);
        assert_eq!(obj.abilities.len(), 1);
        assert_eq!(obj.trigger_definitions.len(), 1);
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert_eq!(obj.static_definitions.len(), 1);
    }

    #[test]
    fn test_type_change_reverts_to_base_when_source_leaves() {
        let mut state = setup();

        let artifact = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        let art_ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = art_ts;
        }

        let animator = make_creature(&mut state, "Animator", 1, 1, PlayerId(0));
        let artifact_you_ctrl = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
        );
        state
            .objects
            .get_mut(&animator)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(artifact_you_ctrl)
                    .modifications(vec![ContinuousModification::AddType {
                        core_type: CoreType::Creature,
                    }]),
            );

        evaluate_layers(&mut state);
        assert!(state.objects[&artifact]
            .card_types
            .core_types
            .contains(&CoreType::Creature));

        state.battlefield.retain(|&id| id != animator);
        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&artifact).unwrap();
        assert_eq!(obj.card_types.core_types, vec![CoreType::Artifact]);
    }

    #[test]
    fn test_color_change_reverts_to_base_when_source_leaves() {
        let mut state = setup();

        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let painter = make_creature(&mut state, "Painter", 1, 1, PlayerId(0));

        state
            .objects
            .get_mut(&painter)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: bear })
                    .modifications(vec![ContinuousModification::SetColor {
                        colors: vec![ManaColor::Blue],
                    }]),
            );

        evaluate_layers(&mut state);
        assert_eq!(state.objects[&bear].color, vec![ManaColor::Blue]);

        state.battlefield.retain(|&id| id != painter);
        state.layers_dirty = true;
        evaluate_layers(&mut state);

        assert!(
            state.objects[&bear].color.is_empty(),
            "Color should revert to printed/base color when the source leaves"
        );
    }

    #[test]
    fn test_changeling_cda_grants_all_creature_types() {
        let mut state = setup();
        state.all_creature_types = vec![
            "Dragon".to_string(),
            "Elf".to_string(),
            "Human".to_string(),
            "Wizard".to_string(),
        ];

        let shapeshifter = make_creature(&mut state, "Shapeshifter", 2, 2, PlayerId(0));
        // Give it the Changeling keyword (printed)
        state
            .objects
            .get_mut(&shapeshifter)
            .unwrap()
            .base_keywords
            .push(Keyword::Changeling);
        state
            .objects
            .get_mut(&shapeshifter)
            .unwrap()
            .keywords
            .push(Keyword::Changeling);

        // Add the CDA static definition (as the parser/loader would)
        let cda = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddAllCreatureTypes])
            .cda();
        state
            .objects
            .get_mut(&shapeshifter)
            .unwrap()
            .static_definitions
            .push(cda);

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&shapeshifter).unwrap();
        assert!(obj.card_types.subtypes.contains(&"Dragon".to_string()));
        assert!(obj.card_types.subtypes.contains(&"Elf".to_string()));
        assert!(obj.card_types.subtypes.contains(&"Human".to_string()));
        assert!(obj.card_types.subtypes.contains(&"Wizard".to_string()));
    }

    #[test]
    fn test_granted_changeling_gets_all_creature_types_via_postfixup() {
        let mut state = setup();
        state.all_creature_types = vec!["Beast".to_string(), "Goblin".to_string()];

        let creature = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let lord = make_creature(&mut state, "Changeling Lord", 1, 1, PlayerId(0));

        // Lord grants Changeling to all your creatures via AddKeyword (Layer 6)
        let def = StaticDefinition::continuous()
            .affected(creature_you_ctrl())
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Changeling,
            }]);
        state
            .objects
            .get_mut(&lord)
            .unwrap()
            .static_definitions
            .push(def);

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        // The bear should have all creature types via the post-fixup
        let obj = state.objects.get(&creature).unwrap();
        assert!(obj.has_keyword(&Keyword::Changeling));
        assert!(
            obj.card_types.subtypes.contains(&"Beast".to_string()),
            "Granted Changeling should add Beast via post-fixup"
        );
        assert!(
            obj.card_types.subtypes.contains(&"Goblin".to_string()),
            "Granted Changeling should add Goblin via post-fixup"
        );
    }

    #[test]
    fn test_changeling_cda_sorts_before_non_cda_in_same_layer() {
        let mut state = setup();
        state.all_creature_types = vec!["Elf".to_string(), "Sliver".to_string()];

        let shapeshifter = make_creature(&mut state, "Shapeshifter", 1, 1, PlayerId(0));
        state
            .objects
            .get_mut(&shapeshifter)
            .unwrap()
            .base_keywords
            .push(Keyword::Changeling);
        state
            .objects
            .get_mut(&shapeshifter)
            .unwrap()
            .keywords
            .push(Keyword::Changeling);

        // CDA: add all creature types (characteristic_defining = true)
        let cda = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddAllCreatureTypes])
            .cda();

        // Non-CDA: also adds a subtype (later timestamp, but same layer)
        let non_cda = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddSubtype {
                subtype: "Shapeshifter".to_string(),
            }]);

        let obj = state.objects.get_mut(&shapeshifter).unwrap();
        obj.static_definitions.push(cda);
        obj.static_definitions.push(non_cda);

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&shapeshifter).unwrap();
        // All types from CDA + the explicit Shapeshifter subtype should be present
        assert!(obj.card_types.subtypes.contains(&"Elf".to_string()));
        assert!(obj.card_types.subtypes.contains(&"Sliver".to_string()));
        assert!(obj
            .card_types
            .subtypes
            .contains(&"Shapeshifter".to_string()));
    }

    #[test]
    fn test_chosen_basic_land_type_adds_subtype() {
        use crate::types::ability::{BasicLandType, ChosenAttribute};

        let mut state = setup();

        // Create a land with a chosen basic land type (simulating Multiversal Passage)
        let land = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Multiversal Passage".to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            obj.timestamp = ts;
            // Simulate the ETB choice: chose Forest
            obj.chosen_attributes
                .push(ChosenAttribute::BasicLandType(BasicLandType::Forest));
            // Add the static definition that reads the chosen type
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::BasicLandType,
                    }]),
            );
        }

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&land).unwrap();
        assert!(
            obj.card_types.subtypes.contains(&"Forest".to_string()),
            "Land should gain Forest subtype from chosen basic land type"
        );
    }

    #[test]
    fn test_chosen_basic_land_type_no_choice_is_noop() {
        let mut state = setup();

        // Land with AddChosenSubtype(BasicLandType) but no choice stored
        let land = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Unchosen Land".to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            obj.timestamp = ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::BasicLandType,
                    }]),
            );
        }

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&land).unwrap();
        assert!(
            obj.card_types.subtypes.is_empty(),
            "No subtypes should be added when no choice was made"
        );
    }

    #[test]
    fn test_chosen_creature_type_adds_subtype() {
        use crate::types::ability::ChosenAttribute;

        let mut state = setup();

        let mimic = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Metallic Mimic".to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&mimic).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.card_types.subtypes.push("Shapeshifter".to_string());
            obj.timestamp = ts;
            obj.chosen_attributes
                .push(ChosenAttribute::CreatureType("Elf".to_string()));
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::CreatureType,
                    }]),
            );
        }

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&mimic).unwrap();
        assert!(
            obj.card_types.subtypes.contains(&"Elf".to_string()),
            "Creature should gain Elf subtype from chosen creature type"
        );
        assert!(
            obj.card_types
                .subtypes
                .contains(&"Shapeshifter".to_string()),
            "Should retain original subtypes"
        );
    }

    #[test]
    fn test_tarmogoyf_cda_counts_card_types_in_graveyards() {
        let mut state = setup();

        // Create Tarmogoyf with */1+* base P/T and CDA static definition
        let goyf = make_creature(&mut state, "Tarmogoyf", 0, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&goyf).unwrap();
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![
                        ContinuousModification::SetDynamicPower {
                            value: QuantityExpr::Ref {
                                qty: QuantityRef::DistinctCardTypesInZone {
                                    zone: ZoneRef::Graveyard,
                                    scope: CountScope::All,
                                },
                            },
                        },
                        ContinuousModification::SetDynamicToughness {
                            value: QuantityExpr::Offset {
                                inner: Box::new(QuantityExpr::Ref {
                                    qty: QuantityRef::DistinctCardTypesInZone {
                                        zone: ZoneRef::Graveyard,
                                        scope: CountScope::All,
                                    },
                                }),
                                offset: 1,
                            },
                        },
                    ])
                    .cda(),
            );
        }

        // Empty graveyards: 0 card types → P/T = 0/1
        state.layers_dirty = true;
        evaluate_layers(&mut state);
        let obj = state.objects.get(&goyf).unwrap();
        assert_eq!(obj.power, Some(0), "No card types in graveyards → power 0");
        assert_eq!(obj.toughness, Some(1), "No card types → toughness 0+1=1");

        // Add a creature to graveyard: 1 card type → P/T = 1/2
        let gy_creature = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Dead Bear".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&gy_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.players[0].graveyard.push_back(gy_creature);

        state.layers_dirty = true;
        evaluate_layers(&mut state);
        let obj = state.objects.get(&goyf).unwrap();
        assert_eq!(obj.power, Some(1), "Creature in graveyard → power 1");
        assert_eq!(
            obj.toughness,
            Some(2),
            "Creature in graveyard → toughness 2"
        );

        // Add an instant to opponent's graveyard: 2 card types → P/T = 2/3
        let gy_instant = create_object(
            &mut state,
            CardId(0),
            PlayerId(1),
            "Spent Bolt".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&gy_instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        state.players[1].graveyard.push_back(gy_instant);

        state.layers_dirty = true;
        evaluate_layers(&mut state);
        let obj = state.objects.get(&goyf).unwrap();
        assert_eq!(obj.power, Some(2), "Creature + Instant → power 2");
        assert_eq!(obj.toughness, Some(3), "Creature + Instant → toughness 3");

        // Add an artifact creature to graveyard: still 2 types (creature already counted), + artifact = 3
        let gy_artcreature = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Dead Robot".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_artcreature).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.core_types.push(CoreType::Creature);
        }
        state.players[0].graveyard.push_back(gy_artcreature);

        state.layers_dirty = true;
        evaluate_layers(&mut state);
        let obj = state.objects.get(&goyf).unwrap();
        assert_eq!(
            obj.power,
            Some(3),
            "Creature + Instant + Artifact → power 3"
        );
        assert_eq!(
            obj.toughness,
            Some(4),
            "Creature + Instant + Artifact → toughness 4"
        );
    }

    // -----------------------------------------------------------------------
    // StaticCondition::And / Or / HasCounters tests
    // -----------------------------------------------------------------------

    #[test]
    fn has_counters_true_when_loyalty_present() {
        let mut state = setup();
        let id = make_creature(&mut state, "Kaito", 0, 0, PlayerId(0));
        {
            use crate::types::counter::CounterType;
            let obj = state.objects.get_mut(&id).unwrap();
            obj.counters.insert(CounterType::Loyalty, 3);
        }
        let cond = StaticCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Loyalty),
            minimum: 1,
            maximum: None,
        };
        assert!(evaluate_condition(&state, &cond, PlayerId(0), id));
    }

    #[test]
    fn has_counters_false_when_zero_loyalty() {
        let mut state = setup();
        let id = make_creature(&mut state, "Kaito", 0, 0, PlayerId(0));
        let cond = StaticCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Loyalty),
            minimum: 1,
            maximum: None,
        };
        assert!(!evaluate_condition(&state, &cond, PlayerId(0), id));
    }

    /// CR 122.1: `CounterMatch::Any` sums across every counter type — "has a
    /// counter on it" fires for any non-zero counter, regardless of kind.
    /// Motivating card: Demon Wall (`as long as this creature has a counter
    /// on it, it can attack as though it didn't have defender`).
    #[test]
    fn has_counters_any_true_when_any_counter_type_present() {
        let mut state = setup();
        let id = make_creature(&mut state, "Demon Wall", 3, 3, PlayerId(0));
        {
            let obj = state.objects.get_mut(&id).unwrap();
            // Any counter type should satisfy CounterMatch::Any — use a
            // generic counter here to prove it is not +1/+1-specific.
            obj.counters
                .insert(CounterType::Generic("page".to_string()), 1);
        }
        let cond = StaticCondition::HasCounters {
            counters: CounterMatch::Any,
            minimum: 1,
            maximum: None,
        };
        assert!(evaluate_condition(&state, &cond, PlayerId(0), id));
    }

    #[test]
    fn has_counters_any_false_when_no_counters() {
        let mut state = setup();
        let id = make_creature(&mut state, "Demon Wall", 3, 3, PlayerId(0));
        let cond = StaticCondition::HasCounters {
            counters: CounterMatch::Any,
            minimum: 1,
            maximum: None,
        };
        assert!(!evaluate_condition(&state, &cond, PlayerId(0), id));
    }

    /// Primordial Hydra: trample gate activates at exactly 10 +1/+1 counters and
    /// stays active above that threshold; inactive at 9 or 0.
    #[test]
    fn has_counters_p1p1_ten_or_more_threshold() {
        use crate::types::counter::CounterType;
        let mut state = setup();
        let id = make_creature(&mut state, "Primordial Hydra", 0, 0, PlayerId(0));
        let cond = StaticCondition::HasCounters {
            counters: crate::types::counter::CounterMatch::OfType(CounterType::Plus1Plus1),
            minimum: 10,
            maximum: None,
        };

        // 0 counters → inactive.
        assert!(!evaluate_condition(&state, &cond, PlayerId(0), id));

        // 9 counters → inactive.
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 9);
        assert!(!evaluate_condition(&state, &cond, PlayerId(0), id));

        // 10 counters → active.
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 10);
        assert!(evaluate_condition(&state, &cond, PlayerId(0), id));

        // 11 counters → active.
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 11);
        assert!(evaluate_condition(&state, &cond, PlayerId(0), id));
    }

    #[test]
    fn compound_and_true_when_both_conditions_met() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        let id = make_creature(&mut state, "Kaito", 0, 0, PlayerId(0));
        {
            use crate::types::counter::CounterType;
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .counters
                .insert(CounterType::Loyalty, 3);
        }
        let cond = StaticCondition::And {
            conditions: vec![
                StaticCondition::DuringYourTurn,
                StaticCondition::HasCounters {
                    counters: CounterMatch::OfType(CounterType::Loyalty),
                    minimum: 1,
                    maximum: None,
                },
            ],
        };
        assert!(evaluate_condition(&state, &cond, PlayerId(0), id));
    }

    #[test]
    fn compound_and_false_when_not_your_turn() {
        let mut state = setup();
        state.active_player = PlayerId(1); // opponent's turn
        let id = make_creature(&mut state, "Kaito", 0, 0, PlayerId(0));
        {
            use crate::types::counter::CounterType;
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .counters
                .insert(CounterType::Loyalty, 3);
        }
        let cond = StaticCondition::And {
            conditions: vec![
                StaticCondition::DuringYourTurn,
                StaticCondition::HasCounters {
                    counters: CounterMatch::OfType(CounterType::Loyalty),
                    minimum: 1,
                    maximum: None,
                },
            ],
        };
        assert!(!evaluate_condition(&state, &cond, PlayerId(0), id));
    }

    #[test]
    fn compound_and_false_when_no_counters() {
        let mut state = setup();
        state.active_player = PlayerId(0);
        let id = make_creature(&mut state, "Kaito", 0, 0, PlayerId(0));
        // No loyalty counters added
        let cond = StaticCondition::And {
            conditions: vec![
                StaticCondition::DuringYourTurn,
                StaticCondition::HasCounters {
                    counters: CounterMatch::OfType(CounterType::Loyalty),
                    minimum: 1,
                    maximum: None,
                },
            ],
        };
        assert!(!evaluate_condition(&state, &cond, PlayerId(0), id));
    }

    #[test]
    fn compound_or_true_when_only_one_condition_met() {
        let mut state = setup();
        state.active_player = PlayerId(1); // opponent's turn
        let id = make_creature(&mut state, "Kaito", 0, 0, PlayerId(0));
        {
            use crate::types::counter::CounterType;
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .counters
                .insert(CounterType::Loyalty, 3);
        }
        // Not your turn, but has loyalty counters → Or should be true
        let cond = StaticCondition::Or {
            conditions: vec![
                StaticCondition::DuringYourTurn,
                StaticCondition::HasCounters {
                    counters: CounterMatch::OfType(CounterType::Loyalty),
                    minimum: 1,
                    maximum: None,
                },
            ],
        };
        assert!(evaluate_condition(&state, &cond, PlayerId(0), id));
    }

    #[test]
    fn compound_condition_animates_planeswalker_as_creature() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create a planeswalker-like object
        let id = make_creature(&mut state, "Kaito", 0, 0, PlayerId(0));
        {
            use crate::types::counter::CounterType;
            let obj = state.objects.get_mut(&id).unwrap();
            // Start as planeswalker, not creature
            obj.card_types.core_types.clear();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.base_card_types = obj.card_types.clone();
            obj.power = None;
            obj.toughness = None;
            obj.base_power = None;
            obj.base_toughness = None;
            obj.counters.insert(CounterType::Loyalty, 3);
        }

        // Add compound static: during your turn + has loyalty counters → animate as 3/4 Ninja creature with hexproof
        let def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(StaticCondition::And {
                conditions: vec![
                    StaticCondition::DuringYourTurn,
                    StaticCondition::HasCounters {
                        counters: CounterMatch::OfType(CounterType::Loyalty),
                        minimum: 1,
                        maximum: None,
                    },
                ],
            })
            .modifications(vec![
                ContinuousModification::SetPower { value: 3 },
                ContinuousModification::SetToughness { value: 4 },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
                ContinuousModification::AddSubtype {
                    subtype: "Ninja".to_string(),
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Hexproof,
                },
            ]);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(def);

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.power, Some(3), "animated power should be 3");
        assert_eq!(obj.toughness, Some(4), "animated toughness should be 4");
        assert!(
            obj.card_types.core_types.contains(&CoreType::Creature),
            "should have Creature type"
        );
        assert!(
            obj.card_types.core_types.contains(&CoreType::Planeswalker),
            "should still be Planeswalker"
        );
        assert!(
            obj.card_types.subtypes.contains(&"Ninja".to_string()),
            "should have Ninja subtype"
        );
        assert!(
            obj.keywords.contains(&Keyword::Hexproof),
            "should have hexproof"
        );
    }

    #[test]
    fn compound_condition_does_not_animate_on_opponents_turn() {
        let mut state = setup();
        state.active_player = PlayerId(1); // opponent's turn

        let id = make_creature(&mut state, "Kaito", 0, 0, PlayerId(0));
        {
            use crate::types::counter::CounterType;
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.clear();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.base_card_types = obj.card_types.clone();
            obj.power = None;
            obj.toughness = None;
            obj.base_power = None;
            obj.base_toughness = None;
            obj.counters.insert(CounterType::Loyalty, 3);
        }

        let def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(StaticCondition::And {
                conditions: vec![
                    StaticCondition::DuringYourTurn,
                    StaticCondition::HasCounters {
                        counters: CounterMatch::OfType(CounterType::Loyalty),
                        minimum: 1,
                        maximum: None,
                    },
                ],
            })
            .modifications(vec![
                ContinuousModification::SetPower { value: 3 },
                ContinuousModification::SetToughness { value: 4 },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
                ContinuousModification::AddSubtype {
                    subtype: "Ninja".to_string(),
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Hexproof,
                },
            ]);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(def);

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&id).unwrap();
        // Should NOT be animated on opponent's turn
        assert_eq!(obj.power, None, "should not have power on opponent's turn");
        assert_eq!(
            obj.toughness, None,
            "should not have toughness on opponent's turn"
        );
        assert!(
            !obj.card_types.core_types.contains(&CoreType::Creature),
            "should not have Creature type on opponent's turn"
        );
        assert!(
            !obj.keywords.contains(&Keyword::Hexproof),
            "should not have hexproof on opponent's turn"
        );
    }

    #[test]
    fn emblem_static_applies_to_matching_creatures() {
        let mut state = setup();

        // Create a Ninja creature on the battlefield for Player 0
        let ninja_id = make_creature(&mut state, "Ninja Spy", 2, 2, PlayerId(0));
        {
            let obj = state.objects.get_mut(&ninja_id).unwrap();
            obj.card_types.subtypes.push("Ninja".to_string());
            obj.base_card_types = obj.card_types.clone();
        }

        // Create a non-Ninja creature for Player 0
        let bear_id = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        {
            let obj = state.objects.get_mut(&bear_id).unwrap();
            obj.card_types.subtypes.push("Bear".to_string());
            obj.base_card_types = obj.card_types.clone();
        }

        // Create a Ninja creature for Player 1 (opponent)
        let opp_ninja_id = make_creature(&mut state, "Opp Ninja", 2, 2, PlayerId(1));
        {
            let obj = state.objects.get_mut(&opp_ninja_id).unwrap();
            obj.card_types.subtypes.push("Ninja".to_string());
            obj.base_card_types = obj.card_types.clone();
        }

        // Create an emblem in the command zone for Player 0
        // CR 114: "Ninjas you control get +1/+1"
        let emblem_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Emblem".to_string(),
            Zone::Command,
        );
        let emblem = state.objects.get_mut(&emblem_id).unwrap();
        emblem.is_emblem = true;
        emblem.static_definitions = vec![StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![crate::types::ability::TypeFilter::Subtype(
                    "Ninja".to_string(),
                )],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }))
            .modifications(vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ])]
        .into();

        // Mark layers dirty and evaluate
        state.layers_dirty = true;
        evaluate_layers(&mut state);

        // Player 0's Ninja should get +1/+1
        let ninja = state.objects.get(&ninja_id).unwrap();
        assert_eq!(
            ninja.power,
            Some(3),
            "Ninja should have 3 power (+1/+1 from emblem)"
        );
        assert_eq!(
            ninja.toughness,
            Some(3),
            "Ninja should have 3 toughness (+1/+1 from emblem)"
        );

        // Player 0's Bear should NOT get the bonus
        let bear = state.objects.get(&bear_id).unwrap();
        assert_eq!(bear.power, Some(2), "Bear should still have 2 power");
        assert_eq!(
            bear.toughness,
            Some(2),
            "Bear should still have 2 toughness"
        );

        // Player 1's Ninja should NOT get the bonus (not "you control")
        let opp_ninja = state.objects.get(&opp_ninja_id).unwrap();
        assert_eq!(
            opp_ninja.power,
            Some(2),
            "Opponent's Ninja should still have 2 power"
        );
        assert_eq!(
            opp_ninja.toughness,
            Some(2),
            "Opponent's Ninja should still have 2 toughness"
        );
    }

    /// CR 305.7: SetBasicLandType replaces old land subtypes and adds the new one.
    #[test]
    fn set_basic_land_type_replaces_subtypes() {
        use crate::types::ability::BasicLandType;

        let mut state = setup();
        let p0 = PlayerId(0);

        // Create a Forest land on the battlefield
        let land_id = create_object(
            &mut state,
            CardId(0),
            p0,
            "Test Forest".to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
        }

        // Create an aura that sets enchanted land to Mountain
        let aura_id = create_object(
            &mut state,
            CardId(1),
            p0,
            "Blood Moon Aura".to_string(),
            Zone::Battlefield,
        );
        let ts2 = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.timestamp = ts2;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .modifications(vec![ContinuousModification::SetBasicLandType {
                        land_type: BasicLandType::Mountain,
                    }]),
            );
        }

        // Attach aura to land
        state.objects.get_mut(&aura_id).unwrap().attached_to = Some(land_id.into());

        evaluate_layers(&mut state);

        let land = state.objects.get(&land_id).unwrap();
        assert!(
            land.card_types.subtypes.contains(&"Mountain".to_string()),
            "Land should have Mountain subtype"
        );
        assert!(
            !land.card_types.subtypes.contains(&"Forest".to_string()),
            "Land should no longer have Forest subtype"
        );
    }

    #[test]
    fn evaluate_source_is_tapped_true_when_tapped() {
        let mut state = setup();
        let id = make_creature(&mut state, "Test", 2, 2, PlayerId(0));
        state.objects.get_mut(&id).unwrap().tapped = true;
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsTapped,
            PlayerId(0),
            id
        ));
    }

    #[test]
    fn evaluate_source_is_tapped_false_when_untapped() {
        let mut state = setup();
        let id = make_creature(&mut state, "Test", 2, 2, PlayerId(0));
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsTapped,
            PlayerId(0),
            id
        ));
    }

    // CR 708.2 + CR 707.2: EnchantedIsFaceDown resolver tests.
    #[test]
    fn evaluate_enchanted_is_face_down_true_when_attached_face_down() {
        let mut state = setup();
        let aura = make_creature(&mut state, "Aura", 0, 0, PlayerId(0));
        let creature = make_creature(&mut state, "Manifested", 2, 2, PlayerId(0));
        state.objects.get_mut(&creature).unwrap().face_down = true;
        state.objects.get_mut(&aura).unwrap().attached_to = Some(creature.into());
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::EnchantedIsFaceDown,
            PlayerId(0),
            aura,
        ));
    }

    #[test]
    fn evaluate_enchanted_is_face_down_false_when_attached_face_up() {
        let mut state = setup();
        let aura = make_creature(&mut state, "Aura", 0, 0, PlayerId(0));
        let creature = make_creature(&mut state, "Face Up", 2, 2, PlayerId(0));
        state.objects.get_mut(&aura).unwrap().attached_to = Some(creature.into());
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::EnchantedIsFaceDown,
            PlayerId(0),
            aura,
        ));
    }

    #[test]
    fn evaluate_enchanted_is_face_down_false_when_unattached() {
        let mut state = setup();
        let aura = make_creature(&mut state, "Aura", 0, 0, PlayerId(0));
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::EnchantedIsFaceDown,
            PlayerId(0),
            aura,
        ));
    }

    // -- Combat-state predicate evaluator tests (CR 508.1k / CR 509.1g / CR 509.1h) --

    #[test]
    fn evaluate_source_is_attacking_true_when_in_attackers() {
        use crate::game::combat::{AttackerInfo, CombatState};
        let mut state = setup();
        let id = make_creature(&mut state, "Attacker", 2, 2, PlayerId(0));
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(id, PlayerId(1))],
            ..Default::default()
        });
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsAttacking,
            PlayerId(0),
            id,
        ));
    }

    #[test]
    fn evaluate_source_is_attacking_false_when_no_combat() {
        let mut state = setup();
        let id = make_creature(&mut state, "Idle", 2, 2, PlayerId(0));
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsAttacking,
            PlayerId(0),
            id,
        ));
    }

    #[test]
    fn evaluate_source_is_attacking_false_when_not_in_attackers() {
        use crate::game::combat::{AttackerInfo, CombatState};
        let mut state = setup();
        let attacker = make_creature(&mut state, "Attacker", 2, 2, PlayerId(0));
        let bystander = make_creature(&mut state, "Bystander", 2, 2, PlayerId(0));
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsAttacking,
            PlayerId(0),
            bystander,
        ));
    }

    #[test]
    fn evaluate_source_is_blocking_true_when_in_blocker_map() {
        use crate::game::combat::CombatState;
        let mut state = setup();
        let blocker = make_creature(&mut state, "Blocker", 2, 2, PlayerId(1));
        let attacker = make_creature(&mut state, "Attacker", 2, 2, PlayerId(0));
        let mut combat = CombatState::default();
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        state.combat = Some(combat);
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsBlocking,
            PlayerId(1),
            blocker,
        ));
    }

    #[test]
    fn evaluate_source_is_blocking_false_when_not_blocking() {
        use crate::game::combat::CombatState;
        let mut state = setup();
        let blocker = make_creature(&mut state, "Blocker", 2, 2, PlayerId(1));
        state.combat = Some(CombatState::default());
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsBlocking,
            PlayerId(1),
            blocker,
        ));
    }

    #[test]
    fn evaluate_source_is_blocked_true_when_sticky_flag_set() {
        // CR 509.1h: A creature remains blocked even if all the creatures blocking
        // it are removed from combat — `AttackerInfo.blocked` is set during blocker
        // declaration and never cleared.
        use crate::game::combat::{AttackerInfo, CombatState};
        let mut state = setup();
        let id = make_creature(&mut state, "Attacker", 2, 2, PlayerId(0));
        let mut info = AttackerInfo::attacking_player(id, PlayerId(1));
        info.blocked = true;
        state.combat = Some(CombatState {
            attackers: vec![info],
            ..Default::default()
        });
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsBlocked,
            PlayerId(0),
            id,
        ));
    }

    #[test]
    fn evaluate_source_is_blocked_false_when_flag_unset() {
        use crate::game::combat::{AttackerInfo, CombatState};
        let mut state = setup();
        let id = make_creature(&mut state, "Attacker", 2, 2, PlayerId(0));
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(id, PlayerId(1))],
            ..Default::default()
        });
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsBlocked,
            PlayerId(0),
            id,
        ));
    }

    #[test]
    fn gather_skips_for_as_long_as_when_condition_false() {
        let mut state = setup();
        let id = make_creature(&mut state, "Tapper", 1, 1, PlayerId(0));
        // Object is untapped → ForAsLongAs { SourceIsTapped } should NOT apply
        let ts = state.next_timestamp();
        state
            .transient_continuous_effects
            .push_back(TransientContinuousEffect {
                id: 1,
                source_id: id,
                controller: PlayerId(0),
                timestamp: ts,
                duration: Duration::ForAsLongAs {
                    condition: StaticCondition::SourceIsTapped,
                },
                affected: TargetFilter::SelfRef,
                modifications: vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }],
                condition: None,
            });
        let mut effects = vec![];
        gather_transient_continuous_effects(&state, &mut effects);
        assert!(
            effects.is_empty(),
            "effect should not be gathered when source is untapped"
        );
    }

    #[test]
    fn gather_includes_for_as_long_as_when_condition_true() {
        let mut state = setup();
        let id = make_creature(&mut state, "Tapper", 1, 1, PlayerId(0));
        state.objects.get_mut(&id).unwrap().tapped = true;
        let ts = state.next_timestamp();
        state
            .transient_continuous_effects
            .push_back(TransientContinuousEffect {
                id: 1,
                source_id: id,
                controller: PlayerId(0),
                timestamp: ts,
                duration: Duration::ForAsLongAs {
                    condition: StaticCondition::SourceIsTapped,
                },
                affected: TargetFilter::SelfRef,
                modifications: vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }],
                condition: None,
            });
        let mut effects = vec![];
        gather_transient_continuous_effects(&state, &mut effects);
        assert!(
            !effects.is_empty(),
            "effect should be gathered when source is tapped"
        );
    }

    // --- CR 305.7: SetBasicLandType tests ---

    fn make_land(state: &mut GameState, name: &str, player: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(0),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
        obj.timestamp = ts;
        id
    }

    #[test]
    fn set_basic_land_type_removes_rules_text_abilities() {
        // CR 305.7: A land whose type is set loses rules-text abilities.
        let mut state = setup();
        let p0 = PlayerId(0);

        let land_id = make_land(&mut state, "Test Land", p0);
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.subtypes.push("Desert".to_string());
            obj.base_card_types = obj.card_types.clone();
            Arc::make_mut(&mut obj.base_abilities).push(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: GainLifePlayer::Controller,
                },
            ));
            obj.abilities = Arc::new((*obj.base_abilities).clone());
        }

        // Source: enchantment with SetBasicLandType static
        let source_id = create_object(
            &mut state,
            CardId(1),
            p0,
            "Blood Moon".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(TypedFilter::land()))
                    .modifications(vec![ContinuousModification::SetBasicLandType {
                        land_type: BasicLandType::Mountain,
                    }]),
            );
        }

        evaluate_layers(&mut state);

        let land = state.objects.get(&land_id).unwrap();
        assert!(
            land.abilities.is_empty(),
            "CR 305.7: Rules-text abilities should be removed"
        );
        assert!(land.card_types.subtypes.contains(&"Mountain".to_string()));
        assert!(
            !land.card_types.subtypes.contains(&"Desert".to_string()),
            "CR 305.7: Old land subtypes should be removed"
        );
    }

    #[test]
    fn set_basic_land_type_preserves_creature_subtypes() {
        // CR 305.7: "Setting a land's subtype doesn't add or remove any card types."
        // Land Creature with "Forest Dryad" → SetBasicLandType Mountain →
        // keeps "Dryad" creature subtype, loses "Forest" land subtype, gains "Mountain".
        let mut state = setup();
        let p0 = PlayerId(0);

        let land_id = make_land(&mut state, "Dryad Arbor", p0);
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.card_types.subtypes.push("Dryad".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
        }

        let source_id = create_object(
            &mut state,
            CardId(1),
            p0,
            "Blood Moon".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(TypedFilter::land()))
                    .modifications(vec![ContinuousModification::SetBasicLandType {
                        land_type: BasicLandType::Mountain,
                    }]),
            );
        }

        evaluate_layers(&mut state);

        let land = state.objects.get(&land_id).unwrap();
        assert!(
            land.card_types.subtypes.contains(&"Mountain".to_string()),
            "Should gain Mountain"
        );
        assert!(
            land.card_types.subtypes.contains(&"Dryad".to_string()),
            "CR 305.7: Creature subtypes must be preserved"
        );
        assert!(
            !land.card_types.subtypes.contains(&"Forest".to_string()),
            "Forest land subtype should be removed"
        );
        assert!(
            land.card_types.core_types.contains(&CoreType::Creature),
            "CR 305.7: Core types must be preserved"
        );
    }

    #[test]
    fn add_all_basic_land_types_adds_five_subtypes() {
        // Prismatic Omen: "Lands you control are every basic land type in addition
        // to their other types."
        let mut state = setup();
        let p0 = PlayerId(0);

        let land_id = make_land(&mut state, "Guildgate", p0);
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.subtypes.push("Gate".to_string());
            obj.base_card_types = obj.card_types.clone();
        }

        let source_id = create_object(
            &mut state,
            CardId(1),
            p0,
            "Prismatic Omen".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::You),
                    ))
                    .modifications(vec![ContinuousModification::AddAllBasicLandTypes]),
            );
        }

        evaluate_layers(&mut state);

        let land = state.objects.get(&land_id).unwrap();
        assert!(
            land.card_types.subtypes.contains(&"Gate".to_string()),
            "Original subtype should be preserved (additive)"
        );
        for name in ["Plains", "Island", "Swamp", "Mountain", "Forest"] {
            assert!(
                land.card_types.subtypes.contains(&name.to_string()),
                "Missing basic land type: {name}"
            );
        }
    }

    #[test]
    fn false_condition_anthem_does_not_modify_power_and_toughness() {
        // CR 604.1 / CR 613.1 regression: when an anthem-style continuous
        // static has a `condition` that evaluates false, it must contribute
        // NO continuous effects — the target creature's computed P/T stays
        // at its base. Drives `evaluate_layers` end-to-end through the
        // `battlefield_active_statics` gate.
        let mut state = setup();
        // IsMonarch is false by default (no monarch set).
        assert!(state.monarch.is_none());

        // "Creatures you control get +1/+1" conditioned on IsMonarch.
        let anthem = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Conditional Anthem".to_string(),
            Zone::Battlefield,
        );
        let anthem_ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&anthem).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.timestamp = anthem_ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .condition(StaticCondition::IsMonarch)
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ))
                    .modifications(vec![
                        ContinuousModification::AddPower { value: 1 },
                        ContinuousModification::AddToughness { value: 1 },
                    ]),
            );
        }

        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert_eq!(
            bear_obj.power,
            Some(2),
            "Anthem with false IsMonarch condition must not modify power"
        );
        assert_eq!(
            bear_obj.toughness,
            Some(2),
            "Anthem with false IsMonarch condition must not modify toughness"
        );

        // Baseline: setting monarch flips the condition true and the anthem
        // takes effect, proving the anthem itself is otherwise wired up and
        // that the only reason it didn't apply above was the condition gate.
        state.monarch = Some(PlayerId(0));
        evaluate_layers(&mut state);
        let bear_obj = state.objects.get(&bear).unwrap();
        assert_eq!(bear_obj.power, Some(3));
        assert_eq!(bear_obj.toughness, Some(3));
    }

    /// CR 702.94a + CR 400.3: A continuous static ability whose `affected`
    /// filter carries `InZone { zone: Hand }` applies to hand objects rather
    /// than battlefield objects. Verifies `apply_continuous_effect` dispatches
    /// on the filter's zone.
    #[test]
    fn hand_zone_static_grants_keyword_to_hand_card() {
        use crate::types::ability::{FilterProp, TypedFilter};
        use crate::types::keywords::Keyword;
        use crate::types::mana::ManaCost;

        let mut state = setup();

        let grant_static = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Instant)
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
            ))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Miracle(ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                }),
            }]);

        // Place a "Lorehold"-style source on the battlefield that grants
        // miracle {2} to each instant card in its controller's hand.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "HandGrantSource".to_string(),
            Zone::Battlefield,
        );
        {
            let src = state.objects.get_mut(&source).unwrap();
            src.card_types.core_types.push(CoreType::Creature);
            src.base_card_types = src.card_types.clone();
            src.static_definitions.push(grant_static.clone());
            src.base_static_definitions = Arc::new(vec![grant_static]);
        }

        // Put an instant in the same player's hand.
        let bolt = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "TestBolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.base_card_types = obj.card_types.clone();
        }

        // Hand objects don't need player.hand population for create_object's
        // zone routing — but `zone_object_ids(Hand)` reads `state.players[n].hand`.
        // Add bolt to the player's hand vector explicitly.
        if let Some(player) = state.players.iter_mut().find(|p| p.id == PlayerId(0)) {
            player.hand.push_back(bolt);
        }

        // Pre-condition: hand card has no keywords.
        assert!(state.objects[&bolt].keywords.is_empty());

        evaluate_layers(&mut state);

        // Post-condition: the hand card now has Miracle({2}).
        let obj = state.objects.get(&bolt).unwrap();
        assert!(
            obj.keywords
                .iter()
                .any(|k| matches!(k, Keyword::Miracle(_))),
            "expected hand card to have Miracle after layers pass, got {:?}",
            obj.keywords,
        );

        // Also: an instant owned by the opponent should NOT receive the grant
        // (controller: You on the filter restricts to source controller's hand).
        let opp_bolt = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "OpponentBolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&opp_bolt).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.base_card_types = obj.card_types.clone();
        }
        if let Some(player) = state.players.iter_mut().find(|p| p.id == PlayerId(1)) {
            player.hand.push_back(opp_bolt);
        }

        evaluate_layers(&mut state);
        let opp_obj = state.objects.get(&opp_bolt).unwrap();
        assert!(
            !opp_obj
                .keywords
                .iter()
                .any(|k| matches!(k, Keyword::Miracle(_))),
            "opponent's hand card must NOT receive the grant (controller: You)",
        );

        // A re-evaluation must NOT stack keywords — the reset logic should clear
        // hand-zone grants to base before re-applying.
        evaluate_layers(&mut state);
        let obj = state.objects.get(&bolt).unwrap();
        let miracle_count = obj
            .keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Miracle(_)))
            .count();
        assert_eq!(
            miracle_count, 1,
            "hand-zone keyword grant must not accumulate across layers passes"
        );
    }

    fn make_exiled_card(state: &mut GameState, owner: PlayerId) -> ObjectId {
        create_object(
            state,
            CardId(0),
            owner,
            "Exiled Card".to_string(),
            Zone::Exile,
        )
    }

    #[test]
    fn end_of_turn_prune_clears_until_end_of_turn_play_from_exile() {
        let mut state = setup();
        let exiled = make_exiled_card(&mut state, PlayerId(0));
        state
            .objects
            .get_mut(&exiled)
            .unwrap()
            .casting_permissions
            .push(CastingPermission::PlayFromExile {
                duration: Duration::UntilEndOfTurn,
                granted_to: PlayerId(0),
            });

        prune_end_of_turn_casting_permissions(&mut state);

        assert!(
            state.objects[&exiled].casting_permissions.is_empty(),
            "UntilEndOfTurn PlayFromExile should expire at cleanup"
        );
    }

    #[test]
    fn end_of_turn_prune_preserves_other_durations() {
        let mut state = setup();
        let exiled = make_exiled_card(&mut state, PlayerId(0));
        let perms = &mut state.objects.get_mut(&exiled).unwrap().casting_permissions;
        perms.push(CastingPermission::PlayFromExile {
            duration: Duration::UntilYourNextTurn,
            granted_to: PlayerId(0),
        });
        perms.push(CastingPermission::PlayFromExile {
            duration: Duration::Permanent,
            granted_to: PlayerId(0),
        });
        perms.push(CastingPermission::AdventureCreature);

        prune_end_of_turn_casting_permissions(&mut state);

        assert_eq!(
            state.objects[&exiled].casting_permissions.len(),
            3,
            "non-UntilEndOfTurn permissions must survive cleanup"
        );
    }

    #[test]
    fn until_your_next_turn_prune_expires_for_grantee_only() {
        let mut state = setup();
        let card_a = make_exiled_card(&mut state, PlayerId(0));
        let card_b = make_exiled_card(&mut state, PlayerId(1));
        state
            .objects
            .get_mut(&card_a)
            .unwrap()
            .casting_permissions
            .push(CastingPermission::PlayFromExile {
                duration: Duration::UntilYourNextTurn,
                granted_to: PlayerId(0),
            });
        state
            .objects
            .get_mut(&card_b)
            .unwrap()
            .casting_permissions
            .push(CastingPermission::PlayFromExile {
                duration: Duration::UntilYourNextTurn,
                granted_to: PlayerId(1),
            });

        // Active player is P0 — only P0's permission should expire.
        prune_until_next_turn_casting_permissions(&mut state, PlayerId(0));

        assert!(
            state.objects[&card_a].casting_permissions.is_empty(),
            "P0's UntilYourNextTurn permission should expire on P0's untap"
        );
        assert_eq!(
            state.objects[&card_b].casting_permissions.len(),
            1,
            "P1's permission must survive P0's untap"
        );
    }

    #[test]
    fn until_your_next_turn_prune_ignores_end_of_turn_duration() {
        let mut state = setup();
        let exiled = make_exiled_card(&mut state, PlayerId(0));
        state
            .objects
            .get_mut(&exiled)
            .unwrap()
            .casting_permissions
            .push(CastingPermission::PlayFromExile {
                duration: Duration::UntilEndOfTurn,
                granted_to: PlayerId(0),
            });

        prune_until_next_turn_casting_permissions(&mut state, PlayerId(0));

        assert_eq!(
            state.objects[&exiled].casting_permissions.len(),
            1,
            "UntilEndOfTurn permissions are pruned by the cleanup step, not untap"
        );
    }

    /// CR 113.6 + CR 113.6b: Anger (Onslaught / Incarnation cycle) —
    /// "As long as this card is in your graveyard and you control a Mountain,
    /// creatures you control have haste." The static's `active_zones` opts
    /// into Graveyard, so when Anger is in its controller's graveyard and that
    /// controller also controls a Mountain, the anthem grants Haste to every
    /// creature they control.
    #[test]
    fn incarnation_anger_grants_haste_from_graveyard_when_mountain_controlled() {
        use crate::types::keywords::Keyword;

        let mut state = setup();

        // Anger in player 0's graveyard.
        let anger = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Anger".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&anger).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ))
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste,
                    }])
                    .condition(StaticCondition::And {
                        conditions: vec![
                            StaticCondition::SourceInZone {
                                zone: Zone::Graveyard,
                            },
                            StaticCondition::IsPresent {
                                filter: Some(TargetFilter::Typed(TypedFilter {
                                    type_filters: vec![TypeFilter::Subtype("Mountain".to_string())],
                                    controller: Some(ControllerRef::You),
                                    properties: vec![],
                                })),
                            },
                        ],
                    })
                    .active_zones(vec![Zone::Graveyard]),
            );
        }
        state.players[0].graveyard.push_back(anger);

        // Mountain on player 0's battlefield.
        let mountain = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&mountain).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Mountain".to_string());
        }

        // Bear (creature you control), no intrinsic haste.
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert!(
            bear_obj.has_keyword(&Keyword::Haste),
            "Anger in graveyard + Mountain controlled must grant Haste to creatures you control"
        );
    }

    /// CR 604.1 / CR 613.1: Anger's compound `IsPresent(Mountain)` side must
    /// evaluate false when the controller has no Mountain, so no anthem
    /// applies even though Anger is in the graveyard and the zone gate passes.
    #[test]
    fn incarnation_anger_without_mountain_grants_no_haste() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let anger = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Anger".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&anger).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ))
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste,
                    }])
                    .condition(StaticCondition::And {
                        conditions: vec![
                            StaticCondition::SourceInZone {
                                zone: Zone::Graveyard,
                            },
                            StaticCondition::IsPresent {
                                filter: Some(TargetFilter::Typed(TypedFilter {
                                    type_filters: vec![TypeFilter::Subtype("Mountain".to_string())],
                                    controller: Some(ControllerRef::You),
                                    properties: vec![],
                                })),
                            },
                        ],
                    })
                    .active_zones(vec![Zone::Graveyard]),
            );
        }
        state.players[0].graveyard.push_back(anger);

        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert!(
            !bear_obj.has_keyword(&Keyword::Haste),
            "Without a Mountain, the compound condition fails and Haste is not granted"
        );
    }

    /// CR 113.6 + CR 113.6b: Sanity check for the zone-of-function gate.
    /// When Anger is on the battlefield (not in the graveyard), the compound
    /// `SourceInZone(Graveyard)` arm evaluates false, so the anthem must not
    /// apply — verifying the condition check correctly dis-applies the static
    /// even though it would otherwise function on the battlefield default.
    #[test]
    fn incarnation_anger_on_battlefield_does_not_grant_haste() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        // Mountain on player 0's battlefield.
        let mountain = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&mountain).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Mountain".to_string());
        }

        // Anger on battlefield (not graveyard). Its active_zones lists only
        // Graveyard, so the per-static zone gate drops its effects regardless
        // of the condition.
        let anger = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Anger".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&anger).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ))
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste,
                    }])
                    .condition(StaticCondition::And {
                        conditions: vec![
                            StaticCondition::SourceInZone {
                                zone: Zone::Graveyard,
                            },
                            StaticCondition::IsPresent {
                                filter: Some(TargetFilter::Typed(TypedFilter {
                                    type_filters: vec![TypeFilter::Subtype("Mountain".to_string())],
                                    controller: Some(ControllerRef::You),
                                    properties: vec![],
                                })),
                            },
                        ],
                    })
                    .active_zones(vec![Zone::Graveyard]),
            );
        }

        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert!(
            !bear_obj.has_keyword(&Keyword::Haste),
            "Anger on battlefield (outside its active_zones) must not grant Haste"
        );
    }

    // ---------------------------------------------------------------
    // CR 305.6: Basic-land subtype additions inject their mana ability.
    // ---------------------------------------------------------------

    fn make_land_with_mana(
        state: &mut GameState,
        name: &str,
        controller: PlayerId,
        color: ManaColor,
    ) -> ObjectId {
        let land = create_object(
            state,
            CardId(9000),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![color],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        obj.base_abilities = Arc::new(vec![ability.clone()]);
        obj.abilities = Arc::new(vec![ability]);
        land
    }

    fn add_global_land_subtype_static(state: &mut GameState, host: ObjectId, subtype: &str) {
        let obj = state.objects.get_mut(&host).unwrap();
        obj.static_definitions.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(TypedFilter::land()))
                .modifications(vec![ContinuousModification::AddSubtype {
                    subtype: subtype.to_string(),
                }]),
        );
    }

    fn count_mana_abilities(obj: &crate::game::game_object::GameObject, color: ManaColor) -> usize {
        obj.abilities
            .iter()
            .filter(|ability| {
                matches!(ability.kind, AbilityKind::Activated)
                    && matches!(ability.cost, Some(AbilityCost::Tap))
                    && matches!(
                        &*ability.effect,
                        Effect::Mana {
                            produced: ManaProduction::Fixed { colors, .. },
                            ..
                        } if colors.as_slice() == [color]
                    )
            })
            .count()
    }

    #[test]
    fn urborg_adds_swamp_mana_ability_to_every_land() {
        // Urborg, Tomb of Yawgmoth makes every land a Swamp IN ADDITION to
        // its other types. A Mountain should retain `{T}: Add {R}` AND gain
        // `{T}: Add {B}` (CR 305.6).
        let mut state = setup();
        let mountain = make_land_with_mana(&mut state, "Mountain", PlayerId(0), ManaColor::Red);
        let urborg = make_land_with_mana(&mut state, "Urborg", PlayerId(0), ManaColor::Black);
        add_global_land_subtype_static(&mut state, urborg, "Swamp");

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let mountain_obj = state.objects.get(&mountain).unwrap();
        assert_eq!(
            count_mana_abilities(mountain_obj, ManaColor::Red),
            1,
            "Mountain must retain its {{T}}: Add {{R}} ability"
        );
        assert_eq!(
            count_mana_abilities(mountain_obj, ManaColor::Black),
            1,
            "Mountain must gain {{T}}: Add {{B}} from the injected Swamp subtype"
        );
    }

    #[test]
    fn yavimaya_adds_forest_mana_ability_to_plains() {
        let mut state = setup();
        let plains = make_land_with_mana(&mut state, "Plains", PlayerId(0), ManaColor::White);
        let yavimaya = make_land_with_mana(&mut state, "Yavimaya", PlayerId(0), ManaColor::Green);
        add_global_land_subtype_static(&mut state, yavimaya, "Forest");

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let plains_obj = state.objects.get(&plains).unwrap();
        assert_eq!(count_mana_abilities(plains_obj, ManaColor::White), 1);
        assert_eq!(count_mana_abilities(plains_obj, ManaColor::Green), 1);
    }

    #[test]
    fn double_urborg_only_injects_one_swamp_mana_ability() {
        // Idempotency: two copies of Urborg in play must not stack the Swamp
        // mana ability twice on every land.
        let mut state = setup();
        let mountain = make_land_with_mana(&mut state, "Mountain", PlayerId(0), ManaColor::Red);
        let urborg1 = make_land_with_mana(&mut state, "Urborg", PlayerId(0), ManaColor::Black);
        add_global_land_subtype_static(&mut state, urborg1, "Swamp");
        let urborg2 = make_land_with_mana(&mut state, "Urborg", PlayerId(0), ManaColor::Black);
        add_global_land_subtype_static(&mut state, urborg2, "Swamp");

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let mountain_obj = state.objects.get(&mountain).unwrap();
        assert_eq!(
            count_mana_abilities(mountain_obj, ManaColor::Black),
            1,
            "Two Urborgs must inject exactly one Swamp mana ability"
        );
    }

    #[test]
    fn basic_swamp_receives_no_duplicate_swamp_ability_from_urborg() {
        // An actual basic Swamp already has `{T}: Add {B}`. Urborg adding
        // Swamp to it must not append a second `{T}: Add {B}`.
        let mut state = setup();
        let basic_swamp = make_land_with_mana(&mut state, "Swamp", PlayerId(0), ManaColor::Black);
        state
            .objects
            .get_mut(&basic_swamp)
            .unwrap()
            .card_types
            .subtypes
            .push("Swamp".to_string());
        // Ensure base_card_types mirrors so layers reset doesn't lose it.
        state
            .objects
            .get_mut(&basic_swamp)
            .unwrap()
            .base_card_types
            .subtypes
            .push("Swamp".to_string());
        let urborg = make_land_with_mana(&mut state, "Urborg", PlayerId(0), ManaColor::Black);
        add_global_land_subtype_static(&mut state, urborg, "Swamp");

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let swamp_obj = state.objects.get(&basic_swamp).unwrap();
        assert_eq!(
            count_mana_abilities(swamp_obj, ManaColor::Black),
            1,
            "Basic Swamp must keep a single {{T}}: Add {{B}} ability"
        );
    }

    #[test]
    fn urborg_does_not_inject_mana_onto_non_land() {
        // Defensive: the injection must be guarded by CoreType::Land. An
        // Urborg-like static whose filter accidentally matched a creature
        // should not grant mana abilities.
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let host = make_land_with_mana(&mut state, "GhostHost", PlayerId(0), ManaColor::Black);
        // Use a self-targeted static so we can exercise the AddSubtype path
        // on a non-land directly.
        let bear_obj = state.objects.get_mut(&bear).unwrap();
        bear_obj.static_definitions.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddSubtype {
                    subtype: "Swamp".to_string(),
                }]),
        );
        // Silence host warning.
        let _ = host;

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert_eq!(
            count_mana_abilities(bear_obj, ManaColor::Black),
            0,
            "Non-land objects must not receive injected mana abilities"
        );
    }

    /// CR 613.1f: Granting the same ability twice in a single layer pass must
    /// be idempotent. Ragost, Deft Gastronaut parses two identical
    /// `GrantAbility` modifications for its "Artifacts you control ... have
    /// '{2}, {T}, Sacrifice: You gain 3 life'" clause; the layer system must
    /// deduplicate so each artifact ends up with exactly one granted ability.
    /// The same dedup must also hold across two distinct Ragosts granting the
    /// same ability to the same artifact.
    fn ragost_food_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: GainLifePlayer::Controller,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1,
                },
            ],
        })
    }

    fn make_artifact(state: &mut GameState, name: &str, player: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(0),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
        obj.timestamp = ts;
        id
    }

    fn count_food_abilities(obj: &crate::game::game_object::GameObject) -> usize {
        let target = ragost_food_ability();
        obj.abilities.iter().filter(|a| **a == target).count()
    }

    fn ragost_static(ability: AbilityDefinition) -> StaticDefinition {
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
            ))
            .modifications(vec![
                ContinuousModification::GrantAbility {
                    definition: Box::new(ability.clone()),
                },
                ContinuousModification::AddSubtype {
                    subtype: "Food".to_string(),
                },
                // Parser emits the GrantAbility twice (see Ragost card data):
                // the "have ..." clause round-trips through two handlers.
                ContinuousModification::GrantAbility {
                    definition: Box::new(ability),
                },
            ])
    }

    #[test]
    fn ragost_duplicate_grant_ability_dedups_to_single_ability() {
        let mut state = setup();
        let ragost = make_creature(&mut state, "Ragost", 2, 2, PlayerId(0));
        let artifact1 = make_artifact(&mut state, "Artifact 1", PlayerId(0));
        let artifact2 = make_artifact(&mut state, "Artifact 2", PlayerId(0));
        let artifact3 = make_artifact(&mut state, "Artifact 3", PlayerId(0));

        let static_def = ragost_static(ragost_food_ability());
        state
            .objects
            .get_mut(&ragost)
            .unwrap()
            .static_definitions
            .push(static_def);

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        for id in [artifact1, artifact2, artifact3] {
            let obj = state.objects.get(&id).unwrap();
            assert_eq!(
                count_food_abilities(obj),
                1,
                "each artifact must have exactly one granted Food ability"
            );
        }

        // Idempotency across layer passes: running layers twice must not stack.
        state.layers_dirty = true;
        evaluate_layers(&mut state);
        for id in [artifact1, artifact2, artifact3] {
            assert_eq!(count_food_abilities(&state.objects[&id]), 1);
        }
    }

    #[test]
    fn two_ragosts_grant_food_ability_only_once() {
        let mut state = setup();
        let ragost_a = make_creature(&mut state, "Ragost A", 2, 2, PlayerId(0));
        let ragost_b = make_creature(&mut state, "Ragost B", 2, 2, PlayerId(0));
        let artifact = make_artifact(&mut state, "Artifact", PlayerId(0));

        for host in [ragost_a, ragost_b] {
            let static_def = ragost_static(ragost_food_ability());
            state
                .objects
                .get_mut(&host)
                .unwrap()
                .static_definitions
                .push(static_def);
        }

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        assert_eq!(
            count_food_abilities(&state.objects[&artifact]),
            1,
            "two Ragosts must not stack the granted Food ability",
        );
    }

    // -- CR 302.6 control-change sickness diff --
    //
    // Helper: add a Layer 2 ChangeController effect targeting `target_id`,
    // controlled by `new_controller` (i.e., they become the effect's controller
    // and per CR 613.1b the new effective controller of `target_id`).
    fn add_change_controller_effect(
        state: &mut GameState,
        source_id: ObjectId,
        target_id: ObjectId,
        new_controller: PlayerId,
        duration: Duration,
    ) -> u64 {
        state.add_transient_continuous_effect(
            source_id,
            new_controller,
            duration,
            TargetFilter::SpecificObject { id: target_id },
            vec![ContinuousModification::ChangeController],
            None,
        )
    }

    /// CR 302.6 + CR 613.1b: Act-of-Treason-style mid-game control change.
    /// A creature whose effective controller flips from P0 to P1 must become
    /// summoning-sick for P1 (the new controller has not had it
    /// "continuously since their most recent turn began").
    #[test]
    fn control_change_sicks_new_controller() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        // Pre-existing creature — clear sickness as if controller had a prior turn.
        state.objects.get_mut(&bear).unwrap().summoning_sick = false;
        evaluate_layers(&mut state);
        assert!(
            !state.objects[&bear].summoning_sick,
            "stable creature, no control change → not sick"
        );

        // Apply Act-of-Treason-style control change: P1 takes control of bear.
        let _eid = add_change_controller_effect(
            &mut state,
            bear,
            bear,
            PlayerId(1),
            Duration::UntilEndOfTurn,
        );
        evaluate_layers(&mut state);

        assert_eq!(
            state.objects[&bear].controller,
            PlayerId(1),
            "Layer 2 should have applied the ChangeController effect"
        );
        assert!(
            state.objects[&bear].summoning_sick,
            "control change P0→P1 must re-apply summoning sickness (CR 302.6)"
        );
    }

    /// CR 302.6: When a control-changing effect expires, the permanent
    /// reverts to its owner per CR 613.1b's owner-reset. That reversion is
    /// itself a control transition and must re-sick the original owner —
    /// continuity broke during the opponent's tenure.
    #[test]
    fn control_change_expiry_resicks_original_controller() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        state.objects.get_mut(&bear).unwrap().summoning_sick = false;

        let eid = add_change_controller_effect(
            &mut state,
            bear,
            bear,
            PlayerId(1),
            Duration::UntilEndOfTurn,
        );
        evaluate_layers(&mut state);
        // Simulate the original owner clearing sickness via their next turn.
        state.objects.get_mut(&bear).unwrap().summoning_sick = false;
        // Re-eval with effect still present: stable, no flip.
        evaluate_layers(&mut state);
        assert!(
            !state.objects[&bear].summoning_sick,
            "stable Control Magic must not re-sick on every eval"
        );

        // Effect expires — drop it from the transient list.
        state.transient_continuous_effects.retain(|e| e.id != eid);
        state.layers_dirty = true;
        evaluate_layers(&mut state);

        assert_eq!(
            state.objects[&bear].controller,
            PlayerId(0),
            "owner-reset (CR 613.1b) reverts controller to owner on expiry"
        );
        assert!(
            state.objects[&bear].summoning_sick,
            "expiry-revert P1→P0 is a control transition → sick again (CR 302.6)"
        );
    }

    /// CR 302.6: Exchange-control sicks BOTH permanents — each side sees a
    /// new effective controller, so continuity breaks symmetrically.
    #[test]
    fn exchange_control_sicks_both_permanents() {
        let mut state = setup();
        let bear_a = make_creature(&mut state, "Bear A", 2, 2, PlayerId(0));
        let bear_b = make_creature(&mut state, "Bear B", 2, 2, PlayerId(1));
        for id in [bear_a, bear_b] {
            state.objects.get_mut(&id).unwrap().summoning_sick = false;
        }

        // Swap: A becomes controlled by P1, B becomes controlled by P0.
        add_change_controller_effect(&mut state, bear_a, bear_a, PlayerId(1), Duration::Permanent);
        add_change_controller_effect(&mut state, bear_b, bear_b, PlayerId(0), Duration::Permanent);
        evaluate_layers(&mut state);

        assert_eq!(state.objects[&bear_a].controller, PlayerId(1));
        assert_eq!(state.objects[&bear_b].controller, PlayerId(0));
        assert!(
            state.objects[&bear_a].summoning_sick,
            "exchanged permanent A: new controller, sick (CR 302.6)"
        );
        assert!(
            state.objects[&bear_b].summoning_sick,
            "exchanged permanent B: new controller, sick (CR 302.6)"
        );
    }

    /// Defensive: a permanent that exits the battlefield mid-pass (e.g., SBA
    /// destroys it during a chained effect) still appears in the pre-pass
    /// snapshot. The post-pass `get_mut` must None-guard gracefully.
    #[test]
    fn permanent_removed_during_eval_does_not_panic() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        state.objects.get_mut(&bear).unwrap().summoning_sick = false;

        // Snapshot will capture `bear` once eval starts; remove it before the
        // diff runs by simulating a mid-pass drop. The cleanest reproduction
        // here is to evaluate normally (stable), then manually drop the
        // object and evaluate again — the snapshot at the second eval will
        // include `bear`, but mid-pass we delete it. We approximate by
        // dropping it from `state.objects` between snapshot and diff via a
        // direct call boundary; here we just verify the post-eval-get-after-
        // remove doesn't crash on a separate eval cycle.
        evaluate_layers(&mut state);
        state.objects.remove(&bear);
        state.layers_dirty = true;
        // No panic; the diff loop's `get_mut(...).if let Some` swallows it.
        evaluate_layers(&mut state);
    }

    /// CR 702.16g: "Protection from [A] and from [B]" behaves as two separate
    /// protection abilities. The Mirran Sword cycle (Sword of Truth and
    /// Justice, Sword of Fire and Ice, etc.) emits both colors as separate
    /// `AddKeyword(Protection(_))` modifications; the layer applier must
    /// preserve both even though they share an enum discriminant.
    #[test]
    fn add_keyword_preserves_distinct_protection_parameters() {
        use crate::types::keywords::ProtectionTarget;
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        let source = make_creature(&mut state, "Sword of Truth and Justice", 1, 1, PlayerId(0));
        let def = StaticDefinition::continuous()
            .affected(creature_you_ctrl())
            .modifications(vec![
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(ProtectionTarget::Color(ManaColor::White)),
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(ProtectionTarget::Color(ManaColor::Blue)),
                },
            ]);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&bear).unwrap();
        let protection_keywords: Vec<&Keyword> = obj
            .keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Protection(_)))
            .collect();
        assert_eq!(
            protection_keywords.len(),
            2,
            "both Protection(White) and Protection(Blue) must coexist; got {:?}",
            obj.keywords
        );
        assert!(obj
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::White
            ))));
        assert!(obj
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Blue
            ))));
    }

    /// CR 702.21: Ward is parameterized by cost. Two separate `Ward(_)` grants
    /// with different costs must both persist on the keyword list — the
    /// targeting player pays each cost (CR 702.21b) so dropping one is a
    /// silent rules violation. Regression-protects the same fix that unblocks
    /// multi-protection swords for any future multi-Ward grants.
    #[test]
    fn add_keyword_preserves_distinct_ward_parameters() {
        use crate::types::keywords::WardCost;
        use crate::types::mana::ManaCost;
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        let ward_one = Keyword::Ward(WardCost::Mana(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        }));
        let ward_two = Keyword::Ward(WardCost::PayLife(2));

        let source = make_creature(&mut state, "Source", 1, 1, PlayerId(0));
        let def = StaticDefinition::continuous()
            .affected(creature_you_ctrl())
            .modifications(vec![
                ContinuousModification::AddKeyword {
                    keyword: ward_one.clone(),
                },
                ContinuousModification::AddKeyword {
                    keyword: ward_two.clone(),
                },
            ]);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&bear).unwrap();
        let ward_keywords: Vec<&Keyword> = obj
            .keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Ward(_)))
            .collect();
        assert_eq!(
            ward_keywords.len(),
            2,
            "both Ward({{1}}) and Ward({{2}}) must coexist; got {:?}",
            obj.keywords
        );
        assert!(obj.keywords.contains(&ward_one));
        assert!(obj.keywords.contains(&ward_two));
    }

    /// CR 702.16m: Multiple instances of protection from the same quality on
    /// the same permanent are redundant. `AddKeyword` must still deduplicate
    /// when the parameter is identical — two grants of `Protection(White)`
    /// from different sources should land as a single keyword entry.
    #[test]
    fn add_keyword_deduplicates_identical_parameters() {
        use crate::types::keywords::ProtectionTarget;
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        let source = make_creature(&mut state, "Source", 1, 1, PlayerId(0));
        let def = StaticDefinition::continuous()
            .affected(creature_you_ctrl())
            .modifications(vec![
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(ProtectionTarget::Color(ManaColor::White)),
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(ProtectionTarget::Color(ManaColor::White)),
                },
            ]);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&bear).unwrap();
        let white_protections = obj
            .keywords
            .iter()
            .filter(|k| {
                matches!(
                    k,
                    Keyword::Protection(ProtectionTarget::Color(ManaColor::White))
                )
            })
            .count();
        assert_eq!(
            white_protections, 1,
            "duplicate identical-parameter keyword grants must collapse to a single entry"
        );
    }

    /// Regression: `keywords::has_keyword` is documented as discriminant-only
    /// matching ("any kind of Protection"). The layer-applier fix must not
    /// migrate that helper — generic-presence checks elsewhere in the engine
    /// (e.g. "is this creature protected from anything?") still rely on the
    /// discriminant semantic. Verify both call shapes coexist correctly.
    #[test]
    fn has_keyword_remains_discriminant_only_for_generic_presence_queries() {
        use crate::types::keywords::ProtectionTarget;
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let obj = state.objects.get_mut(&bear).unwrap();
        obj.keywords
            .push(Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Blue,
            )));

        let obj = state.objects.get(&bear).unwrap();
        // Discriminant-based query: "do you have any kind of Protection?" — yes.
        assert!(
            obj.has_keyword(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::White
            )))
        );
        // PartialEq query: "do you have Protection from White specifically?" — no.
        assert!(!obj
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::White
            ))));
        assert!(obj
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Blue
            ))));
    }

    /// CR 903.3d + CR 702.21: "Commanders you control have ward {2}." —
    /// Codsworth, Handy Helper. The static must grant Ward to a controlled
    /// commander on the battlefield, and must NOT grant it to a non-commander
    /// creature you control. Verifies the FilterProp::IsCommander runtime path.
    #[test]
    fn codsworth_ward_grant_targets_only_commanders() {
        use crate::types::ability::{FilterProp, TargetFilter, TypedFilter};
        use crate::types::keywords::WardCost;
        use crate::types::mana::ManaCost;
        let mut state = setup();

        let codsworth = make_creature(&mut state, "Codsworth", 2, 3, PlayerId(0));
        let commander = make_creature(&mut state, "MyCommander", 4, 4, PlayerId(0));
        state.objects.get_mut(&commander).unwrap().is_commander = true;
        // A vanilla creature you control — must not receive Ward.
        let vanilla = make_creature(&mut state, "VanillaBear", 2, 2, PlayerId(0));

        let ward = Keyword::Ward(WardCost::Mana(ManaCost::Cost {
            generic: 2,
            shards: vec![],
        }));
        let def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::IsCommander]),
            ))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: ward.clone(),
            }]);
        state
            .objects
            .get_mut(&codsworth)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        assert!(
            state.objects[&commander].keywords.contains(&ward),
            "commander must receive Ward grant"
        );
        assert!(
            !state.objects[&vanilla].keywords.contains(&ward),
            "non-commander must NOT receive Ward grant"
        );
    }

    /// CR 903.3d: When no commander is on the battlefield, a "commanders you
    /// control" static is a no-op — it must not affect any other permanent.
    #[test]
    fn commanders_you_control_static_no_op_without_commander() {
        use crate::types::ability::{FilterProp, TargetFilter, TypedFilter};
        let mut state = setup();
        let codsworth = make_creature(&mut state, "Codsworth", 2, 3, PlayerId(0));
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        let def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::IsCommander]),
            ))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }]);
        state
            .objects
            .get_mut(&codsworth)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        assert!(!state.objects[&bear].keywords.contains(&Keyword::Hexproof));
        assert!(!state.objects[&codsworth]
            .keywords
            .contains(&Keyword::Hexproof));
    }

    /// CR 903.3d: Each player's commander receives Ward only from their own
    /// controller's Codsworth. A second Codsworth controlled by the opponent
    /// does NOT grant Ward to your commander (filter is `controller=You`).
    #[test]
    fn commanders_you_control_filter_respects_per_player_scope() {
        use crate::types::ability::{FilterProp, TargetFilter, TypedFilter};
        use crate::types::keywords::WardCost;
        use crate::types::mana::ManaCost;
        let mut state = setup();

        let codsworth_p0 = make_creature(&mut state, "Codsworth_P0", 2, 3, PlayerId(0));
        let cmd_p0 = make_creature(&mut state, "Cmd_P0", 4, 4, PlayerId(0));
        state.objects.get_mut(&cmd_p0).unwrap().is_commander = true;
        let cmd_p1 = make_creature(&mut state, "Cmd_P1", 4, 4, PlayerId(1));
        state.objects.get_mut(&cmd_p1).unwrap().is_commander = true;

        let ward = Keyword::Ward(WardCost::Mana(ManaCost::Cost {
            generic: 2,
            shards: vec![],
        }));
        let def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::IsCommander]),
            ))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: ward.clone(),
            }]);
        state
            .objects
            .get_mut(&codsworth_p0)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        assert!(
            state.objects[&cmd_p0].keywords.contains(&ward),
            "P0's commander must receive Ward from P0's Codsworth"
        );
        assert!(
            !state.objects[&cmd_p1].keywords.contains(&ward),
            "P1's commander must NOT receive Ward from P0's Codsworth"
        );
    }
}
