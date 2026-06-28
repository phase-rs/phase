use std::collections::HashSet;
use std::sync::Arc;

use crate::database::synthesis::KeywordTriggerInstaller;
use crate::game::arithmetic::saturating_pt_add;
use crate::game::conditions::{
    counter_condition_matches, eval_chosen_label_is, eval_class_level_ge, eval_has_city_blessing,
    eval_is_initiative, eval_is_monarch, eval_no_monarch, eval_recipient_attacking_owner_target,
    eval_source_entered_this_turn, eval_source_has_dealt_damage, eval_source_in_zone,
    eval_source_is_attacking, eval_source_is_tapped_on_battlefield,
};
use crate::game::devotion::count_devotion;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::game_object::DisplaySource;
use crate::game::printed_cards::{
    apply_copiable_values, ensure_keyword_triggers_for_copiable_values, intrinsic_copiable_values,
};
use crate::game::quantity::{
    continuous_modification_dynamic_quantity, filter_uses_recipient, quantity_expr_uses_recipient,
    QuantityContext,
};
use crate::game::speed::{effective_speed, has_max_speed};
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, BasicLandType, CastingPermission,
    ChosenSubtypeKind, CommanderOwnership, ContinuousModification, CopiableValues, Duration,
    Effect, FilterProp, ManaContribution, ManaProduction, PlayerScope, QuantityExpr,
    StaticCondition, StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::attribution::EffectRef;
use crate::types::card_type::{
    is_land_subtype, noncreature_subtype_set, CoreType, SubtypeSet, Supertype,
};
use crate::types::counter::{has_positive_counters, CounterType};
#[cfg(test)]
use crate::types::game_state::MayTriggerOrigin;
use crate::types::game_state::{
    DayNight, GameState, LayersDirty, StaticGateKey, TransientContinuousEffect,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
#[cfg(test)]
use crate::types::keywords::KeywordKind;
use crate::types::layers::{ActiveContinuousEffect, Layer};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;

#[derive(Debug, Clone)]
struct ActiveCombatAssignmentRuleEffect {
    source_id: ObjectId,
    controller: PlayerId,
    timestamp: u64,
    modification: ContinuousModification,
    affected_filter: TargetFilter,
    condition: Option<StaticCondition>,
}

// CR 205.3c: Each subtype is correlated to its appropriate card type.
/// CR 205.1a: Whether a subtype correlates to at least one of the given core
/// types — i.e. whether it survives a card-type replacement. Shared with the
/// token-copy "stamp at creation" path so both the layered and baked
/// applications of `SetCardTypes` drop the same uncorrelated subtypes.
pub(crate) fn subtype_matches_core_types(
    subtype: &str,
    core_types: &[CoreType],
    all_creature_types: &[String],
) -> bool {
    let Some(set) = noncreature_subtype_set(subtype) else {
        return core_types.contains(&CoreType::Creature)
            || core_types.contains(&CoreType::Kindred)
            || all_creature_types
                .iter()
                .any(|creature_type| creature_type == subtype);
    };
    core_types.iter().any(|core_type| {
        matches!(
            (core_type, set),
            (CoreType::Artifact, SubtypeSet::Artifact)
                | (CoreType::Enchantment, SubtypeSet::Enchantment)
                | (CoreType::Land, SubtypeSet::Land)
                | (CoreType::Planeswalker, SubtypeSet::Planeswalker)
                | (CoreType::Instant | CoreType::Sorcery, SubtypeSet::Spell)
                | (CoreType::Battle, SubtypeSet::Battle)
        )
    })
}

/// Remove transient effects that have expired based on their duration.
/// Called during cleanup (end of turn) to prune `UntilEndOfTurn` effects.
/// CR 514.2: End-of-turn continuous effects expire at cleanup.
pub fn prune_end_of_turn_effects(state: &mut GameState) {
    let before = state.transient_continuous_effects.len();
    state
        .transient_continuous_effects
        .retain(|e| e.duration != Duration::UntilEndOfTurn);
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty.mark_full();
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
        state.layers_dirty.mark_full();
    }
}

/// CR 513.1 + CR 611.2a: Remove transient continuous effects whose
/// `Duration::UntilNextStepOf { step: Phase::End, player: Controller }` expires at the start of
/// `active_player`'s end step. Called from `turns.rs::auto_advance` at the
/// End phase alongside `prune_end_step_casting_permissions` so any future
/// parser arm that emits this duration on a `TimedContinuousEffect` (pump,
/// control change, etc.) is pruned by its scheduled step rather than
/// outliving it.
pub fn prune_until_next_end_step_effects(state: &mut GameState, active_player: PlayerId) {
    let before = state.transient_continuous_effects.len();
    state.transient_continuous_effects.retain(|e| {
        !(matches!(
            e.duration,
            Duration::UntilNextStepOf {
                step: Phase::End,
                player: PlayerScope::Controller
            }
        ) && e.controller == active_player)
    });
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty.mark_full();
    }
}

/// CR 514.2: Remove durational casting permissions whose
/// `Duration::UntilEndOfTurn` expires at cleanup. Called from the cleanup step
/// alongside `prune_end_of_turn_effects`.
// CR 611.2a: Consumers of the `duration`-bearing variants — `PlayFromExile`
// (impulse-draw, Light Up the Stage class) and `ExileWithAltCost`
// (Rebound, CR 702.88a).
///
/// Variants without a `duration` field (`AdventureCreature`,
/// `ExileWithEnergyCost`, `WarpExile`, `Plotted`, `Foretold`) and
/// `ExileWithAltCost { duration: None }` (Airbending, Suspend, Discover,
/// Cascade) persist until the object leaves exile (handled by
/// `zones::apply_zone_exit_cleanup`).
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
                duration: Duration::UntilNextTurnOf { .. } | Duration::Permanent,
                ..
            } => true,
            // CR 513.1: `UntilNextStepOf { step: End }` is expired by
            // `prune_end_step_casting_permissions` at the End phase entry,
            // NOT at cleanup. Retain here.
            CastingPermission::PlayFromExile {
                duration:
                    Duration::UntilNextStepOf {
                        step: Phase::End, ..
                    },
                ..
            } => true,
            // UntilHostLeavesPlay / ForAsLongAs / UntilNextStepOf { step: Untap }:
            // these are pruned by their own systems (zone-exit cleanup, condition
            // re-evaluation, untap step). Retain here — they are not end-of-turn.
            CastingPermission::PlayFromExile { .. } => true,
            // CR 702.88a: Rebound's upkeep recast offer carries
            // `duration: Some(UntilEndOfTurn)` so the granted "cast this
            // card without paying its mana cost" permission expires at the
            // end of the same turn if the controller declines or fails to
            // cast it. Mirrors the PlayFromExile arms above so all
            // durational casting permissions share the same pruning
            // semantics.
            CastingPermission::ExileWithAltCost {
                duration: Some(Duration::UntilEndOfTurn),
                ..
            } => false,
            // CR 514.2: defensive — same handling as PlayFromExile.
            CastingPermission::ExileWithAltCost {
                duration: Some(Duration::UntilEndOfCombat),
                ..
            } => false,
            // CR 513.1: end-step duration handled by
            // `prune_end_step_casting_permissions`; retain here.
            CastingPermission::ExileWithAltCost {
                duration:
                    Some(Duration::UntilNextStepOf {
                        step: Phase::End, ..
                    }),
                ..
            } => true,
            // Other durational shapes (UntilNextTurnOf, Permanent, etc.)
            // and the standing `duration: None` form persist here.
            CastingPermission::AdventureCreature
            | CastingPermission::ExileWithAltCost { .. }
            | CastingPermission::ExileWithAltAbilityCost { .. }
            | CastingPermission::ExileWithEnergyCost
            | CastingPermission::WarpExile { .. }
            // CR 702.170d: Plotted persists across turns (that is the whole
            // point of Plot — cast "on a later turn"); never pruned at cleanup.
            | CastingPermission::Plotted { .. }
            // CR 702.143a: Foretold permissions likewise persist while the
            // card remains in exile so it can be cast on a later turn.
            | CastingPermission::Foretold { .. } => true,
        });
    }
    // CR 601.2a + CR 603.7 + CR 611.2a: Garbage-collect single-use consumed
    // markers whose grant has expired. After the prune above, drop any consumed
    // tracked-set entry that no longer has a live single-use `PlayFromExile`
    // grant in exile, so the set does not grow without bound.
    let live_single_use_groups: std::collections::HashSet<crate::types::identifiers::TrackedSetId> =
        state
            .objects
            .values()
            .flat_map(|obj| {
                obj.casting_permissions.iter().filter_map(|p| match p {
                    CastingPermission::PlayFromExile {
                        single_use_group,
                        single_use: true,
                        ..
                    } => *single_use_group,
                    _ => None,
                })
            })
            .collect();
    state
        .exile_play_single_use_consumed
        .retain(|group| live_single_use_groups.contains(group));
}

/// CR 514.2: Remove durational casting permissions granted to
/// `active_player` whose `Duration::UntilNextTurnOf { Controller }` expires
/// at that player's untap step. Called from the untap step alongside
/// `prune_until_next_turn_effects`.
// CR 611.2a: Consumers of the `duration`-bearing variants — `PlayFromExile`
// (impulse-draw, Light Up the Stage class) and `ExileWithAltCost`
// (Rebound, CR 702.88a).
pub fn prune_until_next_turn_casting_permissions(state: &mut GameState, active_player: PlayerId) {
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        // CR 514.2: arm "until the end of your next turn" play-permissions when
        // the grantee's next turn begins — convert to `UntilEndOfTurn` so the
        // cleanup-step prune (`prune_end_of_turn_casting_permissions`) ends them
        // at this turn's cleanup, letting the cards be played throughout the
        // grantee's next turn (Light Up the Stage class).
        for p in obj.casting_permissions.iter_mut() {
            if let CastingPermission::PlayFromExile {
                duration,
                granted_to,
                ..
            } = p
            {
                if *granted_to == active_player
                    && matches!(
                        duration,
                        Duration::UntilEndOfNextTurnOf {
                            player: PlayerScope::Controller
                        }
                    )
                {
                    *duration = Duration::UntilEndOfTurn;
                }
            }
            // CR 514.2: same arming for durational `ExileWithAltCost`
            // (Rebound-class). `granted_to` is `Option<PlayerId>`; only
            // arm when set and matching the active player.
            if let CastingPermission::ExileWithAltCost {
                duration: Some(d),
                granted_to: Some(g),
                ..
            } = p
            {
                if *g == active_player
                    && matches!(
                        d,
                        Duration::UntilEndOfNextTurnOf {
                            player: PlayerScope::Controller
                        }
                    )
                {
                    *d = Duration::UntilEndOfTurn;
                }
            }
        }

        obj.casting_permissions.retain(|p| match p {
            CastingPermission::PlayFromExile {
                duration:
                    Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller,
                    },
                granted_to,
                ..
            } => *granted_to != active_player,
            // CR 513.1 + CR 611.2a/b: `UntilNextStepOf { step: End }` is
            // expired by `prune_end_step_casting_permissions` at the end
            // step, NOT at the untap step. Retain here.
            CastingPermission::PlayFromExile {
                duration:
                    Duration::UntilNextStepOf {
                        step: Phase::End, ..
                    },
                ..
            } => true,
            // CR 514.2: durational `ExileWithAltCost` with
            // `UntilNextTurnOf { Controller }` granted to the active
            // player expires at their untap step (mirrors PlayFromExile).
            CastingPermission::ExileWithAltCost {
                duration:
                    Some(Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller,
                    }),
                granted_to: Some(g),
                ..
            } => *g != active_player,
            // CR 513.1: end-step duration is handled by
            // `prune_end_step_casting_permissions`; retain here.
            CastingPermission::ExileWithAltCost {
                duration:
                    Some(Duration::UntilNextStepOf {
                        step: Phase::End, ..
                    }),
                ..
            } => true,
            CastingPermission::PlayFromExile { .. }
            | CastingPermission::AdventureCreature
            | CastingPermission::ExileWithAltCost { .. }
            | CastingPermission::ExileWithAltAbilityCost { .. }
            | CastingPermission::ExileWithEnergyCost
            | CastingPermission::WarpExile { .. }
            // CR 702.170d: Plotted persists across turns; never pruned at the
            // untap step. Retention is zone-scoped (see zones::apply_zone_exit_cleanup).
            | CastingPermission::Plotted { .. }
            | CastingPermission::Foretold { .. } => true,
        });
    }
}

/// CR 513.1: Remove durational casting permissions granted to
/// `active_player` whose `Duration::UntilNextStepOf { step: End, player: Controller }`
/// expires at that player's next end step. Called at the start of the
/// End phase in `turns.rs::auto_advance`.
// CR 611.2a: Consumers of the `duration`-bearing variants — `PlayFromExile`
// (Rocco, Street Chef class) and `ExileWithAltCost` (Rebound, CR 702.88a).
///
/// CR 513.2 ordering: this prune runs BEFORE end-step triggers fire, so a
/// new grant created by an end-step trigger (e.g., Rocco, Street Chef) is
/// NOT wiped by the same end step's prune — the new trigger cannot back up
/// per CR 513.2, so the new permission lands AFTER the prune completes.
///
/// 2023-05-12 Wizards ruling on Rocco, Street Chef: the permission outlives
/// the granting permanent leaving the battlefield. This prune keys off the
/// permission's `granted_to`, not the source object's presence on the
/// battlefield.
pub fn prune_end_step_casting_permissions(state: &mut GameState, active_player: PlayerId) {
    for obj in state.objects.iter_mut().map(|(_, v)| v) {
        obj.casting_permissions.retain(|p| match p {
            CastingPermission::PlayFromExile {
                duration:
                    Duration::UntilNextStepOf {
                        step: Phase::End,
                        player: PlayerScope::Controller,
                    },
                granted_to,
                exiled_by_ability_controller,
                ..
            } => exiled_by_ability_controller.unwrap_or(*granted_to) != active_player,
            // CR 513.1: durational `ExileWithAltCost` with
            // `UntilNextStepOf { End, Controller }` granted to the active
            // player expires at their end step (mirrors PlayFromExile).
            CastingPermission::ExileWithAltCost {
                duration:
                    Some(Duration::UntilNextStepOf {
                        step: Phase::End,
                        player: PlayerScope::Controller,
                    }),
                granted_to: Some(g),
                ..
            } => *g != active_player,
            CastingPermission::PlayFromExile { .. }
            | CastingPermission::AdventureCreature
            | CastingPermission::ExileWithAltCost { .. }
            | CastingPermission::ExileWithAltAbilityCost { .. }
            | CastingPermission::ExileWithEnergyCost
            | CastingPermission::WarpExile { .. }
            | CastingPermission::Plotted { .. }
            | CastingPermission::Foretold { .. } => true,
        });
    }
}

/// Remove transient `UntilNextTurnOf { Controller }` effects whose controller's
/// turn is starting. Called at the start of the active player's turn (untap step)
/// per CR 514.2.
///
/// Also clears `goaded_by` entries for the active player on all battlefield objects,
/// per CR 701.15a: goad expires at the beginning of the goading player's next turn.
pub fn prune_until_next_turn_effects(state: &mut GameState, active_player: PlayerId) {
    // CR 514.2: "until the end of your next turn" effects are *armed* when the
    // controller's next turn begins — convert them to `UntilEndOfTurn` so the
    // cleanup-step prune (`prune_end_of_turn_effects`) ends them at THIS turn's
    // cleanup, persisting through the whole turn. They survive the creation
    // turn's own cleanup because that turn's untap step already passed before
    // the effect was created, so this is the controller's *next* turn.
    for e in state.transient_continuous_effects.iter_mut() {
        if matches!(
            e.duration,
            Duration::UntilEndOfNextTurnOf {
                player: PlayerScope::Controller
            }
        ) && e.controller == active_player
        {
            e.duration = Duration::UntilEndOfTurn;
        }
    }

    let before = state.transient_continuous_effects.len();
    state.transient_continuous_effects.retain(|e| {
        !(matches!(
            e.duration,
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller
            }
        ) && e.controller == active_player)
    });
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty.mark_full();
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
        if !matches!(
            e.duration,
            Duration::UntilNextStepOf {
                step: Phase::Untap,
                player: PlayerScope::Controller
            }
        ) {
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
        state.layers_dirty.mark_full();
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
        state.layers_dirty.mark_full();
    }
}

/// Remove transient effects bound to a specific affected object that has left the battlefield.
pub fn prune_affected_object_left_effects(state: &mut GameState, departed_id: ObjectId) {
    let before = state.transient_continuous_effects.len();
    state.transient_continuous_effects.retain(|effect| {
        !matches!(effect.affected, TargetFilter::SpecificObject { id } if id == departed_id)
    });
    if state.transient_continuous_effects.len() != before {
        state.layers_dirty.mark_full();
    }
}

/// CR 611.2b + CR 613.1b: a control swap of the captured source ends the
/// can't-untap continuous effect permanently; remove the gated def from
/// base+live so a later swap-back cannot revive it. Runs inside
/// `evaluate_layers` after the Layer-2 control board is finalized.
///
/// Read/mutate split: the read pass borrows `&state` to consult the shared gate
/// (`controller_controls_source_gate`) for every `ControllerControlsSource` def
/// it finds (live OR base); the mutate pass uses only the owned host-id list and
/// lapsed `(source, controller)` set, so no `&state` borrow is alive while
/// mutating `state.objects`.
pub(crate) fn prune_lapsed_controller_controls_source(state: &mut GameState) {
    use crate::types::ability::ReplacementCondition;

    let mut lapsed: HashSet<(ObjectId, PlayerId)> = HashSet::new();
    let mut hosts: Vec<ObjectId> = Vec::new();

    // READ pass.
    for (host_id, obj) in state.objects.iter() {
        let mut host_has_lapsed = false;
        let mut scan = |def: &crate::types::ability::ReplacementDefinition| {
            if let Some(ReplacementCondition::ControllerControlsSource { source, controller }) =
                def.condition
            {
                if !crate::game::replacement::controller_controls_source_gate(
                    state, source, controller,
                ) {
                    lapsed.insert((source, controller));
                    host_has_lapsed = true;
                }
            }
        };
        obj.replacement_definitions.iter_all().for_each(&mut scan);
        obj.base_replacement_definitions.iter().for_each(&mut scan);
        if host_has_lapsed {
            hosts.push(*host_id);
        }
    }

    if hosts.is_empty() {
        return;
    }

    // MUTATE pass — owned data only.
    let is_lapsed = |def: &crate::types::ability::ReplacementDefinition| {
        matches!(
            def.condition,
            Some(ReplacementCondition::ControllerControlsSource { source, controller })
                if lapsed.contains(&(source, controller))
        )
    };
    for host_id in hosts {
        if let Some(obj) = state.objects.get_mut(&host_id) {
            obj.replacement_definitions.retain(|d| !is_lapsed(d));
            std::sync::Arc::make_mut(&mut obj.base_replacement_definitions)
                .retain(|d| !is_lapsed(d));
        }
    }
}

/// CR 611.2b + CR 400.7: the captured source leaving play, OR the host leaving
/// and re-entering as a new object (same storage ObjectId), ends the can't-untap
/// continuous effect permanently — drop the gated def from base+live so it
/// cannot revive on a same-ObjectId re-entry. Called from `zones.rs` on a
/// battlefield exit, OUTSIDE `evaluate_layers`, so it marks layers full on change
/// (mirroring `prune_host_left_effects`).
///
/// The predicate is purely id-based (`departed_id`), so no `&state` gate read is
/// needed — each object is mutated in a single pass: case (b) the captured
/// `source == departed_id`, OR case (c) the holding object's own id ==
/// `departed_id` (host left and may re-enter as a new CR 400.7 object reusing the
/// same storage key).
pub(crate) fn prune_controller_controls_source_on_leave(
    state: &mut GameState,
    departed_id: ObjectId,
) {
    use crate::types::ability::ReplacementCondition;

    let mut changed = false;
    for (host_id, obj) in state.objects.iter_mut() {
        let host_left = *host_id == departed_id;
        let is_lapsed = |def: &crate::types::ability::ReplacementDefinition| {
            host_left
                || matches!(
                    def.condition,
                    Some(ReplacementCondition::ControllerControlsSource { source, .. })
                        if source == departed_id
                )
        };
        // Only `ControllerControlsSource` defs are eligible to be dropped — the
        // `host_left` arm must not wipe unrelated riders, so gate on the variant.
        let drop = |def: &crate::types::ability::ReplacementDefinition| {
            matches!(
                def.condition,
                Some(ReplacementCondition::ControllerControlsSource { .. })
            ) && is_lapsed(def)
        };
        let before_live = obj.replacement_definitions.len();
        obj.replacement_definitions.retain(|d| !drop(d));
        let before_base = obj.base_replacement_definitions.len();
        if obj.base_replacement_definitions.iter().any(drop) {
            std::sync::Arc::make_mut(&mut obj.base_replacement_definitions).retain(|d| !drop(d));
        }
        if obj.replacement_definitions.len() != before_live
            || obj.base_replacement_definitions.len() != before_base
        {
            changed = true;
        }
    }
    if changed {
        state.layers_dirty.mark_full();
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
    evaluate_condition_with_context(state, condition, controller, source_id, None)
}

pub(crate) fn evaluate_condition_with_recipient(
    state: &GameState,
    condition: &StaticCondition,
    controller: PlayerId,
    source_id: ObjectId,
    recipient_id: ObjectId,
) -> bool {
    evaluate_condition_with_context(state, condition, controller, source_id, Some(recipient_id))
}

fn condition_uses_recipient_context(condition: &StaticCondition) -> bool {
    match condition {
        StaticCondition::IsPresent {
            filter: Some(filter),
        }
        | StaticCondition::DefendingPlayerControls { filter }
        | StaticCondition::SourceMatchesFilter { filter } => filter_uses_recipient(filter),
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            quantity_expr_uses_recipient(lhs) || quantity_expr_uses_recipient(rhs)
        }
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            conditions.iter().any(condition_uses_recipient_context)
        }
        StaticCondition::Not { condition } => condition_uses_recipient_context(condition),
        StaticCondition::RecipientHasCounters { .. } => true,
        StaticCondition::RecipientMatchesFilter { .. } => true,
        // CR 509.1b + CR 506.2: the attacking creature (recipient) is the subject
        // of the owner-attack check, so this MUST route through the recipient-eval
        // path. The `Not` wrapper above already recurses, so the positive inner
        // condition is what must report `true`. This match carries a `_ => false`
        // wildcard, so the compiler will NOT flag an omission here — it is
        // functionally required: without it `recipient_id` arrives `None`, the
        // evaluator returns `false`, and the `Not` inverts it to "always blockable".
        StaticCondition::RecipientAttackingOwnerTarget { .. } => true,
        // CR 110.5b + CR 611.2b: a target/recipient-scoped tap condition must
        // route through the recipient-eval path so the captured `duration_subject`
        // (the copy target) binds — relying on the `_ => false` default would
        // route `Target` to the source binding and reintroduce the bug.
        // `Source`-scoped (never emitted; spelled `SourceIsTapped`) stays source-bound.
        StaticCondition::IsTapped { scope } => matches!(
            scope,
            crate::types::ability::ObjectScope::Target
                | crate::types::ability::ObjectScope::Recipient
        ),
        _ => false,
    }
}

/// True when a static ability's SOURCE-LEVEL enabling condition can be flipped
/// by an object entering or leaving the battlefield — i.e. its truth value
/// depends on battlefield population/composition. Such conditions gate the
/// effect for the WHOLE recipient set (they are NOT recipient-local; see
/// `condition_uses_recipient_context`), so when an entry flips the gate every
/// pre-existing recipient changes. The incremental layer-flush fast path only
/// re-derives the entered objects, leaving pre-existing recipients with stale
/// derived state, so it must escalate to a full re-evaluation in that case.
///
/// CR 611.3a: a static ability's continuous effect isn't locked in — when its
/// source-level enabling condition depends on board population, an object
/// entering can flip the condition for the whole recipient set, changing
/// PRE-EXISTING recipients. Escalate to a full rebuild.
///
/// EXHAUSTIVE, wildcard-free over `StaticCondition` so a future variant forces a
/// compile-time classification. Threshold/comparison variants recurse their
/// operand `QuantityExpr`s through the existing `quantity_expr_uses_object_count`
/// (DRY — no hand-rolled population detection). When in doubt, conservatively
/// `true`: over-escalation is merely a perf cost, under-escalation is a
/// rules-correctness bug.
fn static_condition_uses_object_population(condition: &StaticCondition) -> bool {
    match condition {
        // Threshold/comparison gates: board-population-dependent iff an operand
        // reads object count. Reuses the shared QuantityExpr classifier.
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            crate::game::quantity::quantity_expr_uses_object_count(lhs)
                || crate::game::quantity::quantity_expr_uses_object_count(rhs)
        }
        // Devotion is a sum of mana symbols across permanents you control — pure
        // board composition.
        StaticCondition::DevotionGE { .. } => true,
        // "you control [filter]" / "a [filter] is on the battlefield" — membership
        // is battlefield population. `IsPresent` has no zone field (always a
        // battlefield-presence check), so it is unconditionally population
        // dependent regardless of whether `filter` is `Some`/`None`.
        StaticCondition::IsPresent { .. } => true,
        // Per-player board-count gate (defending player controls a [filter]).
        StaticCondition::DefendingPlayerControls { .. } => true,
        // "you control a/your commander" — membership of a commander on the
        // battlefield, board-population dependent.
        StaticCondition::ControlsCommander { .. } => true,
        // Recurse combinators.
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => conditions
            .iter()
            .any(static_condition_uses_object_population),
        StaticCondition::Not { condition } => static_condition_uses_object_population(condition),
        // Parse fallback — evaluated permissively (always true today), but its
        // text is unknown; conservatively escalate so an unrecognized
        // population-gated condition can never silently under-escalate.
        StaticCondition::Unrecognized { .. } => true,
        // Genuinely board-population-independent: source-local state, the
        // source's chosen attributes, combat status, player-scoped flags/totals,
        // turn/phase, recipient-context gates (handled by
        // `condition_uses_recipient_context`), zone presence of the source, and
        // cast-history. Enumerated explicitly (no wildcard) so a future variant
        // is forced through this classification.
        StaticCondition::ChosenColorIs { .. }
        | StaticCondition::ChosenLabelIs { .. }
        | StaticCondition::HasMaxSpeed
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::DayNightIs { .. }
        | StaticCondition::HasCounters { .. }
        | StaticCondition::CastVariantPaid { .. }
        | StaticCondition::RecipientHasCounters { .. }
        | StaticCondition::ClassLevelGE { .. }
        | StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsAttacking
        // CR 509.1b: combat-status gate on the recipient's attack target — per-object
        // combat state, never board-population-dependent (like `SourceIsAttacking`).
        | StaticCondition::RecipientAttackingOwnerTarget { .. }
        | StaticCondition::SourceIsBlocking
        | StaticCondition::SourceIsBlocked
        | StaticCondition::IsMonarch
        | StaticCondition::IsInitiative
        | StaticCondition::NoMonarch
        | StaticCondition::HasCityBlessing
        | StaticCondition::CompletedADungeon
        | StaticCondition::WasStartingPlayer { .. }
        | StaticCondition::SpellCastWithVariantThisTurn { .. }
        | StaticCondition::OpponentPoisonAtLeast { .. }
        | StaticCondition::UnlessPay { .. }
        | StaticCondition::DuringYourTurn
        | StaticCondition::SourceEnteredThisTurn
        | StaticCondition::SourceHasDealtDamage
        | StaticCondition::WasCast { .. }
        | StaticCondition::IsRingBearer
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::SourceIsTapped
        // Tap status is per-object state, never board-population-dependent —
        // non-population exactly like `SourceIsTapped`.
        | StaticCondition::IsTapped { .. }
        | StaticCondition::SourceIsSaddled
        | StaticCondition::SourceControllerEquals { .. }
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsEnchanted
        | StaticCondition::SourceIsMonstrous
        | StaticCondition::SourceIsHarnessed
        | StaticCondition::SourceAttachedToCreature
        | StaticCondition::SourceMatchesFilter { .. }
        | StaticCondition::RecipientMatchesFilter { .. }
        | StaticCondition::SourceIsPaired
        | StaticCondition::SourceInZone { .. }
        | StaticCondition::EnchantedIsFaceDown
        | StaticCondition::AdditionalCostPaid
        | StaticCondition::CastingAsVariant { .. }
        | StaticCondition::None => false,
    }
}

/// CR 611.3a: ENTRY-AWARE narrowing for a population-sensitive source-level
/// enabling CONDITION. `static_condition_uses_object_population` proves a
/// condition *can* gate on board population; this proves a SPECIFIC entering
/// object can actually perturb that population input (so the gate might flip for
/// the whole recipient set).
///
/// Monotonicity: reached only for battlefield ENTRIES. An entry only ADDS
/// objects, so a count/devotion gate only flips by the entered object joining
/// the population, and `IsPresent` only flips false→true via a matching member.
/// `ctx` is built from the condition's SOURCE object (CR 109.5 controller
/// rebinding) by the caller.
///
/// EXHAUSTIVE and wildcard-free, mirroring `static_condition_uses_object_population`:
/// every `false` arm there is `false` here; every `true` arm there is narrowed
/// to a membership / threshold-perturb test, with conservative `true` where a
/// precise membership test is awkward (over-escalation is safe).
fn entered_object_perturbs_static_condition(
    state: &GameState,
    entered_id: ObjectId,
    ctx: &FilterContext<'_>,
    condition: &StaticCondition,
) -> bool {
    match condition {
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            entered_perturbs_static_quantity(state, entered_id, ctx, lhs)
                || entered_perturbs_static_quantity(state, entered_id, ctx, rhs)
        }
        // CR 700.5: devotion gate flips only if the entered object's mana cost
        // contributes a symbol for one of the gate colors (mirrors the Devotion
        // magnitude leaf). LOW-1: controller-blind.
        StaticCondition::DevotionGE { colors, .. } => {
            state.objects.get(&entered_id).is_some_and(|entered| {
                crate::game::quantity::entered_object_perturbs_quantity_expr(
                    state,
                    entered,
                    ctx,
                    &QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::Devotion {
                            colors: crate::types::ability::DevotionColors::Fixed(colors.clone()),
                        },
                    },
                )
            })
        }
        // "you control [filter]" / "a [filter] is on the battlefield". A present
        // filter flips only via a matching member; an absent filter is an
        // unqualified presence check — conservatively perturb on any entry.
        StaticCondition::IsPresent { filter } => match filter {
            Some(f) => matches_target_filter(state, entered_id, f, ctx),
            None => true,
        },
        // CR 509.1b: defending-player board-count gate — flips via a matching
        // member entering. (ctx controller is the source's, not the defender's;
        // membership over-approximates, which is safe.)
        StaticCondition::DefendingPlayerControls { filter } => {
            matches_target_filter(state, entered_id, filter, ctx)
        }
        // Commander presence — conservatively perturb (a commander entering can
        // flip it; not worth a precise commander membership test here).
        StaticCondition::ControlsCommander { .. } => true,
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => conditions
            .iter()
            .any(|c| entered_object_perturbs_static_condition(state, entered_id, ctx, c)),
        StaticCondition::Not { condition } => {
            entered_object_perturbs_static_condition(state, entered_id, ctx, condition)
        }
        // Unknown text — conservatively perturb so an unrecognized population gate
        // can never silently under-escalate.
        StaticCondition::Unrecognized { .. } => true,
        // Identical enumeration to the `false` arm of
        // `static_condition_uses_object_population`: source-local, chosen-
        // attribute, combat, player-scoped, turn/phase, recipient-context, source
        // zone, and cast-history gates — none read board population, so an entry
        // cannot perturb them.
        StaticCondition::ChosenColorIs { .. }
        | StaticCondition::ChosenLabelIs { .. }
        | StaticCondition::HasMaxSpeed
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::DayNightIs { .. }
        | StaticCondition::HasCounters { .. }
        | StaticCondition::CastVariantPaid { .. }
        | StaticCondition::RecipientHasCounters { .. }
        | StaticCondition::ClassLevelGE { .. }
        | StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsAttacking
        // CR 509.1b: an entering object cannot perturb a per-object combat-status
        // gate on the recipient's attack target — `false` like `SourceIsAttacking`.
        | StaticCondition::RecipientAttackingOwnerTarget { .. }
        | StaticCondition::SourceIsBlocking
        | StaticCondition::SourceIsBlocked
        | StaticCondition::IsMonarch
        | StaticCondition::IsInitiative
        | StaticCondition::NoMonarch
        | StaticCondition::HasCityBlessing
        | StaticCondition::CompletedADungeon
        | StaticCondition::WasStartingPlayer { .. }
        | StaticCondition::SpellCastWithVariantThisTurn { .. }
        | StaticCondition::OpponentPoisonAtLeast { .. }
        | StaticCondition::UnlessPay { .. }
        | StaticCondition::DuringYourTurn
        | StaticCondition::SourceEnteredThisTurn
        | StaticCondition::SourceHasDealtDamage
        | StaticCondition::WasCast { .. }
        | StaticCondition::IsRingBearer
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::SourceIsTapped
        // An entering object cannot perturb a per-object tap gate — `false`
        // exactly like `SourceIsTapped`.
        | StaticCondition::IsTapped { .. }
        | StaticCondition::SourceIsSaddled
        | StaticCondition::SourceControllerEquals { .. }
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsEnchanted
        | StaticCondition::SourceIsMonstrous
        | StaticCondition::SourceIsHarnessed
        | StaticCondition::SourceAttachedToCreature
        | StaticCondition::SourceMatchesFilter { .. }
        | StaticCondition::RecipientMatchesFilter { .. }
        | StaticCondition::SourceIsPaired
        | StaticCondition::SourceInZone { .. }
        | StaticCondition::EnchantedIsFaceDown
        | StaticCondition::AdditionalCostPaid
        | StaticCondition::CastingAsVariant { .. }
        | StaticCondition::None => false,
    }
}

/// Bridge: route a condition operand `QuantityExpr` through the quantity
/// module's entry-aware classifier (resolving the entered object).
fn entered_perturbs_static_quantity(
    state: &GameState,
    entered_id: ObjectId,
    ctx: &FilterContext<'_>,
    expr: &QuantityExpr,
) -> bool {
    state.objects.get(&entered_id).is_some_and(|entered| {
        crate::game::quantity::entered_object_perturbs_quantity_expr(state, entered, ctx, expr)
    })
}

fn source_condition_gate_passes(
    state: &GameState,
    condition: &StaticCondition,
    controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    if !condition_uses_recipient_context(condition) {
        return evaluate_condition(state, condition, controller, source_id);
    }

    match condition {
        StaticCondition::And { conditions } => conditions
            .iter()
            .all(|condition| source_condition_gate_passes(state, condition, controller, source_id)),
        StaticCondition::Not { condition } if !condition_uses_recipient_context(condition) => {
            !evaluate_condition(state, condition, controller, source_id)
        }
        _ => true,
    }
}

fn evaluate_condition_with_context(
    state: &GameState,
    condition: &StaticCondition,
    controller: PlayerId,
    source_id: ObjectId,
    recipient_id: Option<ObjectId>,
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
        // CR 614.12c + CR 607.2d: An anchor-word linked static ability is
        // active iff the source permanent's persisted `ChosenAttribute::Label`
        // matches the anchor word. The comparison is case-insensitive so a
        // capitalised anchor word ("Jeskai") matches a label persisted in
        // any canonicalisation. Mirrors `ChosenColorIs`'s lookup pattern.
        StaticCondition::ChosenLabelIs { label } => eval_chosen_label_is(state, source_id, label),
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => {
            let resolve = |expr: &QuantityExpr| -> i32 {
                crate::game::quantity::resolve_quantity_with_ctx(
                    state,
                    expr,
                    controller,
                    QuantityContext {
                        entering: None,
                        source: source_id,
                        recipient: recipient_id,
                        scoped_player: None,
                    },
                )
            };
            comparator.evaluate(resolve(lhs), resolve(rhs))
        }
        StaticCondition::HasMaxSpeed => has_max_speed(state, controller),
        StaticCondition::SpeedGE { threshold } => effective_speed(state, controller) >= *threshold,
        StaticCondition::And { conditions } => conditions.iter().all(|c| {
            evaluate_condition_with_context(state, c, controller, source_id, recipient_id)
        }),
        StaticCondition::Or { conditions } => conditions.iter().any(|c| {
            evaluate_condition_with_context(state, c, controller, source_id, recipient_id)
        }),
        StaticCondition::Not { condition } => {
            !evaluate_condition_with_context(state, condition, controller, source_id, recipient_id)
        }
        // CR 731.1: True when the game has the requested day/night designation.
        StaticCondition::DayNightIs {
            state: DayNight::Day,
        } => state.day_night == Some(DayNight::Day),
        StaticCondition::DayNightIs {
            state: DayNight::Night,
        } => state.day_night == Some(DayNight::Night),
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
            .map(|obj| counter_condition_matches(obj, counters, *minimum, *maximum))
            .unwrap_or(false),
        // CR 702.176a + CR 611.3a: Persistent alternative-cost marker on the
        // source permanent. This is intentionally not turn-scoped.
        StaticCondition::CastVariantPaid { variant } => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.cast_variant_paid.is_some_and(|(v, _)| v == *variant)),
        StaticCondition::RecipientHasCounters {
            counters,
            minimum,
            maximum,
        } => recipient_id
            .and_then(|id| state.objects.get(&id))
            .map(|obj| counter_condition_matches(obj, counters, *minimum, *maximum))
            .unwrap_or(false),
        // CR 611.3a: True when the recipient (effective subject) of the continuous
        // effect matches `filter`. The anaphoric "it" binds to the per-recipient
        // object being modified this layer cycle; tests THIS recipient against the
        // type/subtype/color filter (not mere existence of some matching object).
        // No recipient → false (mirrors the RecipientHasCounters defensive default).
        StaticCondition::RecipientMatchesFilter { filter } => recipient_id
            .map(|id| {
                matches_target_filter(
                    state,
                    id,
                    filter,
                    &FilterContext::from_source_with_recipient(state, source_id, id),
                )
            })
            .unwrap_or(false),
        // CR 509.1b + CR 506.2 + CR 108.3: the recipient (the attacking creature
        // this static gates) is attacking its owner / a permanent its owner
        // controls. Owner-relative (CR 108.3); no recipient → false (mirrors the
        // RecipientMatchesFilter defensive default).
        StaticCondition::RecipientAttackingOwnerTarget { target } => recipient_id
            .map(|id| eval_recipient_attacking_owner_target(state, id, target))
            .unwrap_or(false),
        // CR 716.2a + CR 716.3: Level abilities are active at or above the specified
        // level. No battlefield zone guard here — the functioning-abilities machinery
        // already constrains source availability before this static is evaluated.
        StaticCondition::ClassLevelGE { level } => eval_class_level_ge(state, source_id, *level),
        StaticCondition::Unrecognized { .. } => true,
        // CR 102.1: "during your turn" means the turn of the static's source
        // controller — never the caster. Most call sites pass the source's
        // controller as `controller`, but the cost-modification resolver passes
        // the *caster* (so caster-relative conditions like SpellsCastThisTurn
        // resolve correctly), which would invert this turn check for an
        // opponent-affecting tax (Tithe Taker: "During your turn, spells your
        // opponents cast cost {1} more"). Bind to the source permanent's
        // controller directly so the gate is correct in every call path; fall
        // back to `controller` only when the source object is absent.
        StaticCondition::DuringYourTurn => {
            let source_controller = state
                .objects
                .get(&source_id)
                .map(|obj| obj.controller)
                .unwrap_or(controller);
            state.active_player == source_controller
        }
        // CR 103.1: True when the scoped player took the first turn of the
        // game (fixed at game start). The parser emits `ControllerRef::You`.
        StaticCondition::WasStartingPlayer { .. } => state.current_starting_player == controller,
        // CR 702.185c: True when any player cast a spell using `variant` (e.g.
        // Warp) this turn. Not controller-scoped.
        StaticCondition::SpellCastWithVariantThisTurn { variant } => {
            crate::game::restrictions::spell_cast_with_variant_this_turn(state, variant)
        }
        // CR 400.7: True when the source permanent entered the battlefield this turn.
        StaticCondition::SourceEnteredThisTurn => eval_source_entered_this_turn(state, source_id),
        // CR 120.3 + CR 120.6 + CR 702.11b: True once the source has actually dealt
        // damage since entering the battlefield (sticky). The "hasn't dealt damage
        // yet" hexproof grant wraps this in `StaticCondition::Not`.
        StaticCondition::SourceHasDealtDamage => eval_source_has_dealt_damage(state, source_id),
        // CR 601.2 + CR 611.3a: True when the source permanent was cast.
        StaticCondition::WasCast { zone } => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.cast_from_zone)
            .is_some_and(|cz| zone.is_none_or(|z| cz == z)),
        // CR 701.54a: True when this creature is the ring-bearer for its controller.
        StaticCondition::IsRingBearer => {
            super::effects::ring::is_current_ring_bearer(state, controller, source_id)
        }
        // CR 701.54c: True when the controller's ring level is at least this value.
        StaticCondition::RingLevelAtLeast { level } => {
            state.ring_level.get(&controller).copied().unwrap_or(0) >= *level
        }
        // CR 611.2b + CR 110.5d: require battlefield — cards not on the battlefield are
        // neither tapped nor untapped. A source that has left the battlefield (e.g.
        // Callous Oppressor dying while tapped) fails this predicate and any
        // `ForAsLongAs { SourceIsTapped }` continuous effect (gain-control, etc.) ends.
        StaticCondition::SourceIsTapped => eval_source_is_tapped_on_battlefield(state, source_id),
        // CR 110.5b + CR 110.5d: scope-parameterized tap check (the non-source
        // sibling of `SourceIsTapped`). Resolve the scope to a concrete object,
        // then reuse the same zone-guarded battlefield tap predicate. The parser
        // only ever emits `scope: Target` (the demonstrative "that creature
        // remains tapped" case — Zygon Infiltrator), bound at resolution time to
        // the copy target via `duration_subject` and surfaced here as the
        // `recipient_id`. `Recipient` resolves identically. `Source` is spelled
        // `SourceIsTapped` and never reaches this arm; the remaining scopes are
        // never produced for a duration tap condition, so they fail safely.
        StaticCondition::IsTapped { scope } => match scope {
            crate::types::ability::ObjectScope::Source => {
                eval_source_is_tapped_on_battlefield(state, source_id)
            }
            crate::types::ability::ObjectScope::Target
            | crate::types::ability::ObjectScope::Recipient => {
                recipient_id.is_some_and(|id| eval_source_is_tapped_on_battlefield(state, id))
            }
            crate::types::ability::ObjectScope::EventSource
            | crate::types::ability::ObjectScope::EventTarget
            | crate::types::ability::ObjectScope::CostPaidObject
            | crate::types::ability::ObjectScope::Anaphoric
            | crate::types::ability::ObjectScope::Demonstrative => false,
        },
        // CR 702.171b + CR 110.5d: off-battlefield permanents have no saddled designation.
        StaticCondition::SourceIsSaddled => state.objects.get(&source_id).is_some_and(|obj| {
            obj.zone == crate::types::zones::Zone::Battlefield && obj.is_saddled
        }),
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
        // CR 303.4: True when at least one Aura is attached to the source object.
        // Aura-twin of `SourceIsEquipped` (CR 301.5a); the only delta is the
        // subtype check ("Aura" vs "Equipment"). An Aura attaches to objects or
        // players (CR 303.4), but a creature host is always an Object, so
        // non-Object hosts are rejected by `as_object`.
        StaticCondition::SourceIsEnchanted => state.objects.values().any(|obj| {
            obj.attached_to.and_then(|t| t.as_object()) == Some(source_id)
                && obj.card_types.subtypes.iter().any(|s| s == "Aura")
        }),
        // CR 701.37: True when the source permanent is monstrous.
        StaticCondition::SourceIsMonstrous => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.monstrous),
        // CR 701.64b + CR 702.186b: True when the source permanent is harnessed.
        // The ∞ (Infinity) ability gate reads `GameObject::harnessed`.
        StaticCondition::SourceIsHarnessed => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.harnessed),
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
        StaticCondition::SourceInZone { zone } => eval_source_in_zone(state, source_id, *zone),
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
        StaticCondition::SourceIsPaired => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.paired_with.is_some()),
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
        StaticCondition::SourceIsAttacking => eval_source_is_attacking(state, source_id),
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
        StaticCondition::IsMonarch => eval_is_monarch(state, controller),
        // CR 726.3: True when the controller has the initiative.
        StaticCondition::IsInitiative => eval_is_initiative(state, controller),
        // CR 725.1: True when no player holds the monarch designation.
        StaticCondition::NoMonarch => eval_no_monarch(state),
        // CR 702.131a: True when the controller has the city's blessing.
        StaticCondition::HasCityBlessing => eval_has_city_blessing(state, controller),
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
        // CR 702.166a: True when an optional additional cost (Bargain) was paid for the
        // spell being cast. `source_id` is the spell whose self-spell `ReduceCost` static
        // is being evaluated; read the in-flight cast's `additional_cost_paid` flag.
        StaticCondition::AdditionalCostPaid => state
            .pending_cast
            .as_ref()
            .filter(|pc| pc.object_id == source_id)
            .map(|pc| pc.ability.context.additional_cost_paid)
            .unwrap_or(false),
        // CR 702.34a: Cast-time variant gates are evaluated in
        // `collect_self_spell_cost_modifiers`, not the layer pipeline.
        StaticCondition::CastingAsVariant { .. } => false,
        StaticCondition::None => true,
        // CR 309.7: True when the controller has completed at least one dungeon.
        StaticCondition::CompletedADungeon => state
            .dungeon_progress
            .get(&controller)
            .is_some_and(|p| !p.completed.is_empty()),
        // CR 903.3 / CR 903.3d: Lieutenant ("your commander") requires ownership;
        // generic ("a commander") is controller-only.
        StaticCondition::ControlsCommander { ownership } => match ownership {
            CommanderOwnership::Own => {
                crate::game::commander::controls_own_commander(state, controller)
            }
            CommanderOwnership::Any => {
                crate::game::commander::controls_any_commander(state, controller)
            }
        },
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
/// Test-only counter incremented at the TOP of every FULL `evaluate_layers`
/// pass (NOT incremented by `apply_layers_incremental`). The discriminating
/// performance test reads this to prove the incremental fast path engaged: full
/// evaluations must be near-constant, not proportional to the resolved stack.
#[cfg(test)]
pub(crate) static FULL_EVALUATE_LAYERS_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

// Test-only placement toggle for the `StaticSourceIndex` rebuild, used to prove
// the discriminating regression test goes RED on the (buggy) end-of-pass
// placement and GREEN on the (correct) top-of-pass placement. Production code
// ALWAYS rebuilds at the top of the pass (this toggle does not exist outside
// `cfg(test)`). When `false`, the rebuild is deferred to the END of
// `evaluate_layers` / `apply_layers_incremental` — which leaves the mid-pass
// gathers reading the previous pass's stale index, exactly the GAP-1 bug.
//
// THREAD-LOCAL (not process-global): engine layer resolution is synchronous, so
// the production code invoked by a test runs on that test's own thread. A
// thread-local toggle lets the RED discriminating test flip the placement on its
// OWN thread only — concurrently-scheduled tests (the GREEN counterpart and
// every other parallel test) read their own default `true` and are unaffected.
// A process-global `AtomicBool` here raced under cargo's default parallel runner.
#[cfg(test)]
thread_local! {
    pub(crate) static REBUILD_STATIC_INDEX_AT_TOP: core::cell::Cell<bool> =
        const { core::cell::Cell::new(true) };
}

/// Whether to rebuild the static-source index at the TOP of the pass. Always
/// `true` in production; togglable only under `cfg(test)` for red→green
/// discrimination.
#[cfg(test)]
#[inline]
fn rebuild_static_index_at_top() -> bool {
    REBUILD_STATIC_INDEX_AT_TOP.with(core::cell::Cell::get)
}

/// Production variant: the rebuild is ALWAYS at the top of the pass.
#[cfg(not(test))]
#[inline]
fn rebuild_static_index_at_top() -> bool {
    true
}

/// CR 701.60c: A suspected permanent has menace and "This creature can't block"
/// for as long as it's suspected. The suspected designation (`is_suspected`,
/// CR 701.60b) is the source of truth; the menace + "can't block" abilities are
/// a continuous effect *derived* from it, not stored on the permanent's copiable
/// values. Re-deriving them here — directly onto the just-reset live `keywords`
/// / `static_definitions` (NOT `base_*`) every layers pass — keeps them present
/// exactly while the designation holds and leaves the permanent's printed
/// abilities untouched: clearing the designation (Effect::Unsuspect) simply
/// stops re-deriving them, and a naturally-menace creature keeps its printed
/// menace when it stops being suspected.
///
/// Called from the Step-1 reset of both the full (`evaluate_layers`) and
/// incremental (`apply_layers_incremental`) passes, immediately after the live
/// fields are reset to base, so the derived grant rides along with every reset.
fn derive_suspected_abilities(obj: &mut crate::game::game_object::GameObject) {
    if !obj.is_suspected {
        return;
    }
    // CR 701.60c: menace. Skip if the permanent already has it (printed or
    // otherwise granted) so the derivation is idempotent and a later removal of
    // the printed copy still leaves exactly the derived one.
    if !obj.keywords.iter().any(|k| matches!(k, Keyword::Menace)) {
        obj.keywords.push(Keyword::Menace);
    }
    // CR 701.60c: "This creature can't block." Skip if a `CantBlock` static is
    // already present (printed or otherwise granted).
    if !obj
        .static_definitions
        .iter_all()
        .any(|s| s.mode == StaticMode::CantBlock)
    {
        obj.static_definitions
            .push(StaticDefinition::new(StaticMode::CantBlock));
    }
}

/// Unconditional full layer evaluation (CR 613.1).
///
/// Production code must NOT call this directly — go through [`flush_layers`],
/// which consumes the `LayersDirty` lattice and keeps the public-state dirty
/// marks consistent with what was recomputed. To force a full pass, call
/// `mark_layers_full` then `flush_layers`. Direct calls are reserved for
/// tests that deliberately force a full evaluation regardless of dirty state.
pub fn evaluate_layers(state: &mut GameState) {
    #[cfg(test)]
    FULL_EVALUATE_LAYERS_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // CR 302.6 + CR 613.1b + CR 702.26b: Snapshot effective controllers for
    // phased-in permanents BEFORE the Step 1 reset below wipes them. The
    // post-pass diff at the end of this function compares against this
    // snapshot to detect effective-controller transitions (Layer 2 control-
    // changing effect start/end, exchange-control, gain-control expiry) and
    // re-applies summoning sickness per CR 302.6 ("continuously under that
    // player's control since that player's most recent turn began").
    // Phased-out permanents are excluded per CR 702.26b.
    let prev_controllers: Vec<(ObjectId, PlayerId)> = state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter_map(|id| state.objects.get(&id).map(|o| (id, o.controller)))
        .collect();

    // Step 1: Reset computed characteristics to base values.
    // Only reset fields where base values were explicitly set; objects without
    // base values (e.g., from older test helpers) retain their current values.
    // Attribution is also reset here so each layers pass rebuilds the
    // source-attribution side-table from scratch alongside derived state.
    // `im::HashMap::clear()` drops the cleared map's own root Arc; clones
    // taken by AI search or snapshot diffing retain their own roots, so this
    // does not break structural sharing across `GameState` clones.
    state.attribution.clear();
    let mut abilities_suppressed = HashSet::new();
    // CR 702.26b + CR 702.26e: Phased-out permanents are treated as though
    // they do not exist and are not included in continuous-effect affected
    // sets. Exclude them from the whole layer pass so the reset/apply invariant
    // remains intact; they are frozen until phase-in marks layers dirty and
    // re-includes them.
    let bf_ids: Vec<ObjectId> = state.battlefield_phased_in_ids();
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
            // Reset the display-identity pointer to its baseline; the Copy layer
            // re-applies the copied source's `printed_ref` below for objects
            // under a copy effect, so a temporary copy's art reverts on expiry.
            obj.printed_ref = obj.base_printed_ref.clone();
            // Reset display routing to the object's own derived baseline so a
            // copy effect's override (set by `CopyValues` below) reverts on
            // expiry. Display routing is derived state, not a copiable value
            // (CR 707.2): a true token — created by a token-making effect
            // (CR 111.1), so carrying no printed identity — routes to the token
            // art database; everything else (a real card, or a token-copy *of a
            // real card*, which carries `base_printed_ref`) routes to the card
            // database. Deriving here (rather than storing a `base_display_source`)
            // keeps tokens in pre-existing saved states correct on load.
            obj.display_source = if obj.is_token && obj.base_printed_ref.is_none() {
                DisplaySource::Token
            } else {
                DisplaySource::Card
            };
            // A nontoken never has its own token-art pointer, so clear it to its
            // baseline (`None`); a copy-of-token effect re-applies the source
            // token's `token_image_ref` below while active. A true token keeps
            // its own pointer (its baseline), which the copy layer overrides only
            // while it is copying another object.
            if !obj.is_token {
                obj.token_image_ref = None;
            }
            // CR 613.1b: Reset controller to the object's base controller;
            // Layer 2 re-applies continuous control-changing effects.
            obj.controller = obj.base_controller.unwrap_or(obj.owner);
            // CR 613.11 + CR 510.1a: Reset combat-assignment rule flags;
            // re-applied after object-characteristic layers are complete.
            obj.assigns_damage_from_toughness = false;
            obj.assigns_damage_as_though_unblocked = false;
            obj.assigns_no_combat_damage = false;
            // CR 701.60c: re-derive the suspected designation's menace +
            // "can't block" onto the just-reset live fields (not base), so the
            // grant lasts exactly as long as the designation.
            derive_suspected_abilities(obj);
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

    // CR 611.2 + CR 613.1: Rebuild the static-effect-source index from the
    // just-reset base `static_definitions` so the Copy / main gathers below
    // iterate the current pass's generator set. MUST run AFTER the Step-1 reset
    // (so the predicate reads base, not stale post-layer, definitions) and
    // BEFORE the first gather (so the mid-pass consults are fresh — unlike
    // `TriggerIndex` this index is read INSIDE the pass, so its rebuild is
    // top-of-pass, not end-of-pass). The `rebuild_static_index_at_top` guard is
    // ALWAYS true in production; it is togglable only under `cfg(test)` for the
    // red→green discriminating regression test.
    if rebuild_static_index_at_top() {
        crate::types::game_state::StaticSourceIndex::rebuild_from_state(state);
    }

    // Step 2: Apply copy effects first so copied static abilities exist before later layers.
    let copy_effects = gather_active_effects_for_layer(state, Layer::Copy);
    let ordered_copy = order_active_continuous_effects(Layer::Copy, &copy_effects, state);
    for effect in &ordered_copy {
        apply_continuous_effect(state, effect, &mut abilities_suppressed);
    }
    if crate::game::stickers::apply_battlefield_name_and_ability_stickers(state, &bf_ids) {
        // Sticker ability text is appended after the top-of-pass reset/copy
        // application, so a sticker can turn a non-generator into a continuous
        // static source mid-pass. Refresh the generator index before the main
        // gather so those sticker-granted statics participate in this pass
        // without broadening the non-sticker top-of-pass rebuild contract.
        crate::types::game_state::StaticSourceIndex::rebuild_from_state(state);
    }

    // Step 3: Gather active continuous effects after layer 1 is applied.
    let mut effects_by_layer = gather_active_continuous_effects(state);
    crate::game::stickers::append_battlefield_pt_sticker_effects(state, &mut effects_by_layer);

    // Step 4: Process each remaining layer in order
    for (layer, layer_bucket) in &effects_by_layer {
        if *layer == Layer::Copy {
            // Copy is handled above, before this loop.
            continue;
        }

        if !layer_bucket.is_empty() {
            let layer_effects: Vec<&ActiveContinuousEffect> = layer_bucket.iter().collect();

            let ordered = if layer.has_dependency_ordering() {
                order_with_dependencies(&layer_effects, state)
            } else {
                order_by_timestamp(&layer_effects)
            };

            for effect in &ordered {
                apply_continuous_effect(state, effect, &mut abilities_suppressed);
            }
        }

        // CR 613.1f: "Effects that say an object can't have an ability" are
        // applied in Layer 6 (Ability), together with the keyword grants/removals.
        // Strip denied keywords at the END of Layer 6 — after all grants in this
        // bucket but BEFORE Layer 7 — so a keyword-conditional P/T effect
        // ("creatures with flying get +1/+1") evaluated later observes the denied
        // state (Archetype cycle, Arcane Lighthouse).
        if *layer == Layer::Ability {
            apply_cant_have_keyword_denials(state, None);
        }

        // CR 613.4c: P/T counters modify power/toughness in layer 7c. Counters
        // are object state, not continuous effects, so the `CounterPT` bucket is
        // empty and the fold runs here — after the 7c `+N/+N` effects above and
        // before the 7d `SwitchPT` (CR 613.4d) handled in a later iteration.
        if *layer == Layer::CounterPT {
            apply_pt_counter_modifications(state, bf_ids.iter().copied());
        }

        if *layer == Layer::Type {
            apply_prototype_characteristics(state, bf_ids.iter().copied());
            apply_intrinsic_basic_land_mana_abilities(state, &bf_ids);
        }
        if matches!(*layer, Layer::Control | Layer::Type) {
            super::pairing::cleanup_invalid_pairs(state);
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

    // CR 113.11: "It's also impossible for an effect or keyword counter to add
    // [a denied] ability to the object." The keyword-counter grant above runs
    // after the in-loop Layer 6 denial, so re-apply the denial to strip any
    // counter-granted keyword that a `CantHaveKeyword` static forbids.
    apply_cant_have_keyword_denials(state, None);

    // CR 306.5c: Loyalty is tracked via loyalty counters. After the layer reset
    // reverts obj.loyalty to base_loyalty, re-derive it from the actual counter.
    // (P/T counters are applied in-loop at Layer::CounterPT above, in layer 7c
    // before the 7d switch.)
    //
    // Loyalty is HYBRID: a counter-tracked planeswalker's loyalty IS its counter
    // count (present entry wins, including 0); an un-counter-tracked planeswalker
    // (a clone whose loyalty comes from the Copy layer, an in-place transform, an
    // off-battlefield/pre-seed object) keeps the base/copy-layer value. So the
    // `if let Some` is load-bearing: an ABSENT entry means "not counter-tracked,
    // keep the field", while a PRESENT 0 means "drained to 0, must die" (CR
    // 704.5i). `apply_counter_removal` is what keeps the 0 entry alive for a
    // genuinely-tracked walker so this re-derive can see it.
    for &id in &bf_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            if let Some(&loyalty_counters) = obj.counters.get(&CounterType::Loyalty) {
                obj.loyalty = Some(loyalty_counters);
            }
        }
    }

    // CR 614.12 + CR 604.1 + CR 702.136a: replacement-on-grant (seam 3).
    // A Continuous static that grants an as-enters replacement keyword (Riot)
    // contributes its as-enters `Moved` replacement from the GRANTING permanent,
    // scoped to the static's `affected` filter — not as a SelfRef replacement on
    // the recipient (CR 614.12: a grant to "a general subset of permanents that
    // includes it" comes from the granting source). Build-time `synthesize_riot`
    // installs this once on `face.replacements`; the per-pass reset above
    // (CR 613.1) discards persistent installs, so the replacement must be
    // re-derived each pass onto the source's live `replacement_definitions`. This
    // runs AFTER Layer 6 (Ability) so a Riot-granting static that was itself
    // granted to the source is visible in `static_definitions`. The runtime
    // gather (`find_applicable_replacements`) then applies the affected-filter
    // replacement to matching entering creatures with no further change.
    for &id in &bf_ids {
        let derived: Vec<crate::types::ability::ReplacementDefinition> = {
            let Some(obj) = state.objects.get(&id) else {
                continue;
            };
            obj.static_definitions
                .iter_all()
                .filter_map(|def| {
                    // Cheap discriminator FIRST: `entry_replacement_for_grant_static`
                    // is a couple of field checks returning `None` for ~every
                    // static, whereas `source_condition_gate_passes` can scan all
                    // objects (e.g. `IsPresent`). Run the condition gate only once
                    // an as-enters keyword grant is confirmed, so the layer hot
                    // path pays no board-wide condition tax for non-Riot statics.
                    let replacement =
                        crate::database::synthesis::entry_replacement_for_grant_static(def)?;
                    let active = def.condition.as_ref().is_none_or(|condition| {
                        source_condition_gate_passes(state, condition, obj.controller, id)
                    });
                    active.then_some(replacement)
                })
                .collect()
        };
        if derived.is_empty() {
            continue;
        }
        if let Some(obj) = state.objects.get_mut(&id) {
            for replacement in derived {
                // Idempotent: the per-pass reset already cleared derived
                // replacements, but a static that also appears in the base set
                // (printed Riot grant) could double-install otherwise.
                if !obj
                    .replacement_definitions
                    .iter_all()
                    .any(|r| r == &replacement)
                {
                    obj.replacement_definitions.push(replacement);
                }
            }
        }
    }

    // CR 613.11: Rule-changing continuous effects are applied after object
    // characteristics are determined. These flags feed CR 510.1 combat damage
    // assignment and must observe final post-layer characteristics.
    apply_combat_assignment_rule_effects(state);

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

    super::pairing::cleanup_invalid_pairs(state);
    if super::effects::ring::normalize_ring_bearers(state) {
        evaluate_layers(state);
        return;
    }

    // CR 611.2b + CR 613.1b: a control swap of the captured source ends the
    // can't-untap continuous effect permanently; remove the gated def from
    // base+live so a later swap-back cannot revive it. Runs AFTER the
    // ring-normalization recursion guard (not at the prev_controllers diff loop)
    // so it observes the fully-derived Layer-2 board. No `mark_full` here:
    // `layers_dirty = Clean` is set unconditionally below, so a mark would be
    // dead code — consistency relies on the in-pass live+base mutation.
    prune_lapsed_controller_controls_source(state);

    // CR 611.3a + CR 611.3b: refresh the source-level enabling-condition truth
    // cache from this fully-derived board. Placed AFTER the ring-normalization
    // recursion guard so the re-entrant pass writes the final fixpoint cache
    // once, and BEFORE `layers_dirty = Clean` so a full eval always leaves a
    // fresh cache for the next incremental flush's truth-delta consult.
    refresh_static_gate_truth(state);

    // CR 603.6a + CR 611.2e: Layer evaluation just finalized post-layer
    // trigger sets on every battlefield permanent (granted triggers from
    // sliver lords, Changeling, Bramble Sovereign, suppress-triggers statics).
    // Rebuild the TriggerIndex so the next event scan reads the post-layer
    // trigger set — CR 603.2 requires the post-layer view. Destructive
    // rebuild replaces both `by_key` and `unclassified` from scratch.
    crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);

    // Test-only: the (buggy) end-of-pass placement of the static-source-index
    // rebuild, exercised only when `rebuild_static_index_at_top()` is toggled
    // off. Production always rebuilds at the top (above) and never reaches here.
    if !rebuild_static_index_at_top() {
        crate::types::game_state::StaticSourceIndex::rebuild_from_state(state);
    }

    // Step 5: Clear dirty flag. A full evaluation satisfies any pending request
    // (Clean / EnteredObjects / Full).
    state.layers_dirty = LayersDirty::Clean;
}

/// Mark the layer system as requiring a FULL battlefield re-evaluation. The
/// conservative escalation used by every mutation other than a battlefield entry.
pub fn mark_layers_full(state: &mut GameState) {
    state.layers_dirty.mark_full();
}

/// Record that `id` entered the battlefield and is a candidate for incremental
/// layer re-derivation. If the dirty lattice is already `Full`, this is a no-op
/// (the full pass subsumes the entry).
pub fn mark_layers_entered(state: &mut GameState, id: ObjectId) {
    state.layers_dirty.mark_entered(id);
}

/// Single authority that flushes any pending layer re-evaluation and keeps the
/// public-state dirty marks consistent with what was recomputed.
///
/// CR 613.1: continuous effects are evaluated in layer order over the whole
/// board. The `Full` path always produces a correct board; the `EnteredObjects`
/// path is an O(entered + active-effects) fast path taken only when a
/// per-entered precondition scan AND a board-wide escalation scan prove that
/// re-deriving just the entered objects yields a board identical to a full pass.
pub fn flush_layers(state: &mut GameState) {
    match std::mem::replace(&mut state.layers_dirty, LayersDirty::Clean) {
        LayersDirty::Clean => {}
        LayersDirty::Full => {
            super::perf_counters::record_layers_full_eval();
            evaluate_layers(state);
            super::public_state::mark_public_state_all_dirty(state);
        }
        LayersDirty::EnteredObjects(ids) => {
            if ids.is_empty() {
                return;
            }
            if incremental_flush_must_escalate(state, &ids) {
                super::perf_counters::record_layers_escalated();
                super::perf_counters::record_layers_full_eval();
                evaluate_layers(state);
                super::public_state::mark_public_state_all_dirty(state);
            } else {
                super::perf_counters::record_layers_incremental();
                apply_layers_incremental(state, &ids);
                for id in &ids {
                    super::public_state::mark_public_state_object_dirty(state, *id);
                }
                super::public_state::mark_battlefield_display_dirty(state);
            }
        }
    }
}

/// Decide whether an `EnteredObjects` flush must conservatively escalate to a
/// full re-evaluation.
///
/// Two axes, both required-clean for the fast path:
///
/// 1. Per-entered preconditions: the entered object must not itself be the
///    source of a continuous effect, carry a CDA static, or carry a
///    control-override / type-change / text-change / counter / attachment /
///    transient effect (the entry enqueued none for a plain token).
///
/// 2. Board-wide escalation: no ACTIVE continuous effect may have a magnitude,
///    affected set, or source-level enabling CONDITION that reads battlefield
///    object population.
///    CR 611.3a: a static-ability continuous effect isn't locked in; it applies
///    at any moment to whatever its text indicates — so a board-population-
///    dependent magnitude, affected set, or enabling condition re-evaluates when
///    an object enters, changing PRE-EXISTING recipients. CR 613.7d: the entering
///    object receives its timestamp on zone entry. CR 613.8a: dependency/timestamp
///    ordering operates on the live set. This scan is O(active-effect-count), NOT
///    O(battlefield).
pub(crate) fn incremental_flush_must_escalate(
    state: &GameState,
    entered_ids: &HashSet<ObjectId>,
) -> bool {
    // Axis 1 — per-entered preconditions.
    for &id in entered_ids {
        let Some(obj) = state.objects.get(&id) else {
            // The entered object already left (e.g. it was a token that died to
            // an SBA before flush). A full pass is the safe handling.
            return true;
        };
        if entered_object_blocks_incremental(state, obj) {
            return true;
        }
    }

    // Axis 2a — magnitude + affected-set over the EXISTING active effect set,
    // NARROWED to entries that actually perturb the population input.
    //
    // Two-stage test per effect: the committed exhaustive classifier
    // (`quantity_expr_uses_object_count` / `affected_filter_uses_object_population`)
    // is the OUTER conjunct (compile-time tripwire — a future population-reading
    // variant forces a classification). Then the entry-aware narrowing layer asks
    // whether any ENTERED object can flip THIS effect's population input.
    //
    // CR 109.5: the filter's "you control" must resolve against the EFFECT
    // SOURCE's controller, not the entered object's — so `ctx` is built per-effect
    // from `e.source_id` + `e.controller`. Escalation is `classifier(e) &&
    // any_entered_perturbs(e)`; both required.
    if collect_shared_active_continuous_effects(state)
        .iter()
        .any(|e| {
            let magnitude = continuous_modification_dynamic_quantity(&e.modification);
            let magnitude_sensitive =
                magnitude.is_some_and(crate::game::quantity::quantity_expr_uses_object_count);
            let affected_sensitive =
                crate::game::filter::affected_filter_uses_object_population(&e.affected_filter);
            if !magnitude_sensitive && !affected_sensitive {
                return false;
            }
            let ctx = FilterContext::from_source_with_controller(e.source_id, e.controller);
            entered_ids.iter().any(|id| {
                let Some(entered) = state.objects.get(id) else {
                    return false;
                };
                (magnitude_sensitive
                    && magnitude.is_some_and(|expr| {
                        crate::game::quantity::entered_object_perturbs_quantity_expr(
                            state, entered, &ctx, expr,
                        )
                    }))
                    || (affected_sensitive
                        && crate::game::filter::entered_object_perturbs_affected_filter(
                            state,
                            *id,
                            &ctx,
                            &e.affected_filter,
                        ))
            })
        })
    {
        return true;
    }

    // Axis 2b — source-level enabling CONDITION over the EXISTING static-ability
    // sources, NARROWED to entries that actually perturb the condition. The
    // condition axis CANNOT be read off the collected `ActiveContinuousEffect`s:
    // `active_continuous_effects_from_static_definitions` evaluates a
    // non-recipient-context (source-level) condition as a gate at COLLECTION time
    // and stores `condition: None` on the resulting effect (only recipient-context
    // conditions are retained for per-recipient re-evaluation). So a board-
    // population gate like "as long as you control N creatures" is already consumed
    // and invisible on the active-effect set. We must inspect the intact
    // `StaticDefinition.condition` on each live source instead.
    //
    // CR 611.3a + CR 611.3b: when such a source-level enabling condition depends
    // on board population, an object entering can flip the condition for the
    // WHOLE recipient set, changing PRE-EXISTING recipients — so escalate to a
    // full rebuild. The entry-aware narrowing (built per-source from the visited
    // object, CR 109.5) skips escalation when no entered object can perturb the
    // gate; the truth-delta refinement (below) skips escalation even when an
    // entry perturbs the gate INPUT but does not flip its truth value.
    any_active_static_condition_perturbed_by_entry(state, entered_ids)
}

/// Scan every live static-ability source for a CONTINUOUS `StaticDefinition`
/// whose enabling `condition` is board-population-dependent AND that one of the
/// `entered_ids` actually perturbs. Walks the same source set as
/// `collect_shared_active_continuous_effects` (`for_each_static_effect_source`)
/// but reads the intact pre-collection `condition` field.
/// O(active-source-count × entered-count); short-circuits on the first match.
///
/// Three-stage test:
///  1. The committed exhaustive classifier
///     (`static_condition_uses_object_population`, OUTER conjunct, compile-time
///     tripwire) gates the entry-aware narrowing
///     (`entered_object_perturbs_static_condition`). CR 109.5: `ctx` is built per
///     SOURCE object so the condition's "you control" rebinds to the source's
///     controller, not the entered object's.
///  2. RECIPIENT-CONTEXT gates (CR 611.3b — the effect applies per recipient)
///     escalate UNCONDITIONALLY when perturbed: their truth is per-recipient and
///     cannot be summarized by a single board-level boolean
///     (`source_condition_gate_passes` only over-approximates them). This
///     preserves the d9a40be71 behavior for that class.
///  3. SOURCE-LEVEL gates (CR 611.3a — a single on/off switch consumed at
///     collection): apply the truth-delta short-circuit. The static's BEFORE
///     truth was cached at the last full eval in `static_gate_truth`; recompute
///     AFTER against the live post-entry board. Escalate iff `before != after`.
///     Key absent (source not present / phased out at the last full eval) ->
///     fail closed (escalate). Soundness rests on `after` being recomputed
///     authoritatively from the live board, so the test errs only toward
///     OVER-escalation, never under (safety theorem hypotheses: Continuous mode,
///     `!condition_uses_recipient_context`, affected-set + magnitude
///     population-independent — the latter two are escalated first by Axis 2a).
///
/// `def_index` indexes the LIVE post-layer `static_definitions` via
/// `iter_all().enumerate()` — IDENTICAL indexing to `refresh_static_gate_truth`
/// (invariant 5), so the cached BEFORE truth aligns with the consulted def.
fn any_active_static_condition_perturbed_by_entry(
    state: &GameState,
    entered_ids: &HashSet<ObjectId>,
) -> bool {
    let mut found = false;
    for_each_static_effect_source(state, |state, obj| {
        if found {
            return;
        }
        let ctx = FilterContext::from_source(state, obj.id);
        if obj
            .static_definitions
            .iter_all()
            .enumerate()
            .any(|(def_index, def)| {
                if def.mode != StaticMode::Continuous {
                    return false;
                }
                let Some(condition) = def.condition.as_ref() else {
                    return false;
                };
                if !static_condition_uses_object_population(condition) {
                    return false;
                }
                // CR 611.3b: recipient-context gates re-evaluate per recipient —
                // a single board-level boolean can't summarize them, so escalate
                // unconditionally when perturbed (preserve d9a40be71).
                if condition_uses_recipient_context(condition) {
                    return entered_ids.iter().any(|id| {
                        entered_object_perturbs_static_condition(state, *id, &ctx, condition)
                    });
                }
                let perturbed = entered_ids.iter().any(|id| {
                    entered_object_perturbs_static_condition(state, *id, &ctx, condition)
                });
                if !perturbed {
                    return false;
                }
                // CR 611.3a source-level truth-delta. A multi-axis static (gate
                // ON while magnitude/affected-set also population-sensitive) is
                // already escalated by Axis 2a above, so reaching here means the
                // condition is the only population-sensitive axis. Fail closed
                // when the key is absent (invariant 1: source not present /
                // phased out at the last full eval).
                let before = match state.static_gate_truth.get(&StaticGateKey {
                    source: obj.id,
                    def_index,
                }) {
                    Some(&b) => b,
                    None => return true,
                };
                let after = source_condition_gate_passes(state, condition, obj.controller, obj.id);
                before != after
            })
        {
            found = true;
        }
    });
    found
}

/// CR 611.3a + CR 611.3b: rewrite the source-level enabling-condition truth
/// cache from the FULLY-DERIVED board. Walks `for_each_static_effect_source`
/// (which skips phased-out sources, CR 702.26e), and records the gate truth of
/// every CONTINUOUS static carrying a NON-recipient-context `Some(condition)`,
/// keyed by `(source, def_index)` on the LIVE post-layer `static_definitions`
/// (`iter_all().enumerate()` — identical indexing to the consult; see invariant
/// 5). Recipient-context conditions are EXCLUDED: their truth is per-recipient,
/// re-evaluated per recipient via `evaluate_condition_with_recipient`, and
/// `source_condition_gate_passes` is only an over-approximation for them — so
/// they are never cached and always escalate.
///
/// CR 109.5: the gate's "you"/"your" resolves against the SOURCE's controller.
/// Cleared and repopulated wholesale (keyset is authoritative only for sources
/// present + non-phased at this full eval; absence at consult fails closed).
fn refresh_static_gate_truth(state: &mut GameState) {
    let mut next: im::HashMap<StaticGateKey, bool> = im::HashMap::new();
    for_each_static_effect_source(state, |state, obj| {
        for (def_index, def) in obj.static_definitions.iter_all().enumerate() {
            if def.mode != StaticMode::Continuous {
                continue;
            }
            let Some(condition) = def.condition.as_ref() else {
                continue;
            };
            if condition_uses_recipient_context(condition) {
                continue;
            }
            let truth = source_condition_gate_passes(state, condition, obj.controller, obj.id);
            next.insert(
                StaticGateKey {
                    source: obj.id,
                    def_index,
                },
                truth,
            );
        }
    });
    state.static_gate_truth = next;
}

/// CR 613.1: Continuous effects are applied in layers to determine object characteristics.
/// CR 122.1: Counters can modify object characteristics.
/// CR 301.5: Equipment attachments can affect equipped creatures.
/// CR 303.4: Aura attachments can affect enchanted objects.
/// True when the entered object cannot be handled by the incremental fast path
/// and the flush must escalate to a full re-evaluation. Conservative: any entry
/// kind whose layer contribution cannot be cheaply proven empty fails closed.
fn entered_object_blocks_incremental(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
) -> bool {
    // (1) The entered object sources at least one continuous effect (anthem,
    //     lord, type-changer, etc.): a full pass is required so its effect
    //     reaches every pre-existing recipient.
    if !active_continuous_effects_from_static_source(state, obj).is_empty() {
        return true;
    }
    // (2) CDA static: a characteristic-defining static defines the object's own
    //     P/T/color/type from game state and is not a plain entry.
    if obj
        .static_definitions
        .iter_all()
        .any(|s| s.characteristic_defining)
    {
        return true;
    }
    // (3) The entry carries no control-override / type-change / text-change /
    //     counter / attachment / transient effect. Counters and attachments are
    //     cheaply observable on the object; type/text/control overrides for a
    //     genuine new entry are sourced by statics already covered by (1)/(2).
    //     A controller differing from the base controller indicates a Layer-2
    //     override the incremental path does not reset for the rest of the board.
    if has_positive_counters(&obj.counters) {
        return true;
    }
    if obj.attached_to.is_some() || !obj.attachments.is_empty() {
        return true;
    }
    if obj
        .base_controller
        .is_some_and(|base| base != obj.controller)
    {
        return true;
    }
    false
}

/// Incremental layer re-derivation recipients: freshly-entered objects plus any
/// host they are already attached to (Song of the Dryads class — the aura's
/// continuous static applies to the enchanted permanent, not itself).
fn incremental_recipient_ids(
    state: &GameState,
    entered_ids: &HashSet<ObjectId>,
) -> HashSet<ObjectId> {
    let mut recipients = entered_ids.clone();
    for &id in entered_ids {
        if let Some(host) = state
            .objects
            .get(&id)
            .and_then(|obj| obj.attached_to.as_ref())
            .and_then(|target| target.as_object())
        {
            recipients.insert(host);
        }
    }
    recipients
}

/// Incremental layer re-derivation for a set of freshly-entered objects.
///
/// Mirrors the PER-OBJECT subset of `evaluate_layers` for `entered_ids` only:
/// resets each entered object to its base characteristics, re-applies the
/// EXISTING global continuous-effect set (restricted to the entered objects via
/// `apply_continuous_effect_to`), runs the per-object counter / keyword-counter /
/// P-T-counter / loyalty fixups and the combat-assignment-rule application, then
/// rebuilds the TriggerIndex (CR 603.6a + CR 611.2e — granted-trigger
/// visibility). It does NOT clear attribution globally or touch the rest of the
/// battlefield: pre-existing objects keep their already-derived characteristics.
///
/// Caller (`flush_layers`) only reaches this path after
/// `incremental_flush_must_escalate` returned false, which guarantees no active
/// effect's magnitude or affected set reads board population — so re-deriving
/// just the entered objects yields a board identical to a full pass (CR 613.1).
fn apply_layers_incremental(state: &mut GameState, entered_ids: &HashSet<ObjectId>) {
    let recipient_ids = incremental_recipient_ids(state, entered_ids);
    let mut abilities_suppressed = HashSet::new();
    // Step 1 (per-entered subset): reset computed characteristics to base.
    for &id in &recipient_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.sync_missing_base_characteristics();
            obj.name = obj.base_name.clone();
            obj.power = obj.base_power;
            obj.toughness = obj.base_toughness;
            obj.loyalty = obj.base_loyalty;
            obj.card_types = obj.base_card_types.clone();
            obj.mana_cost = obj.base_mana_cost.clone();
            obj.keywords = obj.base_keywords.clone();
            obj.abilities = Arc::clone(&obj.base_abilities);
            obj.trigger_definitions = Arc::clone(&obj.base_trigger_definitions).into();
            obj.replacement_definitions = Arc::clone(&obj.base_replacement_definitions).into();
            obj.static_definitions = Arc::clone(&obj.base_static_definitions).into();
            obj.color = obj.base_color.clone();
            obj.printed_ref = obj.base_printed_ref.clone();
            obj.controller = obj.base_controller.unwrap_or(obj.owner);
            obj.assigns_damage_from_toughness = false;
            obj.assigns_damage_as_though_unblocked = false;
            obj.assigns_no_combat_damage = false;
            // CR 701.60c: re-derive the suspected designation's menace +
            // "can't block" onto the just-reset live fields (mirrors the full
            // pass) so the incremental path agrees with `evaluate_layers`.
            derive_suspected_abilities(obj);
        }
    }

    // CR 611.2 + CR 613.1: Rebuild the static-effect-source index before the
    // incremental gathers. The incremental path only resets `entered_ids` to
    // base; pre-existing generators keep their already-derived
    // `static_definitions`, which for a generator still carries its continuous
    // def — so a full-battlefield rebuild here lists every current generator
    // (pre-existing + entered). An entered base generator never reaches this
    // path (`entered_object_blocks_incremental` escalates it to a full eval), so
    // this is purely a freshness guarantee for the incremental gather. The
    // `rebuild_static_index_at_top` guard is ALWAYS true in production; togglable
    // only under `cfg(test)`.
    if rebuild_static_index_at_top() {
        crate::types::game_state::StaticSourceIndex::rebuild_from_state(state);
    }

    // Step 2: Copy effects first (Layer 1), restricted to recipient objects.
    let copy_effects = gather_active_effects_for_layer(state, Layer::Copy);
    let ordered_copy = order_active_continuous_effects(Layer::Copy, &copy_effects, state);
    for effect in &ordered_copy {
        apply_continuous_effect_to(state, effect, &recipient_ids, &mut abilities_suppressed);
    }

    let recipient_vec: Vec<ObjectId> = recipient_ids.iter().copied().collect();
    if crate::game::stickers::apply_battlefield_name_and_ability_stickers(state, &recipient_vec) {
        // Incremental resets clear the entered/attached recipients back to base,
        // so retained stickers must be re-applied before the restricted main
        // gather. If a sticker grants a continuous static ability, refresh the
        // generator index so the recipient can source that effect in this pass.
        crate::types::game_state::StaticSourceIndex::rebuild_from_state(state);
    }

    // Step 3-4: Remaining layers in order, restricted to recipient objects.
    let effects_by_layer = gather_active_continuous_effects(state);
    for (layer, layer_bucket) in &effects_by_layer {
        if *layer == Layer::Copy {
            continue;
        }
        if !layer_bucket.is_empty() {
            let layer_effects: Vec<&ActiveContinuousEffect> = layer_bucket.iter().collect();
            let ordered = if layer.has_dependency_ordering() {
                order_with_dependencies(&layer_effects, state)
            } else {
                order_by_timestamp(&layer_effects)
            };
            for effect in &ordered {
                apply_continuous_effect_to(
                    state,
                    effect,
                    &recipient_ids,
                    &mut abilities_suppressed,
                );
            }
        }
        // CR 613.1f: mirror the full-pass end-of-Layer-6 denial hook for the
        // incremental path, restricted to the freshly-entered objects that were
        // reset and re-derived in this pass.
        if *layer == Layer::Ability {
            apply_cant_have_keyword_denials(state, Some(&recipient_ids));
        }
        // CR 613.4c: P/T counters modify power/toughness in layer 7c, before the
        // 7d switch (CR 613.4d). The CounterPT bucket carries no continuous
        // effects, so fold the on-object counters in here.
        if *layer == Layer::CounterPT {
            apply_pt_counter_modifications(state, recipient_ids.iter().copied());
        }
        if *layer == Layer::Type {
            apply_prototype_characteristics(state, recipient_ids.iter().copied());
            apply_intrinsic_basic_land_mana_abilities(state, &recipient_vec);
        }
    }

    // CR 702.73a: Changeling — entered object gains all creature types if it now
    // has Changeling but no CDA covered it.
    if !state.all_creature_types.is_empty() {
        for &id in entered_ids {
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

    // CR 122.1b + CR 613.1f: Keyword counters grant their keyword (Layer 6).
    // CR 306.5c: loyalty re-derives from loyalty counters. Per-entered fixups
    // only. (P/T counters are applied in-loop at Layer::CounterPT above, in
    // layer 7c before the 7d switch — CR 613.4c/613.4d.)
    for &id in &recipient_ids {
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

            if let Some(&loyalty_counters) = obj.counters.get(&CounterType::Loyalty) {
                obj.loyalty = Some(loyalty_counters);
            }
        }
    }

    // CR 113.11: re-apply the can't-have denial after the keyword-counter grant
    // (which runs after the in-loop Layer 6 denial) so a counter can't add a
    // forbidden keyword. Restricted to the freshly-entered objects, mirroring the
    // incremental denial hook above.
    apply_cant_have_keyword_denials(state, Some(&recipient_ids));

    // CR 613.11: Combat-assignment rule effects, restricted to recipient objects.
    apply_combat_assignment_rule_effects_filtered(state, Some(&recipient_ids));

    // CR 603.6a + CR 611.2e: Rebuild the TriggerIndex so the next event scan
    // sees the entered objects' (and any granted) trigger sets.
    crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);

    // Test-only buggy end-of-pass static-index placement (see `evaluate_layers`).
    if !rebuild_static_index_at_top() {
        crate::types::game_state::StaticSourceIndex::rebuild_from_state(state);
    }
}

fn gather_active_effects_for_layer(state: &GameState, layer: Layer) -> Vec<ActiveContinuousEffect> {
    collect_shared_active_continuous_effects(state)
        .into_iter()
        .filter(|effect| effect.layer == layer)
        .collect()
}

pub(crate) fn has_active_copy_layer_effects(state: &GameState) -> bool {
    !gather_active_effects_for_layer(state, Layer::Copy).is_empty()
}

/// CR 718.3b: A prototyped spell and the permanent it becomes have only their
/// alternative mana cost and P/T characteristics. If that mana cost contains
/// colored mana symbols, the spell/permanent is those colors. Reapply this after
/// layer reset so the prototype marker survives normal layer recomputation.
fn apply_prototype_characteristics(state: &mut GameState, ids: impl IntoIterator<Item = ObjectId>) {
    for id in ids {
        let Some(obj) = state.objects.get_mut(&id) else {
            continue;
        };
        let Some(form) = obj.prototype_form.clone() else {
            continue;
        };
        obj.mana_cost = form.mana_cost;
        obj.power = Some(form.power);
        obj.toughness = Some(form.toughness);
        obj.color = form.colors;
    }
}

/// CR 613.4c: Fold each permanent's power/toughness counters into its P/T in
/// layer 7c. Counters are object state rather than continuous effects, so this
/// runs at the `Layer::CounterPT` step of the layer loop — after the 7c `+N/+N`
/// effects and before the 7d power/toughness switch (CR 613.4d). Applying it
/// after the switch would transpose asymmetric P/T counters (e.g. `+0/+1`,
/// `-1/-0`) onto the wrong axis.
fn apply_pt_counter_modifications(state: &mut GameState, ids: impl IntoIterator<Item = ObjectId>) {
    for id in ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            if !has_positive_counters(&obj.counters) {
                continue;
            }

            let (power_delta, toughness_delta) = obj.counters.iter().fold(
                (0i32, 0i32),
                |(power_total, toughness_total), (counter_type, count)| {
                    let Some((power, toughness)) = counter_type.power_toughness_delta() else {
                        return (power_total, toughness_total);
                    };
                    let count = crate::game::arithmetic::u32_to_i32_saturating(*count);
                    (
                        power_total.saturating_add(power.saturating_mul(count)),
                        toughness_total.saturating_add(toughness.saturating_mul(count)),
                    )
                },
            );
            if power_delta != 0 {
                if let Some(ref mut p) = obj.power {
                    *p = saturating_pt_add(*p, power_delta);
                }
            }
            if toughness_delta != 0 {
                if let Some(ref mut t) = obj.toughness {
                    *t = saturating_pt_add(*t, toughness_delta);
                }
            }
        }
    }
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

    for_each_static_effect_source(state, |state, obj| {
        effects.extend(active_continuous_effects_from_static_source(state, obj));
    });
    gather_transient_continuous_effects(state, &mut effects);
    gather_ring_emblem_continuous_effects(state, &mut effects);
    effects
}

fn gather_ring_emblem_continuous_effects(
    state: &GameState,
    effects: &mut Vec<ActiveContinuousEffect>,
) {
    for &player in state.ring_level.keys() {
        let Some(bearer_id) = super::effects::ring::ring_bearer_for(state, player) else {
            continue;
        };
        let timestamp = state
            .objects
            .get(&bearer_id)
            .map(|obj| obj.timestamp)
            .unwrap_or_default();
        let modification = ContinuousModification::AddSupertype {
            supertype: Supertype::Legendary,
        };
        // CR 701.54c: The Ring emblem makes its controller's Ring-bearer
        // legendary. Model the emblem's type-changing continuous effect in
        // layer 4 with the bearer as the affected object.
        effects.push(ActiveContinuousEffect {
            source_id: bearer_id,
            controller: player,
            def_index: None,
            transient_id: None,
            mod_index: 0,
            layer: modification.layer(),
            timestamp,
            modification,
            affected_filter: TargetFilter::SpecificObject { id: bearer_id },
            condition: None,
            mode: StaticMode::Continuous,
            characteristic_defining: false,
        });
    }
}

fn for_each_static_effect_source(
    state: &GameState,
    mut visit: impl FnMut(&GameState, &crate::game::game_object::GameObject),
) {
    // CR 611.2 + CR 613.1: Iterate the static-effect-source index buckets
    // instead of scanning the full battlefield / command zone. The index lists
    // only objects that GENERATE ≥1 continuous effect (rebuilt at the top of
    // every layer pass — see `static_source_index.rs`), so this loop is
    // O(generators) rather than O(battlefield). The per-object gates below are
    // retained verbatim; the index only narrows WHICH ids to look at.
    //
    // Defense-in-depth: a never-yet-evaluated `&GameState` (post-deserialize, or
    // a hand-built test state that never ran a flush — e.g. an off-zone keyword
    // query against a command-zone emblem before any layer pass) has an empty
    // index but a non-empty battlefield and/or command zone.
    // `for_each_static_effect_source` takes `&GameState` and cannot rebuild, so
    // fall back to a direct battlefield + command scan when BOTH indexed buckets
    // are empty AND either source zone is non-empty. (Gating only on the
    // battlefield would miss a command-zone-only emblem board — see
    // `command_zone_emblem_grants_keyword_to_non_battlefield_card`.)
    let index = &state.static_source_index;
    let use_fallback = index.battlefield_sources.is_empty()
        && index.command_sources.is_empty()
        && (!state.battlefield.is_empty() || !state.command_zone.is_empty());

    if use_fallback {
        // CR 702.26e: phased-out permanents contribute no continuous effects.
        for &id in &state.battlefield {
            if state
                .objects
                .get(&id)
                .is_some_and(|obj| obj.is_phased_out())
            {
                continue;
            }
            if let Some(obj) = state.objects.get(&id) {
                visit(state, obj);
            }
        }
        // CR 114.3: command-zone emblems have static abilities that affect the
        // game. CR 905.4 + CR 113.6b: a face-up conspiracy's static abilities
        // function from the command zone too.
        for &id in &state.command_zone {
            let Some(obj) = state.objects.get(&id) else {
                continue;
            };
            if obj.is_emblem || crate::game::conspiracy::functions_from_command_zone(obj) {
                visit(state, obj);
            }
        }
    } else {
        // CR 702.26e: Continuous effects generated by phased-out permanents don't
        // include anything in their set of affected objects — skip phased-out
        // sources here rather than filtering later. The index includes them
        // (they're in `state.battlefield`); the skip below excludes them.
        for &id in &index.battlefield_sources {
            let Some(obj) = state.objects.get(&id) else {
                continue;
            };
            if obj.is_phased_out() {
                continue;
            }
            visit(state, obj);
        }
        // CR 114.3: Emblems in the command zone have static abilities that affect
        // the game. CR 905.4 + CR 113.6b: a face-up conspiracy's static abilities
        // function from the command zone too. The index already filtered to these
        // command-zone generators; the gate is re-asserted here for parity with
        // the fallback path.
        for &id in &index.command_sources {
            let Some(obj) = state.objects.get(&id) else {
                continue;
            };
            if obj.is_emblem || crate::game::conspiracy::functions_from_command_zone(obj) {
                visit(state, obj);
            }
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
        visit(state, obj);
    }
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

        let retained_condition = if let Some(condition) = &def.condition {
            if !source_condition_gate_passes(state, condition, controller, source_id) {
                continue;
            }
            condition_uses_recipient_context(condition).then(|| condition.clone())
        } else {
            None
        };

        let affected_filter = def.affected.clone().unwrap_or(TargetFilter::Any);
        for (mod_index, modification) in def.modifications.iter().enumerate() {
            if is_combat_assignment_rule_modification(modification) {
                continue;
            }
            // CR 113.3d + CR 604.1 + CR 611.2c: A `GrantStaticAbility` modification
            // installs the inner static onto every recipient matching the host's
            // `affected_filter`. The recipient is the granted-static's *source*
            // for the purposes of resolving `ControllerRef::You` and per-recipient
            // condition gating — the inner static functions exactly as if it
            // were printed on the recipient (CR 604.1). We synthesize the inner
            // modifications as additional `ActiveContinuousEffect`s here (one
            // per recipient per inner modification) so the inner effects take
            // effect during the same `evaluate_layers` pass — without this
            // gather-time expansion, the layer-6 push onto `obj.static_definitions`
            // would not appear in `effects_by_layer` (which is captured before
            // layer 6 applies) and the inner static would be inert for a full pass.
            if let ContinuousModification::GrantStaticAbility { definition: inner } = modification {
                effects.extend(expand_granted_static_effects(
                    state,
                    source_id,
                    timestamp,
                    &affected_filter,
                    inner.as_ref(),
                ));
                // Continue: also push the meta-effect below so layer-6 apply
                // pushes the inner static onto the recipient's
                // `static_definitions` for inspectability and downstream
                // queries (e.g., parser/coverage walks).
            }
            // CR 613.1f + CR 113.3: "~ has all activated abilities of [source]"
            // (Myr Welder, Territory Forge, …). Expand into one `GrantAbility` per
            // activated ability of each object matching `source`, so the dynamic
            // set is recomputed each pass and reuses the existing GrantAbility
            // apply + dedup. The meta-effect itself has no standalone layer-6
            // behaviour, so skip pushing it.
            if let ContinuousModification::GrantAllActivatedAbilitiesOf { source } = modification {
                effects.extend(expand_granted_activated_abilities(
                    state,
                    source_id,
                    timestamp,
                    &affected_filter,
                    source,
                ));
                continue;
            }
            effects.push(ActiveContinuousEffect {
                source_id,
                controller,
                def_index: Some(def_idx),
                transient_id: None,
                mod_index,
                layer: modification.layer(),
                timestamp,
                modification: modification.clone(),
                affected_filter: affected_filter.clone(),
                condition: retained_condition.clone(),
                mode: def.mode.clone(),
                characteristic_defining: def.characteristic_defining,
            });
        }
    }

    effects
}

/// CR 113.3d + CR 604.1 + CR 611.2c: Expand a `GrantStaticAbility` into one
/// `ActiveContinuousEffect` per inner modification per recipient matching the
/// host's `host_affected_filter`. Each recipient becomes the synthesized
/// effect's `source_id` so `ControllerRef::You` and any other source-relative
/// references in `inner.affected` resolve against the recipient — which is the
/// semantic the CR requires for a granted ability ("its controller is the
/// controller of the object that gained the ability"). The synthesized effects
/// carry the inner static's own `condition`, `mode`, and CDA flag.
///
/// Single-pass limitation: if `inner.modifications` itself contains another
/// `GrantStaticAbility`, this function does not recursively expand it within
/// the same `evaluate_layers` pass — the inner-inner grant lands on the
/// recipient's `static_definitions` via the apply step and only contributes on
/// the next layer evaluation triggered by `layers_dirty`. No known Magic card
/// exercises a quoted-within-quoted grant, so this is acceptable for now;
/// revisit if such a card appears.
///
/// Intra-static identity-key limitation: synthesized inner effects use
/// `(source_id = recipient_id, def_index = None, transient_id = None)`, the same
/// triple `depends_on` keys on to suppress dependency edges between one static's
/// own clauses (CR 613.7a). Two DISTINCT grants of type-changing + type-referencing
/// inner modifications onto the SAME recipient therefore share one identity key, so
/// `depends_on` would suppress the cross-grant edge between them as if they were one
/// static. No known card grants two such interacting static abilities to the same
/// recipient; documented here for the next maintainer who hits that case.
fn expand_granted_static_effects(
    state: &GameState,
    host_source_id: ObjectId,
    host_timestamp: u64,
    host_affected_filter: &TargetFilter,
    inner: &StaticDefinition,
) -> Vec<ActiveContinuousEffect> {
    if inner.mode != StaticMode::Continuous {
        return Vec::new();
    }
    let inner_affected = inner.affected.clone().unwrap_or(TargetFilter::Any);
    let ctx = crate::game::filter::FilterContext::from_source(state, host_source_id);
    let mut out = Vec::new();
    for &recipient_id in &state.battlefield {
        if !crate::game::filter::matches_target_filter(
            state,
            recipient_id,
            host_affected_filter,
            &ctx,
        ) {
            continue;
        }
        let recipient_controller = match state.objects.get(&recipient_id) {
            Some(obj) => obj.controller,
            None => continue,
        };
        // CR 109.5 + CR 113.7: "You" inside the granted ability refers to the
        // recipient's controller. Re-run any inner condition gate with the
        // recipient as the source so that gating like "during your turn"
        // resolves against the recipient's controller.
        let retained_inner_condition = if let Some(condition) = &inner.condition {
            if !source_condition_gate_passes(state, condition, recipient_controller, recipient_id) {
                continue;
            }
            condition_uses_recipient_context(condition).then(|| condition.clone())
        } else {
            None
        };
        for (mod_index, modification) in inner.modifications.iter().enumerate() {
            if is_combat_assignment_rule_modification(modification) {
                continue;
            }
            out.push(ActiveContinuousEffect {
                source_id: recipient_id,
                controller: recipient_controller,
                // Distinguish synthesized inner effects from the host's own
                // static-definition entries so `apply_continuous_effect` doesn't
                // confuse them with the host's `static_definitions[def_idx]`.
                def_index: None,
                transient_id: None,
                mod_index,
                layer: modification.layer(),
                // Inherit the host's timestamp so ordering within a layer is
                // stable and reproducible per CR 613.7.
                timestamp: host_timestamp,
                modification: modification.clone(),
                affected_filter: inner_affected.clone(),
                condition: retained_inner_condition.clone(),
                mode: inner.mode.clone(),
                characteristic_defining: inner.characteristic_defining,
            });
        }
    }
    out
}

/// CR 613.1f + CR 113.3: Expand a `GrantAllActivatedAbilitiesOf { source }` host
/// modification into one `GrantAbility` effect per activated ability of each
/// object matching `source`. Object-self references in `source` are resolved
/// relative to the host static, while controller-relative references use each
/// recipient's controller. Provider objects are scanned across all zones — the
/// granted-from cards are typically in exile, not on the battlefield — in
/// deterministic `ObjectId` order. Both mana and non-mana activated abilities
/// are granted. Synthesized effects target the recipient via `SelfRef`, reusing
/// the layer-6 `GrantAbility` apply and its structural dedup, and are recomputed
/// each pass so the granted set tracks the current `source` membership.
fn expand_granted_activated_abilities(
    state: &GameState,
    host_source_id: ObjectId,
    host_timestamp: u64,
    host_affected_filter: &TargetFilter,
    source: &TargetFilter,
) -> Vec<ActiveContinuousEffect> {
    let host_ctx = crate::game::filter::FilterContext::from_source(state, host_source_id);
    let mut out = Vec::new();
    let mut provider_ids: Vec<ObjectId> = state.objects.keys().copied().collect();
    provider_ids.sort_unstable_by_key(|id| id.0);
    // CR 109.5: the provider `source` filter resolves through a context built
    // purely from the (constant) host id and the recipient's controller — the
    // recipient id is NOT part of it. So the matching-provider set is identical
    // for every recipient sharing a controller. Memoize it per controller so
    // the O(recipients × objects) filter sweep collapses to O(controllers ×
    // objects). The recipient-equality self-skip stays per-recipient at
    // emission (CR 613.1f), keeping the emitted set byte-identical.
    let mut providers_by_controller: std::collections::HashMap<PlayerId, Vec<ObjectId>> =
        std::collections::HashMap::new();
    for &recipient_id in &state.battlefield {
        if !crate::game::filter::matches_target_filter(
            state,
            recipient_id,
            host_affected_filter,
            &host_ctx,
        ) {
            continue;
        }
        let recipient_controller = match state.objects.get(&recipient_id) {
            Some(obj) => obj.controller,
            None => continue,
        };
        // CR 109.5 + CR 113.7a: Split the `source` filter's resolution context.
        // Object-self references in the named card's text — `ExiledBySource`
        // ("cards exiled with [this card]") and `SameName` ("the same name as
        // this creature") — name the object that HAS the static ability, i.e.
        // the host. Possessive/controller references — `ControllerRef::You`
        // ("creatures *you* control") — refer to the controller of the object
        // that *gained* the ability, i.e. the recipient. So resolve against the
        // host id with the recipient's controller. For self-granting hosts
        // (Myr Welder, Territory Forge, Marvin) host == recipient and this
        // coincides with the recipient context; for cross-object grants
        // (Agatha's Soul Cauldron grants creatures-you-control the abilities of
        // *its own* exiled creature cards) only the host id finds the exile
        // links while "you" still tracks the recipient's controller.
        let matching = providers_by_controller
            .entry(recipient_controller)
            .or_insert_with(|| {
                let provider_ctx = crate::game::filter::FilterContext::from_source_with_controller(
                    host_source_id,
                    recipient_controller,
                );
                provider_ids
                    .iter()
                    .copied()
                    .filter(|&provider_id| {
                        crate::game::perf_counters::record_granted_ability_provider_scan();
                        crate::game::filter::matches_target_filter(
                            state,
                            provider_id,
                            source,
                            &provider_ctx,
                        )
                    })
                    .collect()
            });
        let mut next_mod_index = 0usize;
        for &provider_id in matching.iter() {
            if provider_id == recipient_id {
                continue;
            }
            let Some(provider) = state.objects.get(&provider_id) else {
                continue;
            };
            for ability in provider.abilities.iter() {
                if ability.kind != crate::types::ability::AbilityKind::Activated {
                    continue;
                }
                out.push(ActiveContinuousEffect {
                    source_id: recipient_id,
                    controller: recipient_controller,
                    def_index: None,
                    transient_id: None,
                    mod_index: next_mod_index,
                    layer: Layer::Ability,
                    timestamp: host_timestamp,
                    modification: ContinuousModification::GrantAbility {
                        definition: Box::new(ability.clone()),
                    },
                    affected_filter: TargetFilter::SelfRef,
                    condition: None,
                    mode: StaticMode::Continuous,
                    characteristic_defining: false,
                });
                next_mod_index += 1;
            }
        }
    }
    out
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

        if !transient_duration_holds(state, tce) {
            continue;
        }

        let retained_condition = if let Some(condition) = &tce.condition {
            if !source_condition_gate_passes(state, condition, tce.controller, tce.source_id) {
                continue;
            }
            condition_uses_recipient_context(condition).then(|| condition.clone())
        } else {
            None
        };

        for (mod_index, modification) in tce.modifications.iter().enumerate() {
            if is_combat_assignment_rule_modification(modification) {
                continue;
            }
            // CR 708.5: A `MayLookAtFaceDown` look permission (Lumbering Laundry's
            // duration-bound grant) is a player-scoped visibility permission read
            // directly off the TCE by `visibility::viewer_may_look_at_face_down`
            // — never applied to an object's `static_definitions` through the
            // layer system. Its `affected` filter (face-down creatures you don't
            // control) matches OPPONENT permanents, so feeding it into the object
            // layer gather would erroneously stamp the permission onto each
            // opponent's face-down creature. Skip it here, mirroring the printed
            // `MayLookAtFaceDown` static (built with empty `modifications`, read
            // by mode in `battlefield_active_statics`).
            if matches!(
                modification,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::MayLookAtFaceDown,
                }
            ) {
                continue;
            }
            effects.push(ActiveContinuousEffect {
                source_id: tce.source_id,
                controller: tce.controller,
                def_index: None,
                transient_id: Some(tce.id),
                mod_index,
                layer: modification.layer(),
                timestamp: tce.timestamp,
                modification: modification.clone(),
                affected_filter: tce.affected.clone(),
                condition: retained_condition.clone(),
                mode: StaticMode::Continuous,
                characteristic_defining: false,
            });
        }
    }
}

fn transient_duration_holds(state: &GameState, tce: &TransientContinuousEffect) -> bool {
    let Duration::ForAsLongAs { ref condition } = tce.duration else {
        return true;
    };

    // CR 611.2b: A recipient-referential condition ("for as long as IT has a
    // shield counter" — Shield Broker's gain-control) refers to the object the
    // effect applies to, not the source. For a single-object effect that object
    // is the affected `SpecificObject`; evaluate against it so the duration
    // tracks the controlled/granted creature's counters rather than the source.
    match (&tce.affected, condition_uses_recipient_context(condition)) {
        (TargetFilter::SpecificObject { id }, true) => {
            // CR 611.2b: a target-relative duration tracks the captured
            // `duration_subject` (the copy target for BecomeCopy — Zygon
            // Infiltrator) when it diverges from `affected`; otherwise the
            // affected object (Shield Broker's recipient-relative control
            // duration, where the recipient IS the tracked object).
            let recipient = tce.duration_subject.unwrap_or(*id);
            evaluate_condition_with_recipient(
                state,
                condition,
                tce.controller,
                tce.source_id,
                recipient,
            )
        }
        _ => evaluate_condition(state, condition, tce.controller, tce.source_id),
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

fn is_combat_assignment_rule_modification(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AssignDamageFromToughness
            | ContinuousModification::AssignDamageAsThoughUnblocked
            | ContinuousModification::AssignNoCombatDamage
    )
}

fn apply_combat_assignment_rule_effects(state: &mut GameState) {
    apply_combat_assignment_rule_effects_filtered(state, None);
}

/// CR 613.11: Continuous effects that affect game rules are applied after
/// object-affecting continuous effects.
fn apply_combat_assignment_rule_effects_filtered(
    state: &mut GameState,
    restrict_to: Option<&HashSet<ObjectId>>,
) {
    let mut effects = collect_active_combat_assignment_rule_effects(state);
    effects.sort_by_key(|effect| (effect.timestamp, effect.controller.0, effect.source_id.0));

    for effect in effects {
        let scan_zone = effect
            .affected_filter
            .extract_in_zone()
            .unwrap_or(crate::types::zones::Zone::Battlefield);
        let scan_ids = super::targeting::zone_object_ids(state, scan_zone);
        let ctx = FilterContext::from_source(state, effect.source_id);
        let affected_ids: Vec<ObjectId> = scan_ids
            .iter()
            .filter(|&&id| restrict_to.is_none_or(|ids| ids.contains(&id)))
            .filter(|&&id| matches_target_filter(state, id, &effect.affected_filter, &ctx))
            .filter(|&&id| {
                effect.condition.as_ref().is_none_or(|condition| {
                    evaluate_condition_with_recipient(
                        state,
                        condition,
                        effect.controller,
                        effect.source_id,
                        id,
                    )
                })
            })
            .copied()
            .collect();

        for id in affected_ids {
            if let Some(obj) = state.objects.get_mut(&id) {
                match effect.modification {
                    ContinuousModification::AssignDamageFromToughness => {
                        obj.assigns_damage_from_toughness = true;
                    }
                    ContinuousModification::AssignDamageAsThoughUnblocked => {
                        obj.assigns_damage_as_though_unblocked = true;
                    }
                    ContinuousModification::AssignNoCombatDamage => {
                        obj.assigns_no_combat_damage = true;
                    }
                    _ => {}
                }
            }
        }
    }
}

/// CR 613.1f: Layer 6 applies "effects that say an object can't have an ability."
/// `StaticMode::CantHaveKeyword { keyword }` (Theros Archetype cycle, Arcane
/// Lighthouse: "... can't have or gain [keyword]") denies the keyword to its
/// affected objects. This is run AFTER all keyword grants/removals are applied,
/// so the denial wins regardless of grant timestamp — the rules-correct "can't
/// have" outcome (a concurrent anthem can't restore a denied keyword).
fn apply_cant_have_keyword_denials(state: &mut GameState, restrict_to: Option<&HashSet<ObjectId>>) {
    // Collect (affected object, denied keyword) pairs under an immutable borrow,
    // then strip — avoids a borrow conflict with the per-object mutation.
    let mut denials: Vec<(ObjectId, Keyword)> = Vec::new();
    for (source, def) in super::functioning_abilities::battlefield_functioning_statics(state) {
        let StaticMode::CantHaveKeyword { keyword } = &def.mode else {
            continue;
        };
        let ctx = FilterContext::from_source(state, source.id);
        for id in super::targeting::zone_object_ids(state, crate::types::zones::Zone::Battlefield) {
            if restrict_to.is_some_and(|ids| !ids.contains(&id)) {
                continue;
            }
            // CR 604.1: a static with no `affected` filter is intrinsically SelfRef.
            let affected = match def.affected.as_ref() {
                None => id == source.id,
                Some(filter) => matches_target_filter(state, id, filter, &ctx),
            };
            if !affected {
                continue;
            }
            if let Some(condition) = def.condition.as_ref() {
                if !evaluate_condition_with_recipient(
                    state,
                    condition,
                    source.controller,
                    source.id,
                    id,
                ) {
                    continue;
                }
            }
            denials.push((id, keyword.clone()));
        }
    }
    for (id, keyword) in denials {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.keywords.retain(|k| k != &keyword);
        }
    }
}

fn collect_active_combat_assignment_rule_effects(
    state: &GameState,
) -> Vec<ActiveCombatAssignmentRuleEffect> {
    let mut effects = Vec::new();

    for_each_static_effect_source(state, |state, obj| {
        effects.extend(active_combat_assignment_rule_effects_from_static_source(
            state, obj,
        ));
    });

    collect_transient_combat_assignment_rule_effects(state, &mut effects);
    effects
}

fn active_combat_assignment_rule_effects_from_static_source(
    state: &GameState,
    source: &crate::game::game_object::GameObject,
) -> Vec<ActiveCombatAssignmentRuleEffect> {
    active_combat_assignment_rule_effects_from_static_definitions(
        state,
        source.id,
        source.controller,
        source.timestamp,
        source.static_definitions.as_slice(),
    )
}

fn active_combat_assignment_rule_effects_from_static_definitions(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    timestamp: u64,
    static_definitions: &[StaticDefinition],
) -> Vec<ActiveCombatAssignmentRuleEffect> {
    let mut effects = Vec::new();
    let source_zone = state.objects.get(&source_id).map(|o| o.zone);

    for def in static_definitions {
        if def.mode != StaticMode::Continuous {
            continue;
        }

        if !def.active_zones.is_empty() {
            let Some(zone) = source_zone else { continue };
            if !def.active_zones.contains(&zone) {
                continue;
            }
        }

        let retained_condition = if let Some(condition) = &def.condition {
            if !source_condition_gate_passes(state, condition, controller, source_id) {
                continue;
            }
            condition_uses_recipient_context(condition).then(|| condition.clone())
        } else {
            None
        };

        let affected_filter = def.affected.clone().unwrap_or(TargetFilter::Any);
        effects.extend(
            def.modifications
                .iter()
                .filter(|modification| is_combat_assignment_rule_modification(modification))
                .map(|modification| ActiveCombatAssignmentRuleEffect {
                    source_id,
                    controller,
                    timestamp,
                    modification: modification.clone(),
                    affected_filter: affected_filter.clone(),
                    condition: retained_condition.clone(),
                }),
        );
    }

    effects
}

fn collect_transient_combat_assignment_rule_effects(
    state: &GameState,
    effects: &mut Vec<ActiveCombatAssignmentRuleEffect>,
) {
    for tce in &state.transient_continuous_effects {
        if tce.duration == Duration::UntilHostLeavesPlay
            && !state
                .objects
                .get(&tce.source_id)
                .is_some_and(|obj| obj.zone == crate::types::zones::Zone::Battlefield)
        {
            continue;
        }

        if !transient_duration_holds(state, tce) {
            continue;
        }

        let retained_condition = if let Some(condition) = &tce.condition {
            if !source_condition_gate_passes(state, condition, tce.controller, tce.source_id) {
                continue;
            }
            condition_uses_recipient_context(condition).then(|| condition.clone())
        } else {
            None
        };

        effects.extend(
            tce.modifications
                .iter()
                .filter(|modification| is_combat_assignment_rule_modification(modification))
                .map(|modification| ActiveCombatAssignmentRuleEffect {
                    source_id: tce.source_id,
                    controller: tce.controller,
                    timestamp: tce.timestamp,
                    modification: modification.clone(),
                    affected_filter: tce.affected.clone(),
                    condition: retained_condition.clone(),
                }),
        );
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
    // CR 613.7: equal-timestamp same-source effects (a single static ability's
    // modifications all share one timestamp per CR 613.7a) get a deterministic
    // written-order tiebreak via `mod_index` — the index of the modification
    // within the source's `modifications` Vec, i.e. Oracle written order.
    sorted.sort_by_key(|e| {
        (
            !e.characteristic_defining,
            e.timestamp,
            e.source_id.0,
            e.def_index,
            e.mod_index,
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
    // CR 613.7a + CR 613.8a: A single static ability's modifications share one
    // timestamp and apply in the order written (613.7a). "Depend on" (613.8a) is a
    // relationship between an effect and ANOTHER effect (distinct generators) — it
    // never governs the internal sequencing of one ability's own clauses. Suppress
    // dependency edges between modifications flattened from the same static so that
    // e.g. RemoveAllSubtypes{Creature} wipes pre-existing subtypes and a later
    // AddSubtype survives, exactly as written.
    if a.source_id == b.source_id && a.def_index == b.def_index && a.transient_id == b.transient_id
    {
        return false;
    }

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
            | ContinuousModification::AddAllLandTypes
            | ContinuousModification::AddChosenSubtype { .. }
            | ContinuousModification::SetBasicLandType { .. }
            | ContinuousModification::SetChosenBasicLandType
    );

    if b_changes_types && filter_references_type(&a.affected_filter) {
        return true;
    }

    // If b adds/removes abilities and a's filter checks for abilities
    let b_changes_abilities = matches!(
        &b.modification,
        ContinuousModification::AddKeyword { .. }
            | ContinuousModification::RemoveKeyword { .. }
            | ContinuousModification::RemoveChosenKeyword
            | ContinuousModification::AddDynamicKeyword { .. }
            | ContinuousModification::GrantAbility { .. }
            | ContinuousModification::GrantTrigger { .. }
            | ContinuousModification::RemoveAllAbilities
            | ContinuousModification::AddStaticMode { .. }
            | ContinuousModification::GrantStaticAbility { .. }
            | ContinuousModification::RetainPrintedTriggerFromSource { .. }
            | ContinuousModification::RetainPrintedAbilityFromSource { .. }
    );

    if b_changes_abilities && filter_references_ability(&a.affected_filter) {
        return true;
    }

    // CR 613.8a: b modifies power/toughness and a's filter compares P/T.
    let b_changes_pt = matches!(
        &b.modification,
        ContinuousModification::AddPower { .. }
            | ContinuousModification::AddToughness { .. }
            | ContinuousModification::AddDynamicPower { .. }
            | ContinuousModification::AddDynamicToughness { .. }
            | ContinuousModification::SetPower { .. }
            | ContinuousModification::SetToughness { .. }
            | ContinuousModification::SetPowerDynamic { .. }
            | ContinuousModification::SetToughnessDynamic { .. }
            | ContinuousModification::SetDynamicPower { .. }
            | ContinuousModification::SetDynamicToughness { .. }
    );

    if b_changes_pt && filter_references_pt_stat(&a.affected_filter) {
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
                    | crate::types::ability::FilterProp::CanEnchant { .. }
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

/// Check if a `TargetFilter` compares power or toughness (CR 613.8a dependency axis).
fn filter_references_pt_stat(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => {
            properties.iter().any(filter_prop_references_pt_stat)
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_references_pt_stat)
        }
        TargetFilter::Not { filter } => filter_references_pt_stat(filter),
        _ => false,
    }
}

fn filter_prop_references_pt_stat(prop: &FilterProp) -> bool {
    match prop {
        FilterProp::PtComparison { .. }
        | FilterProp::PowerGTSource
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase => true,
        FilterProp::AnyOf { props } => props.iter().any(filter_prop_references_pt_stat),
        // CR 608.2c: Negation reads the inner prop's stats — recurse (mirrors AnyOf).
        FilterProp::Not { prop } => filter_prop_references_pt_stat(prop),
        _ => false,
    }
}

/// Order effects by timestamp (deterministic fallback). CDAs sort first per CR 604.3.
fn order_by_timestamp(effects: &[&ActiveContinuousEffect]) -> Vec<ActiveContinuousEffect> {
    let mut sorted: Vec<ActiveContinuousEffect> = effects.iter().map(|e| (*e).clone()).collect();
    // CR 613.7: see `order_with_dependencies` — `mod_index` is the
    // written-order tiebreak for equal-timestamp same-source effects.
    sorted.sort_by_key(|e| {
        (
            !e.characteristic_defining,
            e.timestamp,
            e.source_id.0,
            e.def_index,
            e.mod_index,
        )
    });
    sorted
}

/// CR 509.1b + CR 105.4 (issue #327): True when a granted `StaticMode`
/// carries a `FilterProp::IsChosenColor` reference somewhere in its filter,
/// requiring the granting source's chosen color to be resolved at
/// apply-time. See `resolve_static_mode_chosen_color`.
fn static_mode_uses_chosen_color(mode: &crate::types::statics::StaticMode) -> bool {
    use crate::types::statics::StaticMode;
    match mode {
        StaticMode::CantBeBlockedBy { filter } => target_filter_uses_chosen_color(filter),
        _ => false,
    }
}

/// CR 509.1b + CR 105.4 (issue #327): Walk a `TargetFilter` looking for
/// `FilterProp::IsChosenColor`. Mirrors the chosen-ref detection pattern in
/// `effects::prevent_damage::resolve_source_filter`.
fn target_filter_uses_chosen_color(filter: &TargetFilter) -> bool {
    use crate::types::ability::FilterProp;
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::IsChosenColor)),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_uses_chosen_color)
        }
        _ => false,
    }
}

/// CR 509.1b + CR 105.4 + CR 609.6 (issue #327): Resolve every
/// `FilterProp::IsChosenColor` inside the static mode's filter to a concrete
/// `FilterProp::HasColor { color }`, using the granting source's chosen
/// color. When no chosen color is available, the IsChosenColor prop is
/// stripped — leaving an unresolvable predicate on the recipient would make
/// the restriction match every creature.
fn resolve_static_mode_chosen_color(
    mode: &crate::types::statics::StaticMode,
    chosen_color: Option<crate::types::mana::ManaColor>,
) -> crate::types::statics::StaticMode {
    use crate::types::statics::StaticMode;
    match mode {
        StaticMode::CantBeBlockedBy { filter } => StaticMode::CantBeBlockedBy {
            filter: resolve_chosen_color_in_filter(filter, chosen_color),
        },
        other => other.clone(),
    }
}

/// CR 105.4 + CR 609.6 (issue #327): Walk a `TargetFilter` and replace every
/// `FilterProp::IsChosenColor` with a concrete `FilterProp::HasColor` keyed
/// to the supplied chosen color. Mirrors
/// `effects::prevent_damage::resolve_source_filter`.
fn resolve_chosen_color_in_filter(
    filter: &TargetFilter,
    chosen_color: Option<crate::types::mana::ManaColor>,
) -> TargetFilter {
    use crate::types::ability::FilterProp;
    match filter {
        TargetFilter::Typed(tf) => {
            let mut resolved = tf.clone();
            resolved
                .properties
                .retain(|p| !matches!(p, FilterProp::IsChosenColor));
            if let Some(color) = chosen_color {
                resolved.properties.push(FilterProp::HasColor { color });
            }
            TargetFilter::Typed(resolved)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .iter()
                .map(|f| resolve_chosen_color_in_filter(f, chosen_color))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .iter()
                .map(|f| resolve_chosen_color_in_filter(f, chosen_color))
                .collect(),
        },
        other => other.clone(),
    }
}

/// Apply a single continuous effect to all affected objects.
///
/// CR 400.3 + CR 702.94a: The filter's `InZone` property (via
/// `TargetFilter::extract_in_zone`) selects which zone's objects are scanned.
/// Absence of `InZone` defaults to the battlefield (current behavior). This
/// supports non-battlefield grant statics like Lorehold's "Each instant and
/// sorcery card in your hand has miracle {2}", whose filter carries
/// `InZone { zone: Hand }`.
/// Derive the source-attribution reference for an active continuous effect.
///
/// Returns `None` only when the effect has neither a static `def_index` nor a
/// `transient_id` — which shouldn't happen for any path that constructs an
/// `ActiveContinuousEffect` (both gather sites populate one of the two).
fn effect_ref_for(effect: &ActiveContinuousEffect) -> Option<EffectRef> {
    if let Some(id) = effect.transient_id {
        return Some(EffectRef::Transient {
            id,
            mod_index: effect.mod_index,
        });
    }
    effect.def_index.map(|def_index| EffectRef::Static {
        source: effect.source_id,
        def_index,
        mod_index: effect.mod_index,
    })
}

/// Append a per-(target × layer) attribution entry for each affected object.
///
/// `EffectRef` is `Copy` (a small POD), and the referenced
/// `ContinuousModification` / source-name lives in canonical state
/// (`static_definitions` or `transient_continuous_effects`), so this records
/// no copies of the modification itself.
fn record_attribution(
    state: &mut GameState,
    effect: &ActiveContinuousEffect,
    affected_ids: &[ObjectId],
) {
    let Some(effect_ref) = effect_ref_for(effect) else {
        return;
    };
    for &target in affected_ids {
        let attribution = state.attribution.entry(target).or_default();
        attribution.record_layer(effect.layer, effect_ref);
    }
}

fn apply_continuous_effect(
    state: &mut GameState,
    effect: &ActiveContinuousEffect,
    abilities_suppressed: &mut HashSet<ObjectId>,
) {
    apply_continuous_effect_filtered(state, effect, None, abilities_suppressed);
}

/// Apply a continuous effect's modification only to the subset of its affected
/// objects that are in `restrict_to`. Used by the incremental layer-flush fast
/// path so a pre-existing anthem/static re-applies to a freshly-entered object
/// without re-applying to (and thus without needing to reset) the rest of the
/// battlefield. Shares the entire apply body with `apply_continuous_effect` —
/// no duplicated per-recipient logic.
/// CR 613.1: Applies continuous effects through the layer system to the
/// restricted recipient set.
fn apply_continuous_effect_to(
    state: &mut GameState,
    effect: &ActiveContinuousEffect,
    restrict_to: &HashSet<ObjectId>,
    abilities_suppressed: &mut HashSet<ObjectId>,
) {
    apply_continuous_effect_filtered(state, effect, Some(restrict_to), abilities_suppressed);
}

fn apply_continuous_effect_filtered(
    state: &mut GameState,
    effect: &ActiveContinuousEffect,
    restrict_to: Option<&HashSet<ObjectId>>,
    abilities_suppressed: &mut HashSet<ObjectId>,
) {
    // CR 613.1f: A printed static on an object that lost all abilities this
    // pass must not re-apply in later layers (Death's Shadow CDA after
    // Abigale — issue #1321).
    if effect.def_index.is_some() && abilities_suppressed.contains(&effect.source_id) {
        return;
    }

    let scan_zone = effect
        .affected_filter
        .extract_in_zone()
        .unwrap_or(crate::types::zones::Zone::Battlefield);
    let scan_ids = super::targeting::zone_object_ids(state, scan_zone);
    let ctx = FilterContext::from_source(state, effect.source_id);
    let affected_ids: Vec<ObjectId> = scan_ids
        .iter()
        // Incremental fast path: re-apply only to the freshly-entered objects.
        // The rest of the battlefield was not reset and keeps its prior derived
        // values, so re-applying to it would double-apply.
        .filter(|&&id| restrict_to.is_none_or(|ids| ids.contains(&id)))
        .filter(|&&id| matches_target_filter(state, id, &effect.affected_filter, &ctx))
        .filter(|&&id| {
            effect.condition.as_ref().is_none_or(|condition| {
                evaluate_condition_with_recipient(
                    state,
                    condition,
                    effect.controller,
                    effect.source_id,
                    id,
                )
            })
        })
        .copied()
        .collect();

    record_attribution(state, effect, &affected_ids);

    // Pre-read chosen subtype from source (avoids borrow conflict in the loop).
    // Populated for `AddChosenSubtype { kind }` (additive — creature type or
    // basic land type) AND for `SetChosenBasicLandType` (CR 305.7 replacement
    // of a land's subtype with the source's chosen basic land type). The latter
    // is implicitly `ChosenSubtypeKind::BasicLandType`.
    let chosen_subtype_kind = match effect.modification {
        ContinuousModification::AddChosenSubtype { ref kind } => Some(kind),
        ContinuousModification::SetChosenBasicLandType => Some(&ChosenSubtypeKind::BasicLandType),
        _ => None,
    };
    let chosen_subtype = chosen_subtype_kind.and_then(|kind| {
        state
            .objects
            .get(&effect.source_id)
            .and_then(|src| src.chosen_subtype_str(kind))
    });

    // Pre-read chosen color from source (avoids borrow conflict in the loop).
    // Used by `AddChosenColor` (CR 105.3) AND by `AddKeyword` when the keyword
    // is `HexproofFrom(ChosenColor)` / `Protection(ChosenColor)` AND by
    // `AddStaticMode` when the static mode carries an `IsChosenColor` filter
    // prop — CR 702.11d + CR 702.16 + CR 509.1b + CR 105.4 + CR 609.6: the
    // granting source's chosen color must be baked into the granted modifier
    // at apply-time, because the modifier lives on the granted creature
    // (which has no chosen-color attribute of its own).
    let chosen_color = if matches!(effect.modification, ContinuousModification::AddChosenColor)
        || matches!(
            &effect.modification,
            ContinuousModification::AddKeyword { keyword }
                if matches!(
                    keyword,
                    crate::types::keywords::Keyword::HexproofFrom(
                        crate::types::keywords::HexproofFilter::ChosenColor,
                    ) | crate::types::keywords::Keyword::Protection(
                        crate::types::keywords::ProtectionTarget::ChosenColor,
                    )
                )
        )
        || matches!(
            &effect.modification,
            ContinuousModification::AddStaticMode { mode }
                if static_mode_uses_chosen_color(mode)
        ) {
        state
            .objects
            .get(&effect.source_id)
            .and_then(|src| src.chosen_color())
    } else {
        None
    };

    // Pre-read chosen keyword from source (avoids borrow conflict in the loop).
    // CR 608.2d + CR 613.1f: When the modification is `RemoveChosenKeyword`,
    // the granting source's `chosen_attributes` carry the typed `Keyword` that
    // was selected at resolution time (Urborg / Walking Sponge). Read once
    // here so the per-recipient loop below can strip by discriminant without
    // re-borrowing `state` for every affected object — mirrors the
    // `chosen_color` / `chosen_subtype` / `chosen_card_type` pre-read blocks
    // immediately above and below.
    //
    // Caveat (mirrors `chosen_color` semantics): if the same source has
    // multiple concurrent `RemoveChosenKeyword` effects (e.g., Urborg
    // activated twice in the same turn), each currently reads the FIRST
    // `ChosenAttribute::Keyword` on the source. Same limitation applies to
    // `chosen_color` / `chosen_card_type` upstream; documented here for
    // symmetry. Acceptable for v1 — fix paired with the broader
    // chosen-attribute scoping refactor.
    let chosen_keyword = if matches!(
        effect.modification,
        ContinuousModification::RemoveChosenKeyword | ContinuousModification::AddChosenKeyword
    ) {
        state
            .objects
            .get(&effect.source_id)
            .and_then(|src| src.chosen_keyword().cloned())
    } else {
        None
    };

    // Pre-read chosen card type from source (avoids borrow conflict in the loop).
    // CR 702.16 + CR 205.2: when the granted keyword is
    // `Protection(ChosenCardType)`, the granting source's chosen card type must
    // be baked into the granted modifier at apply-time — the modifier lives on
    // the granted creature, which has no chosen-card-type attribute of its own.
    let chosen_card_type = if matches!(
        &effect.modification,
        ContinuousModification::AddKeyword { keyword }
            if matches!(
                keyword,
                crate::types::keywords::Keyword::Protection(
                    crate::types::keywords::ProtectionTarget::ChosenCardType,
                )
            )
    ) {
        state
            .objects
            .get(&effect.source_id)
            .and_then(|src| src.chosen_card_type())
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
    // referent. Recipient-relative quantities ("attached to it", "other",
    // "shares a type with it") need the affected object bound before
    // resolution, so those defer into the per-recipient loop below.
    let dynamic_pt_expr = continuous_modification_dynamic_quantity(&effect.modification);
    let effect_controller = state
        .objects
        .get(&effect.source_id)
        .map(|o| o.controller)
        .unwrap_or(PlayerId(0));
    let dynamic_uses_recipient =
        dynamic_pt_expr.is_some_and(crate::game::quantity::quantity_expr_uses_recipient);
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
    // CR 707.9a: Pre-read the printed activated ability to retain from the
    // source object's `base_abilities`. Cloned before the per-object mutable
    // borrow inside the loop (mirrors the trigger retain pre-read above).
    let retained_printed_ability = if let ContinuousModification::RetainPrintedAbilityFromSource {
        source_ability_index,
    } = &effect.modification
    {
        state
            .objects
            .get(&effect.source_id)
            .and_then(|src| src.base_abilities.get(*source_ability_index).cloned())
    } else {
        None
    };
    let all_creature_types = state.all_creature_types.clone();

    for id in affected_ids {
        // CR 613.4c: When the dynamic modification's QuantityExpr depends on
        // the recipient, resolve here under a recipient-bound FilterContext.
        // The immutable read finishes before the mutable borrow of `obj` below.
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
            ContinuousModification::CopyValues {
                values,
                display_source,
                printed_ref,
                token_image_ref,
            } => {
                apply_copiable_values(obj, values);
                // Display routing follows the copy: override the baseline
                // restored by the layer reset so the copy renders the source's
                // art. Reverts automatically when the copy effect expires.
                // CR 111.1 + CR 707.2: for a copy of a true token, the source's
                // `display_source = Token` + `token_image_ref` carry the art (the
                // copied name has no real-card printing); for a printed source,
                // `display_source = Card` + `printed_ref`. None are copiable
                // values — purely display routing.
                obj.display_source = *display_source;
                obj.printed_ref = printed_ref.clone();
                obj.token_image_ref = token_image_ref.clone();
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
                // CR 702.11d + CR 702.16 + CR 609.6: When the granted keyword
                // refers to "the chosen color" of the granting source, resolve
                // it to the concrete color before push so the keyword is
                // self-contained on the recipient. `chosen_color` is pre-read
                // above when the keyword's variant requires it.
                let resolved_keyword = match keyword {
                    crate::types::keywords::Keyword::HexproofFrom(
                        crate::types::keywords::HexproofFilter::ChosenColor,
                    ) => {
                        if let Some(color) = chosen_color {
                            crate::types::keywords::Keyword::HexproofFrom(
                                crate::types::keywords::HexproofFilter::Color(color),
                            )
                        } else {
                            // No chosen color yet — skip the grant rather than
                            // pushing an unresolvable variant.
                            continue;
                        }
                    }
                    crate::types::keywords::Keyword::Protection(
                        crate::types::keywords::ProtectionTarget::ChosenColor,
                    ) => {
                        if let Some(color) = chosen_color {
                            crate::types::keywords::Keyword::Protection(
                                crate::types::keywords::ProtectionTarget::Color(color),
                            )
                        } else {
                            continue;
                        }
                    }
                    // CR 702.16 + CR 205.2: "protection from the
                    // chosen card type" — resolve to a concrete
                    // `Protection(CardType("creature"))` from the granting
                    // source's chosen card type, so the keyword is
                    // self-contained on the recipient. `source_matches_card_type`
                    // then enforces CR 702.16b/d/e/f. Skip the grant if the
                    // chosen card type is unresolved or has no protection noun.
                    crate::types::keywords::Keyword::Protection(
                        crate::types::keywords::ProtectionTarget::ChosenCardType,
                    ) => match chosen_card_type.and_then(|ct| ct.protection_quality_str()) {
                        Some(quality) => crate::types::keywords::Keyword::Protection(
                            crate::types::keywords::ProtectionTarget::CardType(quality.to_string()),
                        ),
                        None => continue,
                    },
                    other => other.clone(),
                };
                // Three-way grant policy:
                // CR 702.164b: summing keywords (Toxic) accumulate even when an
                // identical instance is already present (granted Toxic 1 on
                // printed Toxic 1 -> total 2).
                // CR 613.7: single-authoritative-value keywords (Crew/Saddle/Enchant,
                // see `overrides_same_kind_on_grant`) replace any earlier same-kind
                // instance so a "first match" reader never sees a stale value.
                // All other parameterized keywords keep deduping identical instances
                // per CR 702.16g (Protection A+B are separate abilities;
                // Ward/Annihilator each apply independently). `evaluate_layers` resets
                // `obj.keywords = obj.base_keywords.clone()` each pass, so this never
                // accumulates unbounded across re-evaluations.
                if resolved_keyword.sums_across_instances() {
                    obj.keywords.push(resolved_keyword.clone());
                } else if resolved_keyword.overrides_same_kind_on_grant() {
                    // CR 613.7: this grant is a single-authoritative-value keyword — the
                    // most recently applied instance (this one, since modifications are
                    // applied in ascending timestamp order, see `order_by_timestamp`)
                    // replaces any earlier same-discriminant instance (printed or a prior
                    // grant) rather than coexisting with it. Mirrors `RemoveKeyword`'s
                    // discriminant technique below, but replaces instead of strips.
                    obj.keywords.retain(|k| {
                        std::mem::discriminant(k) != std::mem::discriminant(&resolved_keyword)
                    });
                    obj.keywords.push(resolved_keyword.clone());
                } else if !obj.keywords.contains(&resolved_keyword) {
                    obj.keywords.push(resolved_keyword.clone());
                }
                for trigger in KeywordTriggerInstaller::triggers_for(&resolved_keyword) {
                    obj.trigger_definitions.push(trigger);
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
                obj.trigger_definitions.retain(|trigger| {
                    !KeywordTriggerInstaller::trigger_matches_keyword_kind(trigger, keyword)
                });
            }
            // CR 608.2d + CR 613.1f + CR 702.14: Strip the *exact* keyword
            // chosen at resolution time (read off the source's
            // `chosen_attributes` above). Unlike the unparameterized
            // `RemoveKeyword` arm, the chosen-keyword surface is concrete —
            // `ChosenAttribute::Keyword(Landwalk("Swamp"))` is exactly
            // swampwalk, not "any landwalk." CR 702.14 treats each landwalk
            // subtype as a distinct keyword, so removing swampwalk must
            // leave islandwalk intact. Use `PartialEq` (`k == kw`) rather
            // than `std::mem::discriminant` to preserve that distinction.
            // Triggers associated with the keyword kind (e.g. lifelink's
            // lifegain hook) are still removed by `KeywordKind`, which is
            // the granularity at which keyword-derived triggers are
            // installed by `KeywordTriggerInstaller`. If no keyword is
            // currently stored on the source (e.g. the static is gathered
            // before the choose effect has resolved), this is a no-op
            // rather than a panic — mirrors the unresolved-attribute
            // behavior of `AddChosenColor`.
            ContinuousModification::RemoveChosenKeyword => {
                if let Some(kw) = chosen_keyword.as_ref() {
                    obj.keywords.retain(|k| k != kw);
                    obj.trigger_definitions.retain(|trigger| {
                        !KeywordTriggerInstaller::trigger_matches_keyword_kind(trigger, kw)
                    });
                }
            }
            // CR 608.2d + CR 613.1f: Grant the *exact* keyword chosen at
            // resolution time (read off the source's `chosen_attributes`
            // above). The additive mirror of `RemoveChosenKeyword` — installs
            // the keyword and its keyword-derived triggers (e.g. lifelink's
            // lifegain hook) onto each recipient, matching the plain
            // `AddKeyword` arm. Used by "choose [keyword]; creatures you
            // control gain that ability until end of turn" (Angelic
            // Skirmisher, Linvala, Shield of Sea Gate). If the source has no
            // stored chosen keyword (e.g. the static is gathered before the
            // choose effect has resolved), this is a no-op rather than a panic,
            // mirroring `AddChosenColor` / `RemoveChosenKeyword`.
            ContinuousModification::AddChosenKeyword => {
                if let Some(kw) = chosen_keyword.as_ref() {
                    // CR 702.164b: summing keywords (Toxic) accumulate rather
                    // than dedup, mirroring the plain `AddKeyword` arm above.
                    if kw.sums_across_instances() || !obj.keywords.contains(kw) {
                        obj.keywords.push(kw.clone());
                    }
                    for trigger in KeywordTriggerInstaller::triggers_for(kw) {
                        obj.trigger_definitions.push(trigger);
                    }
                }
            }
            ContinuousModification::RemoveAllAbilities => {
                Arc::make_mut(&mut obj.abilities).clear();
                obj.trigger_definitions.clear();
                obj.replacement_definitions.clear();
                obj.static_definitions.clear();
                obj.keywords.clear();
                abilities_suppressed.insert(id);
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
            }
            ContinuousModification::RemoveSubtype { ref subtype } => {
                obj.card_types.subtypes.retain(|s| s != subtype);
            }
            // CR 205.1a + CR 613.1d: Replace the entire core card-type set.
            ContinuousModification::SetCardTypes { ref core_types } => {
                obj.card_types.core_types = core_types.clone();
                obj.card_types.subtypes.retain(|subtype| {
                    subtype_matches_core_types(subtype, core_types, &all_creature_types)
                });
            }
            // CR 205.1a + CR 613.1d: Remove every subtype belonging to the
            // named subtype set. Membership for the `Creature` set is resolved
            // against the runtime-populated `state.all_creature_types` — the
            // same source `AddAllCreatureTypes` uses below.
            ContinuousModification::RemoveAllSubtypes { set } => {
                match set {
                    SubtypeSet::Creature => {
                        obj.card_types
                            .subtypes
                            .retain(|s| !all_creature_types.iter().any(|c| c == s));
                    }
                    SubtypeSet::Land => {
                        // CR 205.3i: land-type membership via the basic/non-basic
                        // land-subtype classification.
                        obj.card_types.subtypes.retain(|s| !is_land_subtype(s));
                    }
                    SubtypeSet::Artifact
                    | SubtypeSet::Enchantment
                    | SubtypeSet::Planeswalker
                    | SubtypeSet::Spell
                    | SubtypeSet::Battle => {
                        obj.card_types
                            .subtypes
                            .retain(|s| noncreature_subtype_set(s) != Some(*set));
                    }
                }
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
            // CR 707.9b + CR 306.5b/c: Starting-loyalty exceptions are folded
            // into copied values at copy resolution, before loyalty counters
            // are seeded. Reaching the layer system means a resolver forgot to
            // consume this one-shot copy exception.
            ContinuousModification::SetStartingLoyalty { .. } => {
                debug_assert!(
                    false,
                    "SetStartingLoyalty must be consumed at copy resolution time, \
                     not via apply_continuous_effect"
                );
            }
            // CR 707.9 + CR 202.1b: the "has no mana cost" copy exception is
            // consumed at copy resolution (token_copy.rs bakes it into the token;
            // become_copy.rs strips it from the copied values), never via a
            // continuous effect. Reaching this arm means a wiring bug.
            ContinuousModification::RemoveManaCost => {
                debug_assert!(
                    false,
                    "RemoveManaCost must be consumed at copy resolution time, \
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
                        obj.card_types.subtypes.push(subtype);
                    }
                }
            }
            // CR 205.3i: every land type is one of the 17 land subtypes.
            // CR 305.7: a land that gains land types in addition to its own
            // keeps its types and gains the new land types and their mana
            // abilities. The basic types among the 17 grant their mana ability
            // automatically via `apply_intrinsic_basic_land_mana_abilities`.
            ContinuousModification::AddAllLandTypes => {
                for subtype in crate::types::card_type::LAND_SUBTYPES {
                    if !obj.card_types.subtypes.iter().any(|s| s == subtype) {
                        obj.card_types.subtypes.push((*subtype).to_string());
                    }
                }
            }
            ContinuousModification::AddChosenSubtype { .. } => {
                if let Some(ref subtype) = chosen_subtype {
                    if !obj.card_types.subtypes.iter().any(|s| s == subtype) {
                        obj.card_types.subtypes.push(subtype.clone());
                    }
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
                        obj.keywords.push(keyword.clone());
                    }
                    for trigger in KeywordTriggerInstaller::triggers_for(&keyword) {
                        obj.trigger_definitions.push(trigger);
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
            // CR 613.1f: Handled entirely at continuous-effect collection time —
            // `active_continuous_effects_from_static_definitions` expands this into
            // one `GrantAbility` effect per matching activated ability (it needs
            // read access to the provider objects, which the per-object apply
            // borrow cannot give). No direct per-object mutation here.
            ContinuousModification::GrantAllActivatedAbilitiesOf { .. } => {}
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
            // CR 113.3d + CR 604.1 + CR 613.1f: Grant a full static ability to the
            // recipient. The inner static's `affected`/`condition`/`modifications`
            // are independent of the recipient (e.g. "Other commanders you control
            // get +2/+2 and have lifelink") and are preserved verbatim, so the
            // granted static operates against its own scope under CR 611.2c once
            // it's installed on the recipient's `static_definitions`. Dedup by
            // structural equality so repeated layer passes don't multiply the
            // grant (mirrors the `GrantAbility` / `GrantTrigger` / `AddStaticMode`
            // idempotency invariant in this match).
            ContinuousModification::GrantStaticAbility { definition } => {
                if !obj
                    .static_definitions
                    .iter_all()
                    .any(|sd| sd == definition.as_ref())
                {
                    obj.static_definitions.push(*definition.clone());
                }
            }
            ContinuousModification::AddStaticMode { mode } => {
                // CR 509.1b + CR 105.4 + CR 609.6 (issue #327): When the
                // granted static mode carries an `IsChosenColor` filter prop,
                // resolve it to a concrete `HasColor(<chosen>)` using the
                // granting source's chosen color. The static_def is anchored
                // to the recipient (`affected: SelfRef`) which has no
                // chosen-color attribute of its own; resolving at apply time
                // bakes the granting source's choice into the live filter.
                let resolved_mode = resolve_static_mode_chosen_color(mode, chosen_color);
                let def =
                    StaticDefinition::new(resolved_mode.clone()).affected(TargetFilter::SelfRef);
                if !obj
                    .static_definitions
                    .iter_all()
                    .any(|sd| sd.mode == resolved_mode)
                {
                    obj.static_definitions.push(def);
                }
            }
            // CR 613.4d: Switch power and toughness values.
            ContinuousModification::SwitchPowerToughness => {
                let (p, t) = (obj.power, obj.toughness);
                obj.power = t;
                obj.toughness = p;
            }
            ContinuousModification::AssignDamageFromToughness
            | ContinuousModification::AssignDamageAsThoughUnblocked
            | ContinuousModification::AssignNoCombatDamage => unreachable!(
                "combat-damage assignment rule modifications are applied after layer evaluation"
            ),
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
            // CR 613.1f: mirror RemoveAllAbilities — suppress the recipient's
            // own printed continuous statics for the rest of this pass so layer
            // 6 cannot re-install rules-text abilities the subtype change removed
            // (Song of the Dryads class — issue #3279).
            ContinuousModification::SetBasicLandType { land_type } => {
                set_land_subtype_replacing(obj, land_type.as_subtype_str().to_string());
                abilities_suppressed.insert(id);
            }
            // CR 305.7 + CR 305.6: Set the land's subtype to the basic land type
            // chosen by the granting source (Phantasmal Terrain, Convincing
            // Mirage). Identical replacement semantics to `SetBasicLandType` —
            // remove old land subtypes (CR 205.3i), clear abilities/triggers/
            // replacements/statics/keywords generated from rules text — except
            // the concrete subtype is the source's pre-read `chosen_subtype`
            // (read above for `BasicLandType`). The intrinsic mana ability is
            // derived from the subtype in `mana_sources.rs` (CR 305.6). If no
            // choice was recorded, this is a no-op.
            ContinuousModification::SetChosenBasicLandType => {
                if let Some(ref subtype) = chosen_subtype {
                    set_land_subtype_replacing(obj, subtype.clone());
                    abilities_suppressed.insert(id);
                }
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
            // CR 707.9a: Retain the source's printed activated ability on the
            // copy. After `CopyValues` overwrote `obj.abilities` with the
            // copied values, push the source's printed ability back so the
            // copy retains "this ability". Idempotent — duplicate retain calls
            // (same ability structurally) collapse into one.
            ContinuousModification::RetainPrintedAbilityFromSource { .. } => {
                if let Some(ability) = retained_printed_ability.clone() {
                    if !obj.abilities.iter().any(|a| a == &ability) {
                        Arc::make_mut(&mut obj.abilities).push(ability);
                    }
                }
            }
        }
    }
}

// CR 305.7: Setting a land subtype replaces old land subtypes and removes the
// land's rules-text abilities; layer 6 reapplies abilities from other effects.
fn set_land_subtype_replacing(obj: &mut crate::game::game_object::GameObject, subtype: String) {
    obj.card_types.subtypes.retain(|s| !is_land_subtype(s));
    obj.card_types.subtypes.push(subtype);
    Arc::make_mut(&mut obj.abilities).clear();
    obj.trigger_definitions.clear();
    obj.replacement_definitions.clear();
    obj.static_definitions.clear();
    obj.keywords.clear();
}

/// CR 305.6: After layer 4 establishes final land types, derive each land's
/// intrinsic basic-land mana abilities before layer 6 ability effects apply.
fn apply_intrinsic_basic_land_mana_abilities(state: &mut GameState, battlefield_ids: &[ObjectId]) {
    for &id in battlefield_ids {
        let Some(obj) = state.objects.get_mut(&id) else {
            continue;
        };
        if !obj.card_types.core_types.contains(&CoreType::Land) {
            continue;
        }

        let land_types: Vec<BasicLandType> = obj
            .card_types
            .subtypes
            .iter()
            .filter_map(|subtype| subtype.parse().ok())
            .collect();
        for land_type in land_types {
            add_basic_land_mana_ability(obj, land_type);
        }
    }
}

fn add_basic_land_mana_ability(
    obj: &mut crate::game::game_object::GameObject,
    land_type: BasicLandType,
) {
    let color = land_type.mana_color();
    if has_basic_land_mana_ability(obj, color) {
        return;
    }

    Arc::make_mut(&mut obj.abilities).push(basic_land_mana_ability(color));
}

fn has_basic_land_mana_ability(
    obj: &crate::game::game_object::GameObject,
    color: crate::types::mana::ManaColor,
) -> bool {
    obj.abilities.iter().any(|ability| {
        ability.kind == AbilityKind::Activated
            && matches!(ability.cost, Some(AbilityCost::Tap))
            && matches!(
                &*ability.effect,
                Effect::Mana {
                    produced: ManaProduction::Fixed { colors, .. },
                    ..
                } if colors.as_slice() == [color]
            )
    })
}

fn basic_land_mana_ability(color: crate::types::mana::ManaColor) -> AbilityDefinition {
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
    .cost(AbilityCost::Tap)
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
            .filter(|effect| {
                effect.condition.as_ref().is_none_or(|condition| {
                    evaluate_condition_with_recipient(
                        state,
                        condition,
                        effect.controller,
                        effect.source_id,
                        object_id,
                    )
                })
            })
            .collect();
    copy_effects = order_active_continuous_effects(Layer::Copy, &copy_effects, state);
    for effect in &copy_effects {
        match &effect.modification {
            ContinuousModification::CopyValues {
                values: effect_values,
                ..
            } => {
                values = (**effect_values).clone();
                for trigger in state
                    .transient_continuous_effects
                    .iter()
                    .filter(|tce| {
                        tce.source_id == effect.source_id
                            && tce.timestamp == effect.timestamp
                            && tce.affected == effect.affected_filter
                    })
                    .flat_map(|tce| &tce.modifications)
                    .filter_map(|modification| match modification {
                        ContinuousModification::GrantTrigger { trigger } => Some(trigger),
                        _ => None,
                    })
                {
                    let triggers = Arc::make_mut(&mut values.trigger_definitions);
                    if !triggers.iter().any(|t| t == trigger.as_ref()) {
                        triggers.push(*trigger.clone());
                    }
                }
            }
            // CR 707.9b: Name overrides from "except its name is X" clauses
            // become part of the copiable values of the copy. A subsequent
            // copy of this object must see the overridden name, not the
            // source's name.
            ContinuousModification::SetName { name } => {
                values.name = name.clone();
            }
            // CR 707.9b + CR 306.5b: Starting loyalty is a copy-effect
            // characteristic exception. A later copy of this copy must see
            // the overridden loyalty value.
            ContinuousModification::SetStartingLoyalty { value } => {
                values.loyalty = Some(*value);
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
            // CR 707.9a: A copy effect that retains an activated ability makes
            // that ability part of the copiable values of the copy. Read the
            // printed ability from the effect's source object by index,
            // mirroring the trigger retain path above.
            ContinuousModification::RetainPrintedAbilityFromSource {
                source_ability_index,
            } => {
                if let Some(ability) = state
                    .objects
                    .get(&effect.source_id)
                    .and_then(|src| src.base_abilities.get(*source_ability_index).cloned())
                {
                    let abilities = Arc::make_mut(&mut values.abilities);
                    if !abilities.iter().any(|a| a == &ability) {
                        abilities.push(ability);
                    }
                }
            }
            _ => {}
        }
    }
    // CR 707.2: Copies must receive synthesized keyword companion triggers when
    // the copiable snapshot carries the keyword but omits its dies trigger.
    ensure_keyword_triggers_for_copiable_values(&mut values);
    Some(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::GameScenario;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, BasicLandType, CastVariantPaid,
        ChosenSubtypeKind, CommanderOwnership, Comparator, ContinuousModification, ControllerRef,
        CountScope, Duration, Effect, FilterProp, ObjectScope, PlayerFilter, PlayerScope, PtStat,
        PtValueScope, QuantityExpr, QuantityRef, SacrificeCost, StaticCondition, StaticDefinition,
        TargetFilter, TriggerCondition, TypeFilter, TypedFilter, ZoneRef,
    };
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::counter::{CounterMatch, CounterType};
    use crate::types::game_state::{StaticSourceIndex, TransientContinuousEffect};
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;
    use std::sync::Arc;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    #[test]
    fn set_card_types_prunes_subtypes_not_matching_new_core_types() {
        let mut state = setup();
        state.all_creature_types = vec!["Bear".to_string(), "Berserker".to_string()];

        assert!(subtype_matches_core_types(
            "Bear",
            &[CoreType::Creature],
            &state.all_creature_types
        ));
        assert!(!subtype_matches_core_types(
            "Equipment",
            &[CoreType::Creature],
            &state.all_creature_types
        ));
        assert!(!subtype_matches_core_types(
            "Mountain",
            &[CoreType::Creature],
            &state.all_creature_types
        ));
        assert!(subtype_matches_core_types(
            "Equipment",
            &[CoreType::Artifact, CoreType::Creature],
            &state.all_creature_types
        ));
        assert!(subtype_matches_core_types(
            "Siege",
            &[CoreType::Battle],
            &state.all_creature_types
        ));
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

    #[test]
    fn prototyped_permanent_keeps_secondary_characteristics_after_type_change() {
        let mut state = setup();
        let prototype = make_creature(&mut state, "Combat Thresher", 3, 3, PlayerId(0));
        {
            let obj = state.objects.get_mut(&prototype).unwrap();
            obj.card_types.core_types.insert(0, CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
            obj.mana_cost = ManaCost::Cost {
                shards: vec![],
                generic: 7,
            };
            obj.base_mana_cost = obj.mana_cost.clone();
            obj.base_color.clear();
            obj.color.clear();
            obj.prototype_form = Some(crate::game::game_object::PrototypeFormState {
                mana_cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::White],
                    generic: 2,
                },
                power: 1,
                toughness: 1,
                colors: vec![ManaColor::White],
            });
        }

        evaluate_layers(&mut state);
        let as_creature = state.objects.get(&prototype).unwrap();
        assert_eq!(as_creature.mana_cost.mana_value(), 3);
        assert_eq!(as_creature.power, Some(1));
        assert_eq!(as_creature.toughness, Some(1));
        assert_eq!(as_creature.color, vec![ManaColor::White]);

        let type_changer = create_object(
            &mut state,
            CardId(161),
            PlayerId(0),
            "Prototype Shell".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&type_changer).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: prototype })
                    .modifications(vec![ContinuousModification::SetCardTypes {
                        core_types: vec![CoreType::Artifact],
                    }]),
            );
        }

        evaluate_layers(&mut state);
        let noncreature = state.objects.get(&prototype).unwrap();
        assert_eq!(noncreature.mana_cost.mana_value(), 3);
        assert_eq!(noncreature.power, Some(1));
        assert_eq!(noncreature.toughness, Some(1));
        assert_eq!(noncreature.color, vec![ManaColor::White]);
        assert!(!noncreature
            .card_types
            .core_types
            .contains(&CoreType::Creature));
    }

    /// Places a battlefield commander object with the given owner/controller.
    fn make_commander(state: &mut GameState, owner: PlayerId, controller: PlayerId) -> ObjectId {
        let id = make_creature(state, "Test Commander", 3, 3, owner);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.is_commander = true;
        obj.controller = controller;
        id
    }

    /// CR 903.3 + CR 109.5: Lieutenant ("you control your commander") is satisfied
    /// when a commander you own is on the battlefield under your control.
    #[test]
    fn lieutenant_satisfied_by_own_controlled_commander() {
        let mut state = setup();
        let demon = make_creature(&mut state, "Demon", 4, 4, PlayerId(0));
        make_commander(&mut state, PlayerId(0), PlayerId(0));
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::ControlsCommander {
                ownership: CommanderOwnership::Own,
            },
            PlayerId(0),
            demon,
        ));
    }

    /// CR 903.3 + CR 109.5: THE bug — controlling a STOLEN opponent's commander
    /// does NOT satisfy the Lieutenant "your commander" condition. Revert-
    /// discriminating: pre-fix controller-only code returns `true`.
    #[test]
    fn lieutenant_not_satisfied_by_stolen_opponent_commander() {
        let mut state = setup();
        let demon = make_creature(&mut state, "Demon", 4, 4, PlayerId(0));
        // Opponent (P1) owns the commander; you (P0) have gained control of it.
        make_commander(&mut state, PlayerId(1), PlayerId(0));
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::ControlsCommander {
                ownership: CommanderOwnership::Own,
            },
            PlayerId(0),
            demon,
        ));
    }

    /// CR 903.3 + CR 109.5: the controller half is still required — your own
    /// commander controlled by an opponent does NOT satisfy Lieutenant.
    #[test]
    fn lieutenant_not_satisfied_when_own_commander_controlled_by_opponent() {
        let mut state = setup();
        let demon = make_creature(&mut state, "Demon", 4, 4, PlayerId(0));
        make_commander(&mut state, PlayerId(0), PlayerId(1));
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::ControlsCommander {
                ownership: CommanderOwnership::Own,
            },
            PlayerId(0),
            demon,
        ));
    }

    /// CR 903.3d: the generic "you control a commander" condition STILL counts a
    /// stolen opponent's commander. Regression guard against the parameterization
    /// silently inheriting the `Own` predicate.
    #[test]
    fn generic_control_a_commander_counts_stolen_opponent_commander() {
        let mut state = setup();
        let src = make_creature(&mut state, "Source", 1, 1, PlayerId(0));
        make_commander(&mut state, PlayerId(1), PlayerId(0));
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::ControlsCommander {
                ownership: CommanderOwnership::Any,
            },
            PlayerId(0),
            src,
        ));
    }

    /// Cavernous Maw (std BATCH 12): the `{2}` animation turns the land into a
    /// 3/3 Elemental creature while "It's still a Cave land" (CR 205.1b, CR 305.7)
    /// retains the Land card type and Cave subtype. This drives the parsed
    /// modification set through the real layer engine and asserts the composition
    /// is runtime-sound: the animated permanent is a 3/3 Creature that is STILL a
    /// Cave Land (CR 613.1d Layer 4 additive type/subtype ordering). The
    /// animation's `RemoveAllSubtypes { Creature }` (CR 205.1a) wipes only the
    /// creature-type set, so the Cave land subtype survives.
    #[test]
    fn cavernous_maw_animation_retains_cave_land_through_layers() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Cavernous Maw".to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Cave".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
        }

        // The full `{2}` activated-ability animation as parsed: the
        // "becomes a 3/3 Elemental creature" modifications followed by the
        // "It's still a Cave land" retention re-assertions.
        let animation = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![
                ContinuousModification::SetPower { value: 3 },
                ContinuousModification::SetToughness { value: 3 },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
                ContinuousModification::RemoveAllSubtypes {
                    set: crate::types::card_type::SubtypeSet::Creature,
                },
                ContinuousModification::AddSubtype {
                    subtype: "Elemental".to_string(),
                },
                // "It's still a Cave land" — the clause this batch unblocks.
                ContinuousModification::AddType {
                    core_type: CoreType::Land,
                },
                ContinuousModification::AddSubtype {
                    subtype: "Cave".to_string(),
                },
            ]);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            Arc::make_mut(&mut obj.base_static_definitions).push(animation.clone());
            obj.static_definitions.push(animation);
        }

        evaluate_layers(&mut state);

        let obj = &state.objects[&id];
        assert_eq!(obj.power, Some(3), "animated to 3 power");
        assert_eq!(obj.toughness, Some(3), "animated to 3 toughness");
        assert!(
            obj.card_types.core_types.contains(&CoreType::Creature),
            "must be a creature, got {:?}",
            obj.card_types.core_types
        );
        // CR 205.1b: "still a Cave land" — both the Land card type and the Cave
        // land subtype must survive the animation.
        assert!(
            obj.card_types.core_types.contains(&CoreType::Land),
            "must still be a Land, got {:?}",
            obj.card_types.core_types
        );
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Cave"),
            "must still be a Cave, got {:?}",
            obj.card_types.subtypes
        );
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Elemental"),
            "must gain the Elemental creature type, got {:?}",
            obj.card_types.subtypes
        );
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

    /// CR 122.1a + CR 613.4c: Asymmetric P/T counters modify only the axis
    /// named by the counter, and stack with the legacy +1/+1 / -1/-1 counters.
    #[test]
    fn parameterized_power_toughness_counters_apply_in_layer_7c() {
        let mut state = setup();
        let id = make_creature(&mut state, "Legacy Counter Host", 4, 4, PlayerId(0));
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.counters.insert(
                CounterType::PowerToughness {
                    power: 0,
                    toughness: -1,
                },
                1,
            );
            obj.counters.insert(
                CounterType::PowerToughness {
                    power: 0,
                    toughness: -2,
                },
                1,
            );
            obj.counters.insert(
                CounterType::PowerToughness {
                    power: -1,
                    toughness: 0,
                },
                2,
            );
            obj.counters.insert(CounterType::Plus1Plus1, 1);
        }

        evaluate_layers(&mut state);

        let obj = &state.objects[&id];
        assert_eq!(obj.power, Some(3));
        assert_eq!(obj.toughness, Some(2));
    }

    /// CR 613.4c + CR 613.4d: P/T counters (layer 7c) must be applied BEFORE the
    /// power/toughness switch (layer 7d). With an asymmetric counter the two
    /// orders diverge — applying the counter after the switch transposes it onto
    /// the wrong axis. Regression for the inversion that placed counters in a
    /// fictional "layer 7e" after the switch (engine returned 2/3 instead of 3/2).
    #[test]
    fn pt_counters_apply_before_switch_in_layer_seven() {
        let mut state = setup();
        // Base 2/2 so the only P/T asymmetry comes from the counter.
        let id = make_creature(&mut state, "Switch Host", 2, 2, PlayerId(0));

        let switch = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::SwitchPowerToughness]);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.counters.insert(
                CounterType::PowerToughness {
                    power: 0,
                    toughness: 1,
                },
                1,
            );
            Arc::make_mut(&mut obj.base_static_definitions).push(switch.clone());
            obj.static_definitions.push(switch);
        }

        evaluate_layers(&mut state);

        // 7c counter first: 2/2 -> 2/3. Then 7d switch: 2/3 -> 3/2.
        let obj = &state.objects[&id];
        assert_eq!(
            obj.power,
            Some(3),
            "power must be the post-counter toughness"
        );
        assert_eq!(
            obj.toughness,
            Some(2),
            "toughness must be the post-counter power"
        );
    }

    #[test]
    fn combat_assignment_rule_flags_are_post_layer_effects() {
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

        let layered_effects = collect_shared_active_continuous_effects(&state);
        assert!(
            layered_effects
                .iter()
                .all(|effect| !is_combat_assignment_rule_modification(&effect.modification)),
            "combat-assignment rule effects must not participate in CR 613 layer buckets"
        );

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

    #[test]
    fn combat_assignment_rule_effects_observe_final_layered_characteristics() {
        let mut state = setup();
        let source_id = make_creature(&mut state, "Belligerent Brontodon", 4, 6, PlayerId(0));
        let target_id = make_creature(&mut state, "Layered Bear", 2, 2, PlayerId(0));

        let brontodon_static = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::ToughnessGTPower]),
            ))
            .modifications(vec![ContinuousModification::AssignDamageFromToughness]);
        {
            let obj = state.objects.get_mut(&source_id).unwrap();
            Arc::make_mut(&mut obj.base_static_definitions).push(brontodon_static.clone());
            obj.static_definitions.push(brontodon_static);
        }

        state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target_id },
            vec![ContinuousModification::AddToughness { value: 1 }],
            None,
        );

        evaluate_layers(&mut state);

        let target = &state.objects[&target_id];
        assert_eq!(target.power, Some(2));
        assert_eq!(target.toughness, Some(3));
        assert!(
            target.assigns_damage_from_toughness,
            "post-layer rule effect must match the target after layer 7c toughness changes"
        );
    }

    /// Helper: creatures you control filter
    fn creature_you_ctrl() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
    }

    fn creatures_you_ctrl_with_power_ge(threshold: i32) -> TargetFilter {
        TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: threshold },
                }]),
        )
    }

    fn creatures_you_ctrl_with_power_or_toughness_le(threshold: i32) -> TargetFilter {
        TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::AnyOf {
                    props: vec![
                        FilterProp::PtComparison {
                            stat: PtStat::Power,
                            scope: PtValueScope::Current,
                            comparator: Comparator::LE,
                            value: QuantityExpr::Fixed { value: threshold },
                        },
                        FilterProp::PtComparison {
                            stat: PtStat::Toughness,
                            scope: PtValueScope::Current,
                            comparator: Comparator::LE,
                            value: QuantityExpr::Fixed { value: threshold },
                        },
                    ],
                }]),
        )
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
        state.layers_dirty.mark_full();
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
    fn pt_dependency_detects_disjunctive_pt_filter_props() {
        let filter = creatures_you_ctrl_with_power_or_toughness_le(2);

        assert!(filter_references_pt_stat(&filter));
    }

    #[test]
    fn set_pt_layer_orders_unconditional_setters_before_pt_threshold_filters() {
        let mut state = setup();
        let _bear = make_creature(&mut state, "Bear", 4, 4, PlayerId(0));

        let shrink = make_creature(&mut state, "Shrink", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&shrink).unwrap();
            obj.timestamp = 5;
        }
        {
            let def = StaticDefinition::continuous()
                .affected(creatures_you_ctrl_with_power_ge(5))
                // One modification: re-filtering after SetPower would drop power
                // below the threshold before a second clause could run (CR 613.7a).
                .modifications(vec![ContinuousModification::SetPower { value: 1 }]);
            state
                .objects
                .get_mut(&shrink)
                .unwrap()
                .static_definitions
                .push(def);
        }

        let setter = make_creature(&mut state, "Setter", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&setter).unwrap();
            obj.timestamp = 10;
        }
        {
            let def = StaticDefinition::continuous()
                .affected(creature_you_ctrl())
                .modifications(vec![ContinuousModification::SetPower { value: 6 }]);
            state
                .objects
                .get_mut(&setter)
                .unwrap()
                .static_definitions
                .push(def);
        }

        let set_pt_effects: Vec<ActiveContinuousEffect> =
            collect_shared_active_continuous_effects(&state)
                .into_iter()
                .filter(|e| e.layer == Layer::SetPT)
                .collect();
        let ordered = order_active_continuous_effects(Layer::SetPT, &set_pt_effects, &state);

        let shrink_set_power = ordered.iter().position(|e| {
            e.source_id == shrink
                && matches!(
                    e.modification,
                    ContinuousModification::SetPower { value: 1 }
                )
        });
        let setter_set_power = ordered.iter().position(|e| {
            e.source_id == setter
                && matches!(
                    e.modification,
                    ContinuousModification::SetPower { value: 6 }
                )
        });
        assert!(shrink_set_power.is_some() && setter_set_power.is_some());
        assert!(
            setter_set_power.unwrap() < shrink_set_power.unwrap(),
            "unconditional setter must apply before threshold shrink: {ordered:?}"
        );
    }

    /// CR 613.8a: A `SetPT` effect with a power threshold must apply after P/T
    /// setters that change which creatures meet that threshold.
    #[test]
    fn pt_modification_dependency_overrides_timestamp_in_set_pt_layer() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 4, 4, PlayerId(0));

        let shrink = make_creature(&mut state, "Shrink", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&shrink).unwrap();
            obj.timestamp = 5;
        }
        {
            let def = StaticDefinition::continuous()
                .affected(creatures_you_ctrl_with_power_ge(5))
                .modifications(vec![ContinuousModification::SetPower { value: 1 }]);
            state
                .objects
                .get_mut(&shrink)
                .unwrap()
                .static_definitions
                .push(def);
        }

        let setter = make_creature(&mut state, "Setter", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&setter).unwrap();
            obj.timestamp = 10;
        }
        {
            let def = StaticDefinition::continuous()
                .affected(creature_you_ctrl())
                .modifications(vec![ContinuousModification::SetPower { value: 6 }]);
            state
                .objects
                .get_mut(&setter)
                .unwrap()
                .static_definitions
                .push(def);
        }

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert_eq!(bear_obj.power, Some(1));
    }

    /// CR 613.8a: A conditional `ModifyPT` lord must apply after unconditional
    /// lords that raise creatures above the threshold.
    #[test]
    fn pt_modification_dependency_overrides_timestamp_in_modify_pt_layer() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 3, 3, PlayerId(0));

        let conditional = make_creature(&mut state, "Conditional Lord", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&conditional).unwrap();
            obj.timestamp = 5;
        }
        add_lord_static(
            &mut state,
            conditional,
            creatures_you_ctrl_with_power_ge(4),
            2,
            0,
        );

        let unconditional = make_creature(&mut state, "Unconditional Lord", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&unconditional).unwrap();
            obj.timestamp = 10;
        }
        add_lord_static(&mut state, unconditional, creature_you_ctrl(), 2, 0);

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert_eq!(bear_obj.power, Some(7));
        assert_eq!(bear_obj.toughness, Some(3));
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
        assert!(state.layers_dirty.is_dirty());

        evaluate_layers(&mut state);

        assert!(!state.layers_dirty.is_dirty());
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

    /// CR 509.1b + CR 613.1f + CR 702.18a: End-to-end runtime confirmation that
    /// Whispersilk Cloak's compound "Equipped creature can't be blocked and has
    /// shroud." drives a real `parse_static_line_multi` output through the layer
    /// pipeline and grants Shroud to the equipped creature. The keyword companion
    /// (split out from the `CantBeBlocked` restriction) must actually reach the
    /// equipped creature — not silently dropped.
    #[test]
    fn whispersilk_compound_grants_shroud_through_layers() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        let defs = crate::parser::oracle_static::parse_static_line_multi(
            "Equipped creature can't be blocked and has shroud.",
        );
        assert_eq!(defs.len(), 2, "parser must emit 2 defs, got {defs:?}");

        let equipment = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Whispersilk Cloak".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&equipment).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".into());
            obj.attached_to = Some(bear.into());
            obj.timestamp = ts;
            for def in defs {
                obj.static_definitions.push(def);
            }
        }
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .attachments
            .push(equipment);

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let equipped = state.objects.get(&bear).unwrap();
        assert!(
            equipped.has_keyword(&Keyword::Shroud),
            "equipped creature must gain Shroud from the keyword companion"
        );
    }

    /// CR 613.1f + CR 702: End-to-end confirmation that the Theros Archetype cycle /
    /// Arcane Lighthouse "can't have or gain [keyword]" denial wins in Layer 6 over a
    /// concurrent keyword grant. A creature given Flying by an anthem must NOT keep
    /// Flying once an Archetype-style `CantHaveKeyword { Flying }` static is in play —
    /// the denial is applied after all grants, so it is order-independent.
    #[test]
    fn cant_have_keyword_denial_overrides_concurrent_grant() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        // Anthem: grants Flying to all creatures (Layer 6 ability-adding effect).
        let anthem_static = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]);
        let anthem = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Flight Anthem".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&anthem).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(anthem_static);
        }

        // Baseline: with only the anthem, the bear gains Flying.
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        assert!(
            state
                .objects
                .get(&bear)
                .unwrap()
                .has_keyword(&Keyword::Flying),
            "anthem must grant Flying before the denial is added"
        );

        // Archetype: creatures can't have or gain Flying (Layer 6 denial).
        let denial_static = StaticDefinition::new(StaticMode::CantHaveKeyword {
            keyword: Keyword::Flying,
        })
        .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
        let archetype = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Archetype of Imagination".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&archetype).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(denial_static);
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        assert!(
            !state
                .objects
                .get(&bear)
                .unwrap()
                .has_keyword(&Keyword::Flying),
            "CantHaveKeyword denial must strip Flying granted by the concurrent anthem"
        );
    }

    /// CR 604.1 + CR 702.123a/b: a runtime grant of `Fabricate N` via a
    /// continuous `AddKeyword` static must install the Fabricate ETB trigger
    /// onto the recipient's `trigger_definitions` (the trigger-on-grant seam at
    /// `KeywordTriggerInstaller::triggers_for`). Before the `Fabricate` arm
    /// existed, the recipient gained the bare keyword but no trigger, so this
    /// assertion would have found zero installed Fabricate triggers.
    #[test]
    fn granted_fabricate_installs_etb_trigger_on_recipient() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        // Anthem-style static: grants Fabricate 2 to all creatures.
        let anthem_static = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Fabricate(2),
            }]);
        let anthem = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fabricate Anthem".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&anthem).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(anthem_static);
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let recipient = state.objects.get(&bear).unwrap();
        assert!(
            recipient.has_keyword(&Keyword::Fabricate(2)),
            "granted Fabricate keyword must land on the recipient"
        );
        let fabricate_triggers = recipient
            .trigger_definitions
            .iter_all()
            .filter(|t| {
                KeywordTriggerInstaller::trigger_matches_keyword_kind(t, &Keyword::Fabricate(2))
            })
            .count();
        assert_eq!(
            fabricate_triggers, 1,
            "granted Fabricate must install exactly one ETB ChooseOneOf trigger"
        );
    }

    /// CR 614.12 + CR 604.1 + CR 702.136a (seam 3, replacement-on-grant): a
    /// battlefield permanent whose Continuous static grants `AddKeyword{Riot}` to
    /// a subset of permanents must, after a layer pass, carry the affected-filter
    /// as-enters Riot replacement on its OWN `replacement_definitions` (scoped to
    /// the static's `affected` filter, NOT SelfRef per CR 614.12). Before the
    /// runtime-derivation pass existed, `AddKeyword` installed only the keyword +
    /// triggers, never a replacement, so this assertion found none.
    #[test]
    fn granted_riot_static_derives_affected_filter_replacement_on_source() {
        let mut state = setup();

        let affected = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You),
        );
        let riot_static = StaticDefinition::new(StaticMode::Continuous)
            .affected(affected.clone())
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Riot,
            }]);
        let grantor = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Riot Grantor".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&grantor).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(riot_static);
            obj.base_static_definitions = obj.static_definitions.as_slice().to_vec().into();
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let expected =
            crate::database::synthesis::keyword_entry_replacement(&Keyword::Riot, affected.clone())
                .expect("Riot must map to an entry replacement");
        let source = state.objects.get(&grantor).unwrap();
        let installed = source
            .replacement_definitions
            .iter_all()
            .filter(|r| **r == expected)
            .count();
        assert_eq!(
            installed, 1,
            "granting permanent must carry exactly one affected-filter Riot replacement"
        );

        // Idempotency across re-evaluation (the per-pass reset + re-derive must
        // not accumulate duplicates).
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        let source = state.objects.get(&grantor).unwrap();
        assert_eq!(
            source
                .replacement_definitions
                .iter_all()
                .filter(|r| **r == expected)
                .count(),
            1,
            "re-evaluation must not duplicate the derived replacement"
        );
    }

    /// CR 113.11 + CR 122.1b: a "can't have [keyword]" effect makes it impossible
    /// for even a KEYWORD COUNTER to add that ability. A first-strike counter on a
    /// creature under an Archetype-of-Courage-style `CantHaveKeyword { FirstStrike }`
    /// denial must NOT grant first strike. The keyword-counter grant ran after the
    /// denial pass with no re-denial, so the counter wrongly re-added the keyword.
    #[test]
    fn cant_have_keyword_denial_overrides_keyword_counter() {
        use crate::types::counter::CounterType;
        use crate::types::keywords::KeywordKind;

        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .counters
            .insert(CounterType::Keyword(KeywordKind::FirstStrike), 1);

        // Baseline: the keyword counter grants first strike with no denial present.
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        assert!(
            state
                .objects
                .get(&bear)
                .unwrap()
                .has_keyword(&Keyword::FirstStrike),
            "keyword counter grants first strike before the denial is added"
        );

        // Archetype of Courage: creatures can't have first strike (Layer 6 denial).
        let denial = StaticDefinition::new(StaticMode::CantHaveKeyword {
            keyword: Keyword::FirstStrike,
        })
        .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
        let archetype = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Archetype of Courage".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&archetype).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(denial);
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        assert!(
            !state
                .objects
                .get(&bear)
                .unwrap()
                .has_keyword(&Keyword::FirstStrike),
            "CR 113.11: a keyword counter cannot grant a denied keyword"
        );
    }

    /// CR 613.1f → CR 613.1g: The denial is applied at the END of Layer 6, so a
    /// Layer 7 power/toughness effect conditional on the denied keyword
    /// ("creatures with flying get +1/+1") observes the keyword as already removed
    /// and does NOT apply. Regression guard for the layer-evaluation-order fix:
    /// running the strip after the full layer loop (post-Layer-7) would let the
    /// buff land incorrectly.
    #[test]
    fn cant_have_keyword_denial_is_observed_by_layer7_pt() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        // Layer 6: grant Flying to all creatures.
        let flying_anthem = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]);
        // Layer 7: creatures WITH flying get +1/+1 — keyword-conditional P/T.
        let flying_pt_anthem = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Creature).properties(vec![FilterProp::WithKeyword {
                    value: Keyword::Flying,
                }]),
            ))
            .modifications(vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ]);
        let anthem = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Flight & Buff".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&anthem).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(flying_anthem);
            obj.static_definitions.push(flying_pt_anthem);
        }

        // Layer 6 denial: creatures can't have or gain Flying.
        let denial = StaticDefinition::new(StaticMode::CantHaveKeyword {
            keyword: Keyword::Flying,
        })
        .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
        let archetype = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Archetype of Imagination".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&archetype).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(denial);
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let b = state.objects.get(&bear).unwrap();
        assert!(
            !b.has_keyword(&Keyword::Flying),
            "denial must strip Flying at the end of Layer 6"
        );
        assert_eq!(
            b.power,
            Some(2),
            "Layer 7 'flying creatures get +1/+1' must NOT apply — the denial removed \
             Flying before Layer 7, so the keyword-conditional buff sees no Flying"
        );
        assert_eq!(b.toughness, Some(2), "toughness likewise unbuffed");
    }

    /// CR 613.1f: The incremental layer path must mirror the full pass's
    /// end-of-Layer-6 keyword denial for newly-entered objects. Regression guard:
    /// if the incremental path omits the denial hook, this plain entered creature
    /// keeps Flying from the existing anthem even though the existing Archetype
    /// static says creatures can't have or gain Flying.
    #[test]
    fn cant_have_keyword_denial_applies_to_incremental_entry() {
        let mut state = setup();

        let flying_anthem = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]);
        let anthem = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Flight Anthem".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&anthem).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(flying_anthem);
        }

        let denial = StaticDefinition::new(StaticMode::CantHaveKeyword {
            keyword: Keyword::Flying,
        })
        .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)));
        let archetype = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Archetype of Imagination".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&archetype).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(denial);
        }

        evaluate_layers(&mut state);

        let entered = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Fresh Bear".to_string(),
            Zone::Hand,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&entered).unwrap();
            obj.zone = Zone::Battlefield;
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.timestamp = ts;
        }
        state.battlefield.push_back(entered);
        mark_layers_entered(&mut state, entered);
        flush_layers(&mut state);

        assert!(
            !state
                .objects
                .get(&entered)
                .unwrap()
                .has_keyword(&Keyword::Flying),
            "entered creature must have Flying stripped by the incremental Layer 6 denial hook"
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

    /// CR 205.4a + CR 613.4c: Jodah-style static anthem — "Legendary creatures
    /// you control get +X/+X, where X is the number of legendary creatures you
    /// control." The affected filter must test the legendary supertype, and
    /// the dynamic amount must count the same supertype-qualified population.
    #[test]
    fn dynamic_legendary_anthem_counts_and_affects_legendary_creatures_you_control() {
        let mut state = setup();
        let jodah = make_creature(&mut state, "Jodah, the Unifier", 5, 5, PlayerId(0));
        let ally = make_creature(&mut state, "Legendary Ally", 2, 2, PlayerId(0));
        let ordinary = make_creature(&mut state, "Ordinary Bear", 2, 2, PlayerId(0));
        let opponent_legend = make_creature(&mut state, "Opponent Legend", 2, 2, PlayerId(1));

        for id in [jodah, ally, opponent_legend] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.supertypes.push(Supertype::Legendary);
            obj.base_card_types = obj.card_types.clone();
        }

        // Drive Jodah's real Oracle line through the parser so this runtime test
        // also fails if the supertype-descriptor parse regresses (closing the
        // parser->layers seam, not hand-building the expected StaticDefinition).
        let def = crate::parser::oracle_static::parse_static_line(
            "Legendary creatures you control get +X/+X, where X is the number of legendary creatures you control.",
        )
        .expect("Jodah anthem static should parse");
        state
            .objects
            .get_mut(&jodah)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        assert_eq!(state.objects[&jodah].power, Some(7));
        assert_eq!(state.objects[&jodah].toughness, Some(7));
        assert_eq!(state.objects[&ally].power, Some(4));
        assert_eq!(state.objects[&ally].toughness, Some(4));
        assert_eq!(state.objects[&ordinary].power, Some(2));
        assert_eq!(state.objects[&ordinary].toughness, Some(2));
        assert_eq!(state.objects[&opponent_legend].power, Some(2));
        assert_eq!(state.objects[&opponent_legend].toughness, Some(2));
    }

    #[test]
    fn dynamic_pt_lost_game_player_count_scales_past_one_player() {
        let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 3, 42);
        state.players[1].is_eliminated = true;
        state.players[2].is_eliminated = true;

        let frogantua = make_creature(&mut state, "Rampant Frogantua", 3, 3, PlayerId(0));
        let qty = QuantityExpr::Multiply {
            factor: 10,
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::HasLostTheGame,
                },
            }),
        };
        state
            .objects
            .get_mut(&frogantua)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![
                        ContinuousModification::AddDynamicPower { value: qty.clone() },
                        ContinuousModification::AddDynamicToughness { value: qty },
                    ]),
            );

        evaluate_layers(&mut state);

        let frogantua = state.objects.get(&frogantua).unwrap();
        assert_eq!(frogantua.power, Some(23));
        assert_eq!(frogantua.toughness, Some(23));
    }

    #[test]
    fn alpha_status_counts_other_creatures_sharing_recipient_creature_type() {
        use crate::types::ability::{
            FilterProp, QuantityRef, SharedQuality, SharedQualityRelation, TargetFilter,
            TypeFilter, TypedFilter,
        };

        let mut state = setup();
        state.all_creature_types = vec![
            "Bear".to_string(),
            "Elf".to_string(),
            "Shapeshifter".to_string(),
        ];

        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .card_types
            .subtypes
            .push("Bear".into());
        state.objects.get_mut(&bear).unwrap().base_card_types =
            state.objects.get(&bear).unwrap().card_types.clone();

        let other_bear = make_creature(&mut state, "Other Bear", 2, 2, PlayerId(0));
        state
            .objects
            .get_mut(&other_bear)
            .unwrap()
            .card_types
            .subtypes
            .push("Bear".into());
        state.objects.get_mut(&other_bear).unwrap().base_card_types =
            state.objects.get(&other_bear).unwrap().card_types.clone();

        let elf = make_creature(&mut state, "Elf", 2, 2, PlayerId(0));
        state
            .objects
            .get_mut(&elf)
            .unwrap()
            .card_types
            .subtypes
            .push("Elf".into());
        state.objects.get_mut(&elf).unwrap().base_card_types =
            state.objects.get(&elf).unwrap().card_types.clone();

        let changeling = make_creature(&mut state, "Changeling", 2, 2, PlayerId(0));
        {
            let obj = state.objects.get_mut(&changeling).unwrap();
            obj.card_types.subtypes.push("Shapeshifter".into());
            obj.keywords.push(Keyword::Changeling);
            obj.base_card_types = obj.card_types.clone();
            obj.base_keywords = obj.keywords.clone();
        }

        let alpha_status = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Alpha Status".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&alpha_status).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".into());
            obj.attached_to = Some(bear.into());
            obj.timestamp = ts;

            let qty = QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            controller: None,
                            properties: vec![
                                FilterProp::Another,
                                FilterProp::SharesQuality {
                                    quality: SharedQuality::CreatureType,
                                    reference: Some(Box::new(TargetFilter::ParentTarget)),
                                    relation: SharedQualityRelation::Shares,
                                },
                            ],
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
            .push(alpha_status);

        evaluate_layers(&mut state);

        let final_bear = state.objects.get(&bear).unwrap();
        assert_eq!(
            final_bear.power,
            Some(6),
            "Bear should get +2/+2 for other Bear and Changeling only"
        );
        assert_eq!(final_bear.toughness, Some(6));
        assert_eq!(state.objects.get(&other_bear).unwrap().power, Some(2));
        assert_eq!(state.objects.get(&elf).unwrap().power, Some(2));
        assert_eq!(state.objects.get(&changeling).unwrap().power, Some(2));
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

    /// CR 303.4m + CR 613.4c: Righteous Authority-style Aura statics read the
    /// enchanted creature's controller for "its controller's hand", not the
    /// Aura source controller.
    #[test]
    fn dynamic_pt_uses_recipient_controller_hand_size() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Borrowed Bear", 2, 2, PlayerId(1));

        for n in 0..1 {
            create_object(
                &mut state,
                CardId(100 + n),
                PlayerId(0),
                format!("P0 Hand {n}"),
                Zone::Hand,
            );
        }
        for n in 0..4 {
            create_object(
                &mut state,
                CardId(200 + n),
                PlayerId(1),
                format!("P1 Hand {n}"),
                Zone::Hand,
            );
        }

        let righteous_authority = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Righteous Authority".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&righteous_authority).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".into());
            obj.attached_to = Some(bear.into());
            obj.timestamp = ts;

            let qty = QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: PlayerScope::RecipientController,
                },
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
            .push(righteous_authority);

        evaluate_layers(&mut state);

        let final_bear = state.objects.get(&bear).unwrap();
        assert_eq!(
            final_bear.power,
            Some(6),
            "expected 2 base + P1 hand size 4, not P0 hand size 1"
        );
        assert_eq!(final_bear.toughness, Some(6));
    }

    /// CR 201.1 + CR 201.2 + CR 303.4m + CR 613.4c: Wordmail-style Aura
    /// statics count words in the enchanted creature's name, not the Aura
    /// source's name.
    #[test]
    fn dynamic_pt_counts_words_in_recipient_name_not_source_name() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Silvercoat Lion Cub", 2, 2, PlayerId(0));

        let wordmail = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Wordmail".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&wordmail).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".into());
            obj.attached_to = Some(bear.into());
            obj.timestamp = ts;

            let qty = QuantityExpr::Ref {
                qty: QuantityRef::ObjectNameWordCount {
                    scope: ObjectScope::Recipient,
                },
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
            .push(wordmail);

        evaluate_layers(&mut state);

        let final_bear = state.objects.get(&bear).unwrap();
        assert_eq!(
            final_bear.power,
            Some(5),
            "expected 2 base + 3 words in recipient name, not 1 word in source name"
        );
        assert_eq!(final_bear.toughness, Some(5));
    }

    /// CR 613.4c: Attached continuous-effect conditions that use recipient
    /// quantities must be evaluated per affected object after that object is known.
    #[test]
    fn attached_continuous_condition_reads_recipient_power() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Small Rogue", 3, 3, PlayerId(0));

        let equipment = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Power Gated Equipment".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&equipment).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".into());
            obj.attached_to = Some(bear.into());
            obj.timestamp = ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
                    ))
                    .modifications(vec![ContinuousModification::AddPower { value: 1 }])
                    .condition(StaticCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::Power {
                                scope: ObjectScope::Recipient,
                            },
                        },
                        comparator: crate::types::ability::Comparator::LE,
                        rhs: QuantityExpr::Fixed { value: 3 },
                    }),
            );
        }
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .attachments
            .push(equipment);

        evaluate_layers(&mut state);
        assert_eq!(state.objects.get(&bear).unwrap().power, Some(4));

        let bear_obj = state.objects.get_mut(&bear).unwrap();
        bear_obj.base_power = Some(4);
        bear_obj.power = Some(4);
        evaluate_layers(&mut state);
        assert_eq!(state.objects.get(&bear).unwrap().power, Some(4));
    }

    #[test]
    fn attached_object_presence_condition_uses_source_attachment_context() {
        let mut state = setup();
        let creature = make_creature(&mut state, "Host Creature", 2, 2, PlayerId(0));
        let artifact = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Host Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Attached Condition Aura".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.attached_to = Some(creature.into());
        }

        let condition = StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            )),
        };
        assert!(evaluate_condition(&state, &condition, PlayerId(0), aura));

        state.objects.get_mut(&aura).unwrap().attached_to = Some(artifact.into());
        assert!(!evaluate_condition(&state, &condition, PlayerId(0), aura));
    }

    /// CR 107.4 + CR 202.1 + CR 613.4c: Light from Within-style statics count
    /// mana symbols in each affected creature's own mana cost. Hybrid and
    /// Phyrexian symbols that contain the color count through
    /// `ManaCostShard::contributes_to`.
    #[test]
    fn dynamic_pt_counts_recipient_mana_cost_symbols_per_creature() {
        let mut state = setup();
        let white_bear = make_creature(&mut state, "White Bear", 2, 2, PlayerId(0));
        let hybrid_bear = make_creature(&mut state, "Hybrid Bear", 2, 2, PlayerId(0));
        let blue_bear = make_creature(&mut state, "Blue Bear", 2, 2, PlayerId(0));

        state.objects.get_mut(&white_bear).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 1,
        };
        state.objects.get_mut(&hybrid_bear).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![
                ManaCostShard::WhiteBlack,
                ManaCostShard::TwoWhite,
                ManaCostShard::PhyrexianWhite,
            ],
            generic: 0,
        };
        state.objects.get_mut(&blue_bear).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        };

        let light = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Light from Within".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&light).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.timestamp = ts;

            let qty = QuantityExpr::Ref {
                qty: QuantityRef::ManaSymbolsInManaCost {
                    scope: crate::types::ability::ObjectScope::Recipient,
                    color: Some(ManaColor::White),
                },
            };
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: Some(ControllerRef::You),
                        properties: vec![],
                    }))
                    .modifications(vec![
                        ContinuousModification::AddDynamicPower { value: qty.clone() },
                        ContinuousModification::AddDynamicToughness { value: qty },
                    ]),
            );
        }

        evaluate_layers(&mut state);

        let white = state.objects.get(&white_bear).unwrap();
        assert_eq!(white.power, Some(4));
        assert_eq!(white.toughness, Some(4));

        let hybrid = state.objects.get(&hybrid_bear).unwrap();
        assert_eq!(hybrid.power, Some(5));
        assert_eq!(hybrid.toughness, Some(5));

        let blue = state.objects.get(&blue_bear).unwrap();
        assert_eq!(blue.power, Some(2));
        assert_eq!(blue.toughness, Some(2));
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
        state.layers_dirty.mark_full();
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
                        player: TargetFilter::Controller,
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

    /// CR 613.1f (issue #1321): suppression state must be scoped to the current
    /// layer pass — an incremental flush after a prior full pass that removed
    /// abilities must not inherit stale suppression and skip self-sourced CDAs.
    #[test]
    fn incremental_layer_pass_does_not_inherit_remove_all_abilities_suppression() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        {
            let def = StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddPower { value: 3 }]);
            let bear_obj = state.objects.get_mut(&bear).unwrap();
            bear_obj.static_definitions.push(def.clone());
            Arc::make_mut(&mut bear_obj.base_static_definitions).push(def);
        }

        let suppressor = make_creature(&mut state, "Suppressor", 1, 1, PlayerId(0));
        {
            let def = StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: bear })
                .modifications(vec![ContinuousModification::RemoveAllAbilities]);
            state
                .objects
                .get_mut(&suppressor)
                .unwrap()
                .static_definitions
                .push(def);
        }

        evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&bear).unwrap().power,
            Some(2),
            "RemoveAllAbilities must suppress the CDA during the full pass"
        );

        // Suppressor leaves; only the bear is re-derived incrementally.
        state.battlefield.retain(|&id| id != suppressor);
        state.objects.remove(&suppressor);
        state.layers_dirty = LayersDirty::EnteredObjects([bear].into());
        flush_layers(&mut state);

        assert_eq!(
            state.objects.get(&bear).unwrap().power,
            Some(5),
            "incremental pass must rebuild suppression locally and re-apply the CDA"
        );
    }

    #[test]
    fn darksteel_mutation_full_layer_evaluation_e2e() {
        // Issue #453: drive the real pipeline — parse Darksteel Mutation's
        // Oracle text into its static ability, attach it to a creature via the
        // engine's `attach_to` primitive (real `attached_to` link + real
        // timestamps), then run layer evaluation.
        use crate::game::effects::attach::attach_to;

        let mut scenario = GameScenario::new();

        // A base creature with a printed keyword and an activated ability.
        let bear = {
            let mut card = scenario.add_creature(PlayerId(0), "Grizzly Bears", 2, 2);
            card.trample()
                .with_ability_definition(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ))
                .with_trigger(TriggerMode::Attacks);
            card.id()
        };

        // Darksteel Mutation as a real battlefield Aura, static parsed from
        // Oracle text — not a hand-built modification list.
        let mutation = scenario
            .add_creature(PlayerId(0), "Darksteel Mutation", 0, 0)
            .as_enchantment()
            .from_oracle_text(
                "Enchant creature\nEnchanted creature is an Insect artifact creature \
                 with base power and toughness 0/1 and has indestructible, and it \
                 loses all other abilities, card types, and creature types.",
            )
            .id();

        let mut state = scenario.build().state().clone();
        // Mark the Aura with the Aura subtype so it is a valid attachment.
        state
            .objects
            .get_mut(&mutation)
            .unwrap()
            .card_types
            .subtypes
            .push("Aura".to_string());

        // Real attach pipeline: sets `attached_to` + host `attachments`.
        attach_to(&mut state, mutation, bear);

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&bear).unwrap();
        // CR 613.4b: base P/T set to 0/1.
        assert_eq!(obj.power, Some(0), "power should be 0");
        assert_eq!(obj.toughness, Some(1), "toughness should be 1");
        // CR 613.1f + CR 613.7: indestructible granted, and it survives the
        // RemoveAllAbilities wipe via the written-order (mod_index) tiebreak.
        assert!(
            obj.keywords.contains(&Keyword::Indestructible),
            "should have indestructible, keywords={:?}",
            obj.keywords
        );
        assert!(
            !obj.keywords.contains(&Keyword::Trample),
            "trample must be stripped by RemoveAllAbilities"
        );
        // CR 613.1f: all printed abilities removed.
        assert!(obj.abilities.is_empty(), "abilities must be empty");
        assert!(
            obj.trigger_definitions.is_empty(),
            "trigger definitions must be empty"
        );
        assert!(
            obj.static_definitions.is_empty(),
            "static definitions must be empty"
        );
        // CR 205.1b + CR 613.1d: exactly artifact + creature.
        assert_eq!(
            obj.card_types.core_types,
            vec![CoreType::Artifact, CoreType::Creature],
            "core types must be exactly [Artifact, Creature]"
        );
        // CR 205.1a/b: creature types replaced — exactly Insect.
        assert_eq!(
            obj.card_types.subtypes,
            vec!["Insect".to_string()],
            "subtypes must be exactly [Insect]"
        );
    }

    /// CR 611.2a + CR 613.1f + CR 613.4b (Azure Beastbinder): drive the REAL
    /// parser + resolution pipeline. Azure Beastbinder's attack trigger is parsed
    /// from its printed Oracle text, then resolved (via the same
    /// `build_resolved_from_def_with_targets` the trigger system uses) against an
    /// opponent's 5/5 creature that has a keyword and an activated ability. After
    /// `resolve_ability_chain` registers the transient continuous effects and
    /// `evaluate_layers` applies them, the target must have:
    ///   - base power and toughness set to 2/2 (the historical Unimplemented gap),
    ///   - all abilities and keywords removed.
    ///
    /// REVERT-PROOF: reverting either parser fix re-introduces the failure —
    /// without the "it also" subject strip the base-P/T clause stays
    /// `Unimplemented` (no SetPower/SetToughness → power/toughness remain 5/5);
    /// without the `RevealedHasCardType → TargetMatchesFilter` conversion the
    /// creature-gate evaluates always-false (no revealed card) so the base-P/T
    /// sub-ability never runs (power/toughness remain 5/5).
    #[test]
    fn azure_beastbinder_attack_sets_target_base_pt_and_strips_abilities_e2e() {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::TargetRef;

        let mut scenario = GameScenario::new();

        // Source: Azure Beastbinder, parsed from its real Oracle text.
        let beastbinder = scenario
            .add_creature(PlayerId(0), "Azure Beastbinder", 3, 3)
            .from_oracle_text_with_keywords(
                &["Vigilance"],
                "Vigilance\n\
                 This creature can't be blocked by creatures with power 2 or greater.\n\
                 Whenever this creature attacks, up to one target artifact, creature, or \
                 planeswalker an opponent controls loses all abilities until your next turn. \
                 If it's a creature, it also has base power and toughness 2/2 until your next turn.",
            )
            .id();

        // Target: an opponent's 5/5 creature with a keyword + an activated ability.
        let target = {
            let mut card = scenario.add_creature(PlayerId(1), "Big Beater", 5, 5);
            card.flying()
                .with_ability_definition(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ));
            card.id()
        };

        let mut state = scenario.build().state().clone();

        // Pull the parsed attack-trigger execute body off the source object.
        let execute = state
            .objects
            .get(&beastbinder)
            .unwrap()
            .trigger_definitions
            .iter_all()
            .find(|t| matches!(t.mode, TriggerMode::Attacks))
            .and_then(|t| t.execute.clone())
            .expect("Azure Beastbinder must have a parsed Attacks trigger with an execute body");

        // Resolve the trigger against the chosen target (the 5/5 creature),
        // exactly as the trigger system would after target selection.
        let ability = build_resolved_from_def_with_targets(
            &execute,
            beastbinder,
            PlayerId(0),
            vec![TargetRef::Object(target)],
        );
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("attack-trigger chain must resolve");

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&target).unwrap();
        // CR 613.4b: base power and toughness set to 2/2.
        assert_eq!(
            obj.power,
            Some(2),
            "target power must be 2, got {:?}",
            obj.power
        );
        assert_eq!(
            obj.toughness,
            Some(2),
            "target toughness must be 2, got {:?}",
            obj.toughness
        );
        // CR 613.1f: all abilities and keywords removed.
        assert!(
            obj.keywords.is_empty(),
            "target keywords must be stripped, got {:?}",
            obj.keywords
        );
        assert!(
            obj.abilities.is_empty(),
            "target abilities must be stripped, got {:?}",
            obj.abilities
        );
    }

    /// CR 109.2 + CR 613.4b: the creature-gate discriminates. When Azure
    /// Beastbinder's attack targets an opponent's ARTIFACT (non-creature), the
    /// "if it's a creature" sub-ability must NOT fire — abilities are still
    /// stripped (head clause) but the base-P/T set does not apply (an artifact
    /// has no P/T to set). This pins the `TargetMatchesFilter` gate as a real
    /// discriminator rather than an always-true rider.
    #[test]
    fn azure_beastbinder_attack_artifact_target_skips_base_pt() {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::TargetRef;

        let mut scenario = GameScenario::new();

        let beastbinder = scenario
            .add_creature(PlayerId(0), "Azure Beastbinder", 3, 3)
            .from_oracle_text_with_keywords(
                &["Vigilance"],
                "Vigilance\n\
                 This creature can't be blocked by creatures with power 2 or greater.\n\
                 Whenever this creature attacks, up to one target artifact, creature, or \
                 planeswalker an opponent controls loses all abilities until your next turn. \
                 If it's a creature, it also has base power and toughness 2/2 until your next turn.",
            )
            .id();

        // Opponent's noncreature Artifact with an activated ability. `add_creature`
        // seeds base P/T 0/0; `as_artifact` makes it a noncreature artifact. The
        // creature-gate must leave its (base) power/toughness at 0 — a fired
        // base-P/T set would push it to 2.
        let artifact = {
            let mut card = scenario.add_creature(PlayerId(1), "Sol Ring", 0, 0);
            card.as_artifact()
                .with_ability_definition(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ));
            card.id()
        };

        let mut state = scenario.build().state().clone();

        let execute = state
            .objects
            .get(&beastbinder)
            .unwrap()
            .trigger_definitions
            .iter_all()
            .find(|t| matches!(t.mode, TriggerMode::Attacks))
            .and_then(|t| t.execute.clone())
            .expect("parsed Attacks trigger");

        let ability = build_resolved_from_def_with_targets(
            &execute,
            beastbinder,
            PlayerId(0),
            vec![TargetRef::Object(artifact)],
        );
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("attack-trigger chain must resolve");

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&artifact).unwrap();
        // Head clause still strips abilities from the artifact target.
        assert!(
            obj.abilities.is_empty(),
            "artifact target abilities must be stripped, got {:?}",
            obj.abilities
        );
        // The creature-gated base-P/T set must NOT fire: the artifact keeps its
        // base 0/0 (a fired SetPower/SetToughness would make it 2/2).
        assert_eq!(
            obj.power,
            Some(0),
            "artifact target must not gain power from the creature-gated clause"
        );
        assert_eq!(
            obj.toughness,
            Some(0),
            "artifact target must not gain toughness from the creature-gated clause"
        );
    }

    #[test]
    fn muraganda_petroglyphs_buffs_only_abilityless_creatures_e2e() {
        // Drive the REAL parser end-to-end: Muraganda Petroglyphs' Oracle text
        // ("Creatures with no abilities get +2/+2.") is parsed via
        // `from_oracle_text` into a global continuous static, placed on the
        // battlefield as an enchantment, then applied by `evaluate_layers`.
        // Discriminates on two axes: ability-presence (vanilla buffed, flyer not)
        // and global scope (an OPPONENT's vanilla creature is buffed, since the
        // anthem has no controller restriction).
        let mut scenario = GameScenario::new();

        // Opponent's vanilla creature (no abilities) — must be buffed (global).
        let opp_vanilla = scenario
            .add_creature(PlayerId(1), "Vanilla Bear", 2, 2)
            .id();

        // Controller's flyer (has a keyword ability) — must NOT be buffed.
        let flyer = {
            let mut card = scenario.add_creature(PlayerId(0), "Sky Bear", 2, 2);
            card.flying();
            card.id()
        };

        // Muraganda Petroglyphs as a real battlefield enchantment whose static is
        // parsed from Oracle text (exercises the new `parse_no_abilities` arm).
        scenario
            .add_creature(PlayerId(0), "Muraganda Petroglyphs", 0, 0)
            .as_enchantment()
            .from_oracle_text("Creatures with no abilities get +2/+2.");

        let mut state = scenario.build().state().clone();
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        // CR 613.4c: the abilityless creature gets +2/+2 — every player's
        // creatures, so the opponent's vanilla is buffed to 4/4.
        let v = state.objects.get(&opp_vanilla).unwrap();
        assert_eq!(
            v.power,
            Some(4),
            "abilityless creature should be buffed to 4/4"
        );
        assert_eq!(v.toughness, Some(4));

        // CR 113.3: the flyer HAS an ability, so it is excluded — stays 2/2.
        let f = state.objects.get(&flyer).unwrap();
        assert_eq!(
            f.power,
            Some(2),
            "ability-bearing creature must not be buffed"
        );
        assert_eq!(f.toughness, Some(2));
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
                        player: TargetFilter::Controller,
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
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&target).unwrap();
        assert_eq!(obj.keywords, vec![Keyword::Flying]);
        assert_eq!(obj.abilities.len(), 1);
        assert_eq!(obj.trigger_definitions.len(), 1);
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert_eq!(obj.static_definitions.len(), 1);
    }

    #[test]
    fn impending_not_creature_participates_in_layer_timestamp_ordering() {
        let mut state = setup();
        let impending = make_creature(&mut state, "Impending Permanent", 3, 3, PlayerId(0));
        {
            let obj = state.objects.get_mut(&impending).unwrap();
            obj.timestamp = 5;
            obj.cast_variant_paid = Some((CastVariantPaid::Impending, 1));
            obj.counters.insert(CounterType::Time, 1);
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .condition(StaticCondition::And {
                        conditions: vec![
                            StaticCondition::CastVariantPaid {
                                variant: CastVariantPaid::Impending,
                            },
                            StaticCondition::HasCounters {
                                counters: CounterMatch::OfType(CounterType::Time),
                                minimum: 1,
                                maximum: None,
                            },
                        ],
                    })
                    .modifications(vec![ContinuousModification::RemoveType {
                        core_type: CoreType::Creature,
                    }]),
            );
        }

        let animator = make_creature(&mut state, "Later Animator", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&animator).unwrap();
            obj.timestamp = 10;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: impending })
                    .modifications(vec![ContinuousModification::AddType {
                        core_type: CoreType::Creature,
                    }]),
            );
        }

        evaluate_layers(&mut state);

        assert!(
            state.objects[&impending]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "CR 613.1d + CR 613.7: a later Layer 4 AddType must apply after Impending's RemoveType"
        );
    }

    /// CR 702.151b + CR 613.1d: A reconfigure Equipment is a creature while
    /// unattached, stops being a creature while attached to a creature, and
    /// becomes a creature again when unattached. Drives the synthesized
    /// `synthesize_reconfigure` Layer-4 RemoveType static through
    /// `evaluate_layers`. This is the discriminating test for the type-removal
    /// fix: it flips to a hard failure if Step 2 of the fix is reverted (the
    /// Equipment would remain a creature while attached).
    #[test]
    fn reconfigure_equipment_loses_creature_type_while_attached() {
        use crate::database::synthesis::synthesize_reconfigure;
        use crate::game::effects::attach::{attach_to, unattach};
        use crate::types::card::CardFace;
        use crate::types::keywords::Keyword;

        // Synthesize the reconfigure statics from the typed keyword (real path).
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Reconfigure(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        }));
        synthesize_reconfigure(&mut face);

        let mut state = setup();
        // The reconfigure Equipment is itself an Artifact Creature (Equipment).
        let equip = make_creature(&mut state, "Reconfigure Equipment", 0, 0, PlayerId(0));
        {
            let obj = state.objects.get_mut(&equip).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".to_string());
            obj.base_card_types = obj.card_types.clone();
            // Apply the synthesized continuous static onto the base set so the
            // Step-1 layer reset preserves it across passes.
            obj.base_static_definitions = Arc::new(face.static_abilities.clone());
        }
        let host = make_creature(&mut state, "Host Creature", 2, 2, PlayerId(0));

        // Unattached: still a creature.
        evaluate_layers(&mut state);
        assert!(
            state.objects[&equip]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "unattached reconfigure Equipment is a creature"
        );

        // Attached to a creature: stops being a creature (CR 702.151b).
        attach_to(&mut state, equip, host);
        evaluate_layers(&mut state);
        assert!(
            !state.objects[&equip]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "CR 702.151b: attached reconfigure Equipment stops being a creature"
        );
        // Still an Artifact Equipment — only the Creature type is removed.
        assert!(
            state.objects[&equip]
                .card_types
                .core_types
                .contains(&CoreType::Artifact),
            "only the Creature type is removed (Layer 4 RemoveType)"
        );

        // Unattached again: a creature once more.
        unattach(&mut state, equip);
        evaluate_layers(&mut state);
        assert!(
            state.objects[&equip]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "unattaching restores the creature type"
        );
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
        state.layers_dirty.mark_full();
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
        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

    // CR 205.1a (SET replace) + CR 613.1d (Layer 4) + CR 613.7a (intra-static
    // written order): Conspiracy's "Creatures you control are the chosen type"
    // REPLACES the existing creature subtypes. The composed static
    // [RemoveAllSubtypes{Creature}, AddChosenSubtype{CreatureType}] must wipe the
    // base "Goblin" subtype (membership resolved against state.all_creature_types)
    // and re-add the chosen "Zombie" type, in written order, so Zombie survives.
    #[test]
    fn test_chosen_creature_type_sets_subtype() {
        use crate::types::ability::ChosenAttribute;

        let mut state = setup();
        // RemoveAllSubtypes{Creature} resolves creature-type membership against
        // state.all_creature_types — both the wiped Goblin and the re-added Zombie
        // must be known.
        state.all_creature_types = vec!["Goblin".to_string(), "Zombie".to_string()];

        let source = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Conspiracy".to_string(),
            Zone::Battlefield,
        );
        let goblin = make_creature(&mut state, "Goblin", 1, 1, PlayerId(0));
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&goblin).unwrap();
            obj.card_types.subtypes.push("Goblin".to_string());
            // Mirror into base so the layer-evaluation reset (card_types =
            // base_card_types) doesn't drop the printed Goblin subtype.
            obj.base_card_types.subtypes.push("Goblin".to_string());
            obj.timestamp = ts;
        }
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.chosen_attributes
                .push(ChosenAttribute::CreatureType("Zombie".to_string()));
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ))
                    .modifications(vec![
                        ContinuousModification::RemoveAllSubtypes {
                            set: crate::types::card_type::SubtypeSet::Creature,
                        },
                        ContinuousModification::AddChosenSubtype {
                            kind: ChosenSubtypeKind::CreatureType,
                        },
                    ]),
            );
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&goblin).unwrap();
        assert!(
            obj.card_types.subtypes.contains(&"Zombie".to_string()),
            "Creature should gain the chosen Zombie subtype: {:?}",
            obj.card_types.subtypes
        );
        assert!(
            !obj.card_types.subtypes.contains(&"Goblin".to_string()),
            "SET form must wipe the original Goblin subtype: {:?}",
            obj.card_types.subtypes
        );
    }

    // CR 608.2d + CR 613.1f: Urborg's "loses [chosen ability] until end of
    // turn" — the chosen Keyword is stored on the source's `chosen_attributes`
    // and read back by `RemoveChosenKeyword` at layer evaluation time. The
    // recipient (target creature) is the affected object; the source (Urborg)
    // owns the choice. Same indirection pattern as `AddChosenColor`.
    #[test]
    fn test_remove_chosen_keyword_strips_first_strike_from_target() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::keywords::Keyword;

        let mut state = setup();

        // Source (e.g., Urborg) — carries the chosen keyword attribute.
        let urborg = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Urborg".to_string(),
            Zone::Battlefield,
        );
        // Recipient (target creature) — has First Strike printed; we expect
        // the layered view to strip it.
        let target = make_creature(&mut state, "Knight", 2, 2, PlayerId(0));
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.timestamp = ts;
            obj.base_keywords.push(Keyword::FirstStrike);
            obj.keywords.push(Keyword::FirstStrike);
        }
        {
            let obj = state.objects.get_mut(&urborg).unwrap();
            obj.chosen_attributes
                .push(ChosenAttribute::Keyword(Keyword::FirstStrike));
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target })
                    .modifications(vec![ContinuousModification::RemoveChosenKeyword]),
            );
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&target).unwrap();
        assert!(
            !obj.keywords.contains(&Keyword::FirstStrike),
            "RemoveChosenKeyword should strip First Strike from the target"
        );
    }

    // CR 608.2d + CR 613.1f + CR 702.14: Swampwalk is `Landwalk("Swamp")`
    // and is a *distinct* keyword from islandwalk per CR 702.14 — the
    // chosen-keyword surface must remove only the exact parameterized
    // variant chosen at resolution time, leaving other landwalk variants
    // on the same creature intact. This test guards the `PartialEq`-based
    // stripping in the `RemoveChosenKeyword` arm against future regression
    // to discriminant-only matching.
    #[test]
    fn test_remove_chosen_keyword_strips_only_chosen_landwalk_variant() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::keywords::Keyword;

        let mut state = setup();

        let urborg = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Urborg".to_string(),
            Zone::Battlefield,
        );
        let target = make_creature(&mut state, "Marsh Stalker", 2, 2, PlayerId(0));
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.timestamp = ts;
            obj.base_keywords
                .push(Keyword::Landwalk("Swamp".to_string()));
            obj.base_keywords
                .push(Keyword::Landwalk("Island".to_string()));
            obj.keywords.push(Keyword::Landwalk("Swamp".to_string()));
            obj.keywords.push(Keyword::Landwalk("Island".to_string()));
        }
        {
            let obj = state.objects.get_mut(&urborg).unwrap();
            obj.chosen_attributes
                .push(ChosenAttribute::Keyword(Keyword::Landwalk(
                    "Swamp".to_string(),
                )));
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target })
                    .modifications(vec![ContinuousModification::RemoveChosenKeyword]),
            );
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&target).unwrap();
        assert!(
            !obj.keywords
                .contains(&Keyword::Landwalk("Swamp".to_string())),
            "RemoveChosenKeyword should strip the chosen Swampwalk"
        );
        assert!(
            obj.keywords
                .contains(&Keyword::Landwalk("Island".to_string())),
            "RemoveChosenKeyword must NOT strip the non-chosen Islandwalk (CR 702.14)"
        );
    }

    // No-op safety: when the source has no `ChosenAttribute::Keyword` stored,
    // `RemoveChosenKeyword` must NOT panic and must NOT touch the recipient.
    // Mirrors the unresolved-attribute behavior of `AddChosenColor`.
    #[test]
    fn test_remove_chosen_keyword_without_choice_is_noop() {
        use crate::types::keywords::Keyword;

        let mut state = setup();

        let urborg = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Urborg".to_string(),
            Zone::Battlefield,
        );
        let target = make_creature(&mut state, "Knight", 2, 2, PlayerId(0));
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.timestamp = ts;
            obj.base_keywords.push(Keyword::FirstStrike);
            obj.keywords.push(Keyword::FirstStrike);
        }
        {
            // Note: source has no chosen_attributes pushed.
            let obj = state.objects.get_mut(&urborg).unwrap();
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target })
                    .modifications(vec![ContinuousModification::RemoveChosenKeyword]),
            );
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&target).unwrap();
        assert!(
            obj.keywords.contains(&Keyword::FirstStrike),
            "RemoveChosenKeyword with no stored choice should be a no-op"
        );
    }

    /// Issue #1593 — Abomination of Llanowar: end-to-end (real parser → layers).
    /// "~'s power and toughness are each equal to the number of Elves you control
    ///  plus the number of Elf cards in your graveyard." The graveyard term must
    ///  be ADDED, not dropped (the reported "not adding from graveyard").
    #[test]
    fn issue_1593_abomination_of_llanowar_runtime_counts_elves_and_graveyard() {
        let mut state = setup();

        // Abomination of Llanowar is itself an Elf (Elf Horror), so it counts
        // toward "Elves you control". Parse its CDA with the REAL parser so this
        // test exercises the full parser→runtime pipeline.
        let abomination = make_creature(&mut state, "Abomination of Llanowar", 0, 0, PlayerId(0));
        {
            let def = crate::parser::oracle_static::parse_static_line(
                "Abomination of Llanowar's power and toughness are each equal to the number of Elves you control plus the number of Elf cards in your graveyard.",
            )
            .expect("CDA must parse");
            let obj = state.objects.get_mut(&abomination).unwrap();
            obj.card_types.subtypes.push("Elf".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions.push(def);
        }

        // Baseline: only Abomination itself is an Elf you control, empty graveyard.
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        let obj = state.objects.get(&abomination).unwrap();
        assert_eq!(
            obj.power,
            Some(1),
            "1 Elf you control (itself), 0 graveyard"
        );
        assert_eq!(obj.toughness, Some(1));

        // Add 2 more Elf creatures you control (total 3 Elves you control) and a
        // non-Elf creature that must NOT be counted.
        for _ in 0..2 {
            let elf = make_creature(&mut state, "Llanowar Elves", 1, 1, PlayerId(0));
            let o = state.objects.get_mut(&elf).unwrap();
            o.card_types.subtypes.push("Elf".to_string());
            o.base_card_types = o.card_types.clone();
        }
        let _bear = make_creature(&mut state, "Grizzly Bears", 2, 2, PlayerId(0));

        // Add 4 Elf cards to YOUR graveyard, plus a non-Elf card and an Elf card
        // in the OPPONENT's graveyard (neither must be counted). `create_object`
        // already registers each object in the correct zone via `add_to_zone`,
        // so we must NOT push to the graveyard list again (that would double-count).
        // CR 404.2: Graveyard membership is player-scoped. Deliberately diverge
        // owner/controller below so this test fails if graveyard counts use
        // controller instead of owner.
        for _ in 0..4 {
            let id = create_object(
                &mut state,
                CardId(0),
                PlayerId(0),
                "Dead Elf".to_string(),
                Zone::Graveyard,
            );
            let o = state.objects.get_mut(&id).unwrap();
            o.controller = PlayerId(1);
            o.card_types.core_types.push(CoreType::Creature);
            o.card_types.subtypes.push("Elf".to_string());
            o.base_card_types = o.card_types.clone();
        }
        let non_elf = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Dead Bear".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&non_elf)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&non_elf).unwrap().controller = PlayerId(1);

        let opp_elf = create_object(
            &mut state,
            CardId(0),
            PlayerId(1),
            "Opp Dead Elf".to_string(),
            Zone::Graveyard,
        );
        {
            let o = state.objects.get_mut(&opp_elf).unwrap();
            o.controller = PlayerId(0);
            o.card_types.core_types.push(CoreType::Creature);
            o.card_types.subtypes.push("Elf".to_string());
            o.base_card_types = o.card_types.clone();
        }

        // Expected: 3 Elves you control + 4 Elf cards in YOUR graveyard = 7.
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        let obj = state.objects.get(&abomination).unwrap();
        assert_eq!(
            obj.power,
            Some(7),
            "3 Elves you control + 4 Elf cards in graveyard = 7"
        );
        assert_eq!(obj.toughness, Some(7));
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
                                qty: QuantityRef::DistinctCardTypes {
                                    source: crate::types::ability::CardTypeSetSource::Zone {
                                        zone: ZoneRef::Graveyard,
                                        scope: CountScope::All,
                                    },
                                },
                            },
                        },
                        ContinuousModification::SetDynamicToughness {
                            value: QuantityExpr::Offset {
                                inner: Box::new(QuantityExpr::Ref {
                                    qty: QuantityRef::DistinctCardTypes {
                                        source: crate::types::ability::CardTypeSetSource::Zone {
                                            zone: ZoneRef::Graveyard,
                                            scope: CountScope::All,
                                        },
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
        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

    /// CR 202.3 + CR 208.2a + CR 604.3 / CR 613.4a: Dragon Man, Reformed Robot's
    /// CDA power = the greatest mana value among (noncreature permanents you
    /// control) and (noncreature cards in your graveyard). Drives the real
    /// parser (`parse_cda_quantity`) to build the AST, then evaluates through the
    /// live layer system — revert the "A and B" conjunction arm and
    /// `parse_cda_quantity` returns None, so the static can't be built and the
    /// test panics. Creature cards are excluded from both arms; empty → 0
    /// (CR 208.2a); a +1/+1 counter stacks on top of the CDA base (layer 7c).
    #[test]
    fn dragon_man_cda_power_greatest_mana_value_across_zones() {
        use crate::parser::oracle_quantity::parse_cda_quantity;

        let mut state = setup();

        let value = parse_cda_quantity(
            "the greatest mana value among noncreature permanents you control and noncreature cards in your graveyard",
        )
        .expect("conjunction CDA quantity must parse");

        // Dragon Man itself (a creature) carries the self-ref CDA.
        let dragon_man = make_creature(&mut state, "Dragon Man, Reformed Robot", 0, 4, PlayerId(0));
        state
            .objects
            .get_mut(&dragon_man)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::SetDynamicPower { value }])
                    .cda(),
            );

        // Empty board + graveyard → 0 (CR 208.2a "use 0").
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&dragon_man).unwrap().power,
            Some(0),
            "no noncreature permanents/cards → power 0"
        );

        // MV-5 noncreature permanent on the battlefield you control.
        let bf_artifact = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Mox".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bf_artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![],
                generic: 5,
            };
        }
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&dragon_man).unwrap().power,
            Some(5),
            "battlefield-only max → power 5"
        );

        // MV-7 noncreature card in your graveyard (greater than the bf 5).
        let gy_artifact = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Spent Relic".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![],
                generic: 7,
            };
        }
        // MV-9 *creature* card in your graveyard — must be excluded (noncreature).
        let gy_creature = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Dead Titan".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![],
                generic: 9,
            };
        }
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&dragon_man).unwrap().power,
            Some(7),
            "graveyard noncreature 7 beats bf 5; creature 9 excluded → power 7"
        );

        // +1/+1 counter stacks on top of the CDA base (proves CDA is the base).
        state
            .objects
            .get_mut(&dragon_man)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&dragon_man).unwrap().power,
            Some(8),
            "CDA 7 + one +1/+1 counter → power 8"
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

    /// CR 613.1f + CR 113.3: A permanent with "has all activated abilities of all
    /// cards exiled with it" (Myr Welder) gains each exiled card's activated
    /// ability after layer evaluation, via the dynamic GrantAllActivatedAbilitiesOf
    /// expansion. Issue #3101.
    #[test]
    fn grants_all_activated_abilities_of_cards_exiled_with_it() {
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr,
        };
        use crate::types::game_state::{ExileLink, ExileLinkKind};

        let mut state = setup();

        // Host: Myr Welder-like artifact carrying the self-static.
        let host = create_object(
            &mut state,
            CardId(700),
            PlayerId(0),
            "Myr Welder".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions = vec![StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
                    source: TargetFilter::ExiledBySource,
                }])]
            .into();
        }

        // A card exiled with the host, carrying a "{T}: deal 1 damage" activated
        // ability.
        let exiled = create_object(
            &mut state,
            CardId(701),
            PlayerId(0),
            "Exiled Source".to_string(),
            Zone::Exile,
        );
        let granted = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut state.objects.get_mut(&exiled).unwrap().abilities).push(granted.clone());
        state.exile_links.push(ExileLink {
            exiled_id: exiled,
            source_id: host,
            kind: ExileLinkKind::TrackedBySource,
        });

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let host_obj = state.objects.get(&host).unwrap();
        assert!(
            host_obj.abilities.iter().any(|a| a == &granted),
            "Myr Welder must gain the exiled card's activated ability; got {:?}",
            host_obj.abilities
        );
    }

    /// CR 613.1f + CR 201.2: Marvin, Murderous Mimic — "Marvin has all activated
    /// abilities of creatures you control that don't have the same name as this
    /// creature." Built end-to-end through the real parser
    /// (`parse_oracle_text` → `GrantAllActivatedAbilitiesOf { Typed(creature,
    /// You, [Not{SameName}]) }`) and the real `evaluate_layers` expansion.
    ///
    /// Discriminating: a DIFFERENT-named creature you control donates its
    /// activated ability to Marvin, while a SAME-named creature's ability is
    /// excluded by `Not{SameName}` and an OPPONENT's creature is excluded by the
    /// `controller=You` axis. Reverting the parser's source-set arm (so the grant
    /// no longer routes to the `Not{SameName}` typed filter) flips the positive
    /// assertion; dropping the `Not{SameName}` predicate flips the
    /// same-name-exclusion assertion.
    #[test]
    fn marvin_gains_differently_named_creature_abilities_only() {
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr,
        };

        let mut state = setup();

        // Marvin carries the parser-produced static.
        let marvin = make_creature(&mut state, "Marvin, Murderous Mimic", 0, 3, PlayerId(0));
        let parsed = parse_oracle_text(
            "Marvin has all activated abilities of creatures you control that \
             don't have the same name as this creature.",
            "Marvin, Murderous Mimic",
            &[],
            &["Artifact".into(), "Creature".into()],
            &[],
        );
        assert_eq!(
            parsed.statics.len(),
            1,
            "Marvin parses to exactly one static; got {:?}",
            parsed.statics
        );
        {
            let obj = state.objects.get_mut(&marvin).unwrap();
            obj.card_types.core_types.insert(0, CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions = parsed.statics.clone().into();
        }

        // A differently-named creature you control with a {T}: gain 2 life
        // ability — its ability SHOULD be granted to Marvin.
        let donor = make_creature(&mut state, "Helpful Donor", 2, 2, PlayerId(0));
        let donor_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut state.objects.get_mut(&donor).unwrap().abilities)
            .push(donor_ability.clone());

        // A SAME-named creature you control with a {T}: gain 5 life ability —
        // excluded by `Not{SameName}`.
        let twin = make_creature(&mut state, "Marvin, Murderous Mimic", 0, 3, PlayerId(0));
        let twin_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut state.objects.get_mut(&twin).unwrap().abilities)
            .push(twin_ability.clone());

        // An OPPONENT's creature with a {T}: gain 9 life ability — excluded by
        // the `controller=You` axis.
        let foe = make_creature(&mut state, "Opposing Donor", 2, 2, PlayerId(1));
        let foe_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 9 },
                player: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut state.objects.get_mut(&foe).unwrap().abilities)
            .push(foe_ability.clone());

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let marvin_obj = state.objects.get(&marvin).unwrap();
        assert!(
            marvin_obj.abilities.iter().any(|a| a == &donor_ability),
            "Marvin must gain the differently-named creature's activated \
             ability; got {:?}",
            marvin_obj.abilities
        );
        assert!(
            !marvin_obj.abilities.iter().any(|a| a == &twin_ability),
            "Marvin must NOT gain a same-named creature's ability \
             (Not{{SameName}}); got {:?}",
            marvin_obj.abilities
        );
        assert!(
            !marvin_obj.abilities.iter().any(|a| a == &foe_ability),
            "Marvin must NOT gain an opponent's creature's ability \
             (controller=You); got {:?}",
            marvin_obj.abilities
        );
    }

    /// CR 613.1f + CR 205.3: Agatha's Soul Cauldron — "Creatures you control with
    /// +1/+1 counters on them have all activated abilities of all creature cards
    /// exiled with Agatha's Soul Cauldron." Built end-to-end through the real
    /// parser (`GrantAllActivatedAbilitiesOf { And[Typed(creature),
    /// ExiledBySource] }` on a counter-gated affected filter) and the real
    /// `evaluate_layers` expansion.
    ///
    /// Discriminating along two axes the new source-set arm introduces:
    ///   - source typing: an exiled CREATURE card donates its ability, while a
    ///     non-creature exiled card (the `And` excludes it) does not;
    ///   - affected gating: a counter-bearing creature receives the grant, while
    ///     a counterless creature does not.
    ///
    /// Reverting the typed intersection (back to bare `ExiledBySource`) flips the
    /// non-creature-exclusion assertion. Reverting the host-relative source
    /// resolution in `expand_granted_activated_abilities` (so `ExiledBySource`
    /// reads the recipient creature's exile links instead of the cauldron's)
    /// flips the positive donation assertion — the grant becomes a runtime no-op.
    #[test]
    fn agatha_grants_only_creature_card_abilities_to_countered_creatures() {
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr,
        };
        use crate::types::counter::CounterType;
        use crate::types::game_state::{ExileLink, ExileLinkKind};

        let mut state = setup();

        // Host cauldron carries the parser-produced static.
        let cauldron = create_object(
            &mut state,
            CardId(800),
            PlayerId(0),
            "Agatha's Soul Cauldron".to_string(),
            Zone::Battlefield,
        );
        let parsed = parse_oracle_text(
            "Creatures you control with +1/+1 counters on them have all \
             activated abilities of all creature cards exiled with Agatha's \
             Soul Cauldron.",
            "Agatha's Soul Cauldron",
            &[],
            &["Artifact".into()],
            &[],
        );
        assert_eq!(
            parsed.statics.len(),
            1,
            "Agatha's grant clause parses to one static; got {:?}",
            parsed.statics
        );
        {
            let obj = state.objects.get_mut(&cauldron).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions = parsed.statics.clone().into();
        }

        // Exiled CREATURE card with {T}: gain 2 life — SHOULD be donated.
        let exiled_creature = create_object(
            &mut state,
            CardId(801),
            PlayerId(0),
            "Exiled Beast".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&exiled_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }
        let creature_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut state.objects.get_mut(&exiled_creature).unwrap().abilities)
            .push(creature_ability.clone());
        state.exile_links.push(ExileLink {
            exiled_id: exiled_creature,
            source_id: cauldron,
            kind: ExileLinkKind::TrackedBySource,
        });

        // Exiled NON-creature (artifact) card with {T}: gain 7 life — excluded by
        // the `And[Typed(creature), ExiledBySource]` source filter.
        let exiled_artifact = create_object(
            &mut state,
            CardId(802),
            PlayerId(0),
            "Exiled Relic".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&exiled_artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
        }
        let artifact_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 7 },
                player: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut state.objects.get_mut(&exiled_artifact).unwrap().abilities)
            .push(artifact_ability.clone());
        state.exile_links.push(ExileLink {
            exiled_id: exiled_artifact,
            source_id: cauldron,
            kind: ExileLinkKind::TrackedBySource,
        });

        // A creature you control WITH a +1/+1 counter — SHOULD receive the grant.
        let countered = make_creature(&mut state, "Countered Creature", 1, 1, PlayerId(0));
        state
            .objects
            .get_mut(&countered)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        // A creature you control WITHOUT counters — excluded by the affected
        // counter gate.
        let counterless = make_creature(&mut state, "Plain Creature", 1, 1, PlayerId(0));

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let countered_obj = state.objects.get(&countered).unwrap();
        assert!(
            countered_obj
                .abilities
                .iter()
                .any(|a| a == &creature_ability),
            "A countered creature must gain the exiled creature card's ability; \
             got {:?}",
            countered_obj.abilities
        );
        assert!(
            !countered_obj
                .abilities
                .iter()
                .any(|a| a == &artifact_ability),
            "A countered creature must NOT gain the non-creature exiled card's \
             ability (And[creature, ExiledBySource]); got {:?}",
            countered_obj.abilities
        );

        let counterless_obj = state.objects.get(&counterless).unwrap();
        assert!(
            !counterless_obj
                .abilities
                .iter()
                .any(|a| a == &creature_ability),
            "A counterless creature must NOT receive the grant; got {:?}",
            counterless_obj.abilities
        );
    }

    /// CR 613.1f + CR 603.3: Runtime zone-filter regression for battlefield
    /// source-set grants ("all creatures your opponents control"). The parser
    /// encodes `InZone { Battlefield }` in the source filter; this test
    /// proves `expand_granted_activated_abilities` honours it at runtime by
    /// building `provider_ids` from ALL objects in `state.objects` and
    /// relying on `matches_target_filter` to exclude off-battlefield objects.
    ///
    /// Discriminating: an identical provider creature in HAND must not donate
    /// its activated abilities even though it appears in `state.objects`, while
    /// the same creature ON THE BATTLEFIELD still does.
    ///
    /// Reverting `InZone { Battlefield }` from the parser arm (or removing it
    /// from the source filter here) causes the hand-provider assertion to fail,
    /// flipping the non-donation assertion. Replacing `state.objects.keys()` in
    /// `expand_granted_activated_abilities` with a battlefield-only scan would
    /// trivially make this green without the filter — but that would break the
    /// graveyard provider arms (Necrotic Ooze / "all creature cards in all
    /// graveyards") which scan off-battlefield zones deliberately.
    #[test]
    fn off_battlefield_provider_does_not_donate_activated_abilities() {
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr,
        };

        let mut state = setup();

        // Host: a creature on the battlefield (player 0) carrying the
        // "has all activated abilities of all creatures your opponents
        // control" static, parsed end-to-end.
        let host = make_creature(&mut state, "Drana and Linvala", 3, 4, PlayerId(0));
        let parsed = parse_oracle_text(
            "Drana and Linvala have all activated abilities of all creatures \
             your opponents control.",
            "Drana and Linvala",
            &[],
            &["Creature".into(), "Legendary".into()],
            &[],
        );
        assert_eq!(
            parsed.statics.len(),
            1,
            "grant-opponents-control clause parses to one static; got {:?}",
            parsed.statics
        );
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.static_definitions = parsed.statics.clone().into();
        }

        // Activated ability template: {T}: gain 3 life.
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap);

        // BATTLEFIELD provider: opponent creature on the battlefield —
        // InZone { Battlefield } must match → ability IS donated to host.
        let bf_provider = make_creature(&mut state, "Battlefield Beast", 2, 2, PlayerId(1));
        Arc::make_mut(&mut state.objects.get_mut(&bf_provider).unwrap().abilities)
            .push(ability.clone());

        // HAND provider: identical opponent creature in hand —
        // InZone { Battlefield } must NOT match → ability is NOT donated.
        let hand_provider = create_object(
            &mut state,
            CardId(902),
            PlayerId(1),
            "Hand Beast".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&hand_provider).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }
        Arc::make_mut(&mut state.objects.get_mut(&hand_provider).unwrap().abilities)
            .push(ability.clone());

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let host_obj = state.objects.get(&host).unwrap();
        assert!(
            host_obj.abilities.iter().any(|a| a == &ability),
            "host must gain the battlefield opponent creature's activated \
             ability (InZone{{Battlefield}} matches); got {:?}",
            host_obj.abilities
        );
        // The hand provider has the same ability; if InZone{Battlefield} is not
        // enforced at runtime, the ability would appear twice in host_obj.abilities
        // (once per provider). We assert exactly ONE grant was applied.
        let grant_count = host_obj.abilities.iter().filter(|a| *a == &ability).count();
        assert_eq!(
            grant_count, 1,
            "hand provider must NOT donate: expected 1 grant (from bf_provider), \
             got {grant_count} (hand_provider donated a duplicate)"
        );
    }

    /// CR 109.5 + CR 604.1: `expand_granted_activated_abilities` memoizes the
    /// matching-provider set per recipient controller. With K recipients sharing
    /// ONE controller, the provider filter sweep runs exactly once (M scans, one
    /// per object), and every recipient still gains each provider's activated
    /// ability. Reverting the per-controller cache restores the K×M sweep,
    /// flipping the `granted_ability_provider_scans == M` assertion.
    #[test]
    fn granted_activated_abilities_scan_providers_once_per_controller() {
        use crate::types::game_state::{ExileLink, ExileLinkKind};

        let mut state = setup();

        // Host cauldron-like artifact (NOT a creature, so not a recipient of its
        // own grant) carrying "creatures you control have all activated abilities
        // of all cards exiled with this".
        let host = create_object(
            &mut state,
            CardId(800),
            PlayerId(0),
            "Soul Cauldron".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions = vec![StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ))
                .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
                    source: TargetFilter::ExiledBySource,
                }])]
            .into();
        }

        // K=8 recipient creatures, all controlled by player 0.
        let recipients: Vec<ObjectId> = (0..8)
            .map(|i| make_creature(&mut state, &format!("Recipient {i}"), 1, 1, PlayerId(0)))
            .collect();

        // Two exiled provider cards, each carrying one distinct activated ability,
        // both tracked by the host.
        let mut provider_abilities = Vec::new();
        for i in 0..2 {
            let provider = create_object(
                &mut state,
                CardId(810 + i),
                PlayerId(0),
                format!("Exiled Source {i}"),
                Zone::Exile,
            );
            let ability = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed {
                        value: 2 + i as i32,
                    },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Tap);
            Arc::make_mut(&mut state.objects.get_mut(&provider).unwrap().abilities)
                .push(ability.clone());
            state.exile_links.push(ExileLink {
                exiled_id: provider,
                source_id: host,
                kind: ExileLinkKind::TrackedBySource,
            });
            provider_abilities.push(ability);
        }

        let m = state.objects.len() as u64;
        let affected = TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));
        let source = TargetFilter::ExiledBySource;

        // Single deterministic invocation of the seam so the per-pass scan count
        // is unambiguous (evaluate_layers re-runs the expansion each pass; this
        // isolates one pass). Single controller ⟹ exactly M provider scans.
        crate::game::perf_counters::reset();
        let effects = expand_granted_activated_abilities(&state, host, 1, &affected, &source);
        let scans = crate::game::perf_counters::snapshot().granted_ability_provider_scans;
        assert_eq!(
            scans, m,
            "single controller must scan the provider set exactly once (M objects)"
        );
        // 8 recipients × 2 provider abilities = 16 grant effects.
        assert_eq!(
            effects.len(),
            16,
            "every recipient gains both provider abilities"
        );

        // Behavioral equivalence through the production entry point.
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        for &recipient in &recipients {
            let obj = state.objects.get(&recipient).unwrap();
            for ability in &provider_abilities {
                assert!(
                    obj.abilities.iter().any(|a| a == ability),
                    "each recipient must gain every exiled provider's activated ability"
                );
            }
        }
    }

    /// CR 109.5: the provider cache is keyed per controller, not over-collapsed.
    /// Two recipients with DIFFERENT controllers force TWO full provider sweeps
    /// (2×M scans), and each still receives the controller-independent
    /// `ExiledBySource` provider abilities. Reverting the cache key to a single
    /// shared entry would under-count to M.
    #[test]
    fn granted_activated_abilities_scan_per_distinct_controller() {
        use crate::types::game_state::{ExileLink, ExileLinkKind};

        let mut state = setup();

        // Host grants to ALL creatures (no controller restriction) so recipients
        // of either controller match the affected filter.
        let host = create_object(
            &mut state,
            CardId(800),
            PlayerId(0),
            "Soul Cauldron".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
            obj.static_definitions = vec![StaticDefinition::continuous()
                .affected(TargetFilter::Typed(TypedFilter::creature()))
                .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
                    source: TargetFilter::ExiledBySource,
                }])]
            .into();
        }

        // One recipient per controller.
        let recipient_p0 = make_creature(&mut state, "Recipient P0", 1, 1, PlayerId(0));
        let recipient_p1 = make_creature(&mut state, "Recipient P1", 1, 1, PlayerId(1));

        let provider = create_object(
            &mut state,
            CardId(810),
            PlayerId(0),
            "Exiled Source".to_string(),
            Zone::Exile,
        );
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut state.objects.get_mut(&provider).unwrap().abilities)
            .push(ability.clone());
        state.exile_links.push(ExileLink {
            exiled_id: provider,
            source_id: host,
            kind: ExileLinkKind::TrackedBySource,
        });

        let m = state.objects.len() as u64;
        let affected = TargetFilter::Typed(TypedFilter::creature());
        let source = TargetFilter::ExiledBySource;

        // Single deterministic invocation: two distinct recipient controllers ⟹
        // two cache entries ⟹ two full provider sweeps (2×M scans).
        crate::game::perf_counters::reset();
        let effects = expand_granted_activated_abilities(&state, host, 1, &affected, &source);
        let scans = crate::game::perf_counters::snapshot().granted_ability_provider_scans;
        assert_eq!(
            scans,
            2 * m,
            "two distinct controllers must each trigger one full provider sweep"
        );
        // 2 recipients × 1 provider ability = 2 grant effects.
        assert_eq!(
            effects.len(),
            2,
            "each controller's recipient gains the provider ability"
        );

        // Behavioral equivalence through the production entry point.
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        for recipient in [recipient_p0, recipient_p1] {
            let obj = state.objects.get(&recipient).unwrap();
            assert!(
                obj.abilities.iter().any(|a| a == &ability),
                "each controller's recipient must gain the exiled provider's ability"
            );
        }
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
        state.layers_dirty.mark_full();
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

    /// CR 114.3 + CR 613.1f: End-to-end test for Koth of the Hammer's −5
    /// emblem. The emblem text nests the granted activated ability in single
    /// quotes one level deep (`"Mountains you control have '{T}: …'"`); if the
    /// emblem parser corrupts or drops those quotes the static lowers to an
    /// inert `EmblemStatic` blob and the Mountains never receive the ability
    /// (the reported bug). This drives the real parser, installs the parsed
    /// static on a command-zone emblem, and verifies a Mountain you control
    /// actually gains the activated damage ability (layer-6 ability-adding)
    /// while an opponent's Mountain and a non-Mountain land do not.
    #[test]
    fn koth_emblem_grants_activated_ability_to_controlled_mountains() {
        use crate::types::ability::AbilityKind;

        let mut state = setup();

        let make_basic_land =
            |state: &mut GameState, name: &str, subtype: &str, player: PlayerId| {
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
                obj.card_types.subtypes.push(subtype.to_string());
                obj.base_card_types = obj.card_types.clone();
                obj.timestamp = ts;
                id
            };

        let my_mountain = make_basic_land(&mut state, "Mountain", "Mountain", PlayerId(0));
        let opp_mountain = make_basic_land(&mut state, "Opp Mountain", "Mountain", PlayerId(1));
        let my_forest = make_basic_land(&mut state, "Forest", "Forest", PlayerId(0));

        // Drive the real parser on Koth's emblem Oracle text.
        let effect = crate::parser::oracle_effect::parse_effect(
            "You get an emblem with \"Mountains you control have '{T}: This land deals 1 damage to any target.'\"",
        );
        let crate::types::ability::Effect::CreateEmblem { statics, triggers } = effect else {
            panic!("expected CreateEmblem effect from Koth's emblem text");
        };
        assert!(triggers.is_empty(), "Koth emblem is a static grant");
        assert_eq!(
            statics.len(),
            1,
            "expected the granted static, got {statics:?}"
        );

        // CR 114.4: Install on a command-zone emblem controlled by Player 0;
        // an emblem's abilities function from the command zone.
        let emblem_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Emblem".to_string(),
            Zone::Command,
        );
        {
            let emblem = state.objects.get_mut(&emblem_id).unwrap();
            emblem.is_emblem = true;
            emblem.static_definitions = statics.into();
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let grants_damage_ability = |state: &GameState, id: ObjectId| {
            state.objects.get(&id).unwrap().abilities.iter().any(|a| {
                a.kind == AbilityKind::Activated
                    && matches!(&*a.effect, crate::types::ability::Effect::DealDamage { .. })
            })
        };

        assert!(
            grants_damage_ability(&state, my_mountain),
            "a Mountain you control must gain the granted '{{T}}: deal 1 damage' ability"
        );
        // CR 613.1f: the layer-6 grant is scoped to permanents the emblem's
        // controller controls.
        assert!(
            !grants_damage_ability(&state, opp_mountain),
            "an opponent's Mountain must NOT gain the ability"
        );
        assert!(
            !grants_damage_ability(&state, my_forest),
            "a non-Mountain land must NOT gain the ability"
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

    // CR 113.6b + CR 408: SourceInZone evaluator — used by the Eminence /
    // Anger / Squee class of statics that name a non-battlefield zone.
    #[test]
    fn evaluate_source_in_zone_command_true_when_in_command_zone() {
        let mut state = setup();
        let id = make_creature(&mut state, "Cmdr", 2, 2, PlayerId(0));
        // Move from battlefield to command zone for this scenario.
        state.objects.get_mut(&id).unwrap().zone = Zone::Command;
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceInZone {
                zone: Zone::Command
            },
            PlayerId(0),
            id,
        ));
    }

    /// CR 113.6b: An Eminence-style Or-disjunction ("~ is in the command zone
    /// or on the battlefield") must evaluate true for either zone individually
    /// and false outside both.
    #[test]
    fn evaluate_source_in_zone_or_disjunction_command_or_battlefield() {
        let mut state = setup();
        let id = make_creature(&mut state, "Cmdr", 2, 2, PlayerId(0));
        let cond = StaticCondition::Or {
            conditions: vec![
                StaticCondition::SourceInZone {
                    zone: Zone::Command,
                },
                StaticCondition::SourceInZone {
                    zone: Zone::Battlefield,
                },
            ],
        };
        // On battlefield (created here) → true.
        assert!(evaluate_condition_for_test(&state, &cond, PlayerId(0), id));
        // Move to command zone → still true.
        state.objects.get_mut(&id).unwrap().zone = Zone::Command;
        assert!(evaluate_condition_for_test(&state, &cond, PlayerId(0), id));
        // Move to graveyard → false (neither zone).
        state.objects.get_mut(&id).unwrap().zone = Zone::Graveyard;
        assert!(!evaluate_condition_for_test(&state, &cond, PlayerId(0), id));
        // Exile → false.
        state.objects.get_mut(&id).unwrap().zone = Zone::Exile;
        assert!(!evaluate_condition_for_test(&state, &cond, PlayerId(0), id));
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

    // -- RecipientAttackingOwnerTarget evaluator tests (CR 509.1b / CR 506.2 / CR 108.3) --

    #[test]
    fn recipient_attacking_owner_target_owner_player_matches_owner() {
        use crate::game::combat::{AttackerInfo, CombatState};
        use crate::types::triggers::AttackTargetFilter;
        let mut state = setup();
        // Owned by B(1), controlled by A(0).
        let attacker = make_creature(&mut state, "Donated", 4, 4, PlayerId(1));
        state.objects.get_mut(&attacker).unwrap().controller = PlayerId(0);
        let condition = StaticCondition::RecipientAttackingOwnerTarget {
            target: AttackTargetFilter::Owner,
        };

        // Attacking its owner (B) → positive condition true.
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });
        assert!(evaluate_condition_with_recipient(
            &state,
            &condition,
            PlayerId(0),
            attacker,
            attacker,
        ));

        // Attacking a non-owner player (A) → false.
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(0))],
            ..Default::default()
        });
        assert!(!evaluate_condition_with_recipient(
            &state,
            &condition,
            PlayerId(0),
            attacker,
            attacker,
        ));
    }

    #[test]
    fn recipient_attacking_owner_target_or_planeswalker_matches_owner_controlled_pw() {
        use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
        use crate::types::triggers::AttackTargetFilter;
        let mut state = setup();
        let attacker = make_creature(&mut state, "Donated", 4, 4, PlayerId(1));
        state.objects.get_mut(&attacker).unwrap().controller = PlayerId(0);
        let owner_pw = make_creature(&mut state, "Owner Walker", 0, 4, PlayerId(1));
        let controller_pw = make_creature(&mut state, "Controller Walker", 0, 4, PlayerId(0));
        let condition = StaticCondition::RecipientAttackingOwnerTarget {
            target: AttackTargetFilter::OwnerOrPlaneswalker,
        };

        // Planeswalker the OWNER (B) controls → true.
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(owner_pw),
                PlayerId(1),
            )],
            ..Default::default()
        });
        assert!(evaluate_condition_with_recipient(
            &state,
            &condition,
            PlayerId(0),
            attacker,
            attacker,
        ));

        // Planeswalker the CONTROLLER (A, not owner) controls → false (CR 108.3).
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(controller_pw),
                PlayerId(0),
            )],
            ..Default::default()
        });
        assert!(!evaluate_condition_with_recipient(
            &state,
            &condition,
            PlayerId(0),
            attacker,
            attacker,
        ));
    }

    #[test]
    fn recipient_attacking_owner_target_false_when_not_attacking() {
        use crate::game::combat::CombatState;
        use crate::types::triggers::AttackTargetFilter;
        let mut state = setup();
        let attacker = make_creature(&mut state, "Idle", 4, 4, PlayerId(1));
        state.objects.get_mut(&attacker).unwrap().controller = PlayerId(0);
        let condition = StaticCondition::RecipientAttackingOwnerTarget {
            target: AttackTargetFilter::OwnerOrPlaneswalker,
        };

        // Combat exists but the creature is not an attacker → false.
        state.combat = Some(CombatState::default());
        assert!(!evaluate_condition_with_recipient(
            &state,
            &condition,
            PlayerId(0),
            attacker,
            attacker,
        ));
    }

    #[test]
    fn recipient_attacking_owner_target_false_when_no_combat() {
        use crate::types::triggers::AttackTargetFilter;
        let mut state = setup();
        let attacker = make_creature(&mut state, "Idle", 4, 4, PlayerId(1));
        state.objects.get_mut(&attacker).unwrap().controller = PlayerId(0);
        let condition = StaticCondition::RecipientAttackingOwnerTarget {
            target: AttackTargetFilter::Owner,
        };
        assert!(state.combat.is_none());
        assert!(!evaluate_condition_with_recipient(
            &state,
            &condition,
            PlayerId(0),
            attacker,
            attacker,
        ));
    }

    #[test]
    fn recipient_attacking_owner_target_false_when_no_recipient() {
        // CR 509.1b: routing through the source-binding path (recipient_id == None)
        // must yield false — guards the `condition_uses_recipient_context` arm.
        use crate::game::combat::{AttackerInfo, CombatState};
        use crate::types::triggers::AttackTargetFilter;
        let mut state = setup();
        let attacker = make_creature(&mut state, "Donated", 4, 4, PlayerId(1));
        state.objects.get_mut(&attacker).unwrap().controller = PlayerId(0);
        let condition = StaticCondition::RecipientAttackingOwnerTarget {
            target: AttackTargetFilter::Owner,
        };
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..Default::default()
        });
        // No recipient supplied → defensive false, even though attacking owner.
        assert!(!evaluate_condition_for_test(
            &state,
            &condition,
            PlayerId(0),
            attacker,
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
                duration_subject: None,
                source_name: String::new(),
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
                duration_subject: None,
                source_name: String::new(),
            });
        let mut effects = vec![];
        gather_transient_continuous_effects(&state, &mut effects);
        assert!(
            !effects.is_empty(),
            "effect should be gathered when source is tapped"
        );
    }

    // CR 110.5d: a tapped source that has left the battlefield is neither tapped
    // nor untapped — `SourceIsTapped` must evaluate false once it is off-battlefield.
    #[test]
    fn source_is_tapped_false_when_source_off_battlefield() {
        let mut state = setup();
        let id = make_creature(&mut state, "Tapper", 1, 1, PlayerId(0));
        state.objects.get_mut(&id).unwrap().tapped = true;

        // On the battlefield + tapped → predicate is true, inversion is false.
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsTapped,
            PlayerId(0),
            id,
        ));
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::Not {
                condition: Box::new(StaticCondition::SourceIsTapped),
            },
            PlayerId(0),
            id,
        ));

        // Move it off the battlefield, leaving `tapped == true` (status is stale
        // but harmless — CR 110.5d means it is no longer tapped by rule).
        state.objects.get_mut(&id).unwrap().zone = Zone::Graveyard;
        assert!(state.objects.get(&id).unwrap().tapped);

        // Off-battlefield → predicate false, inversion true.
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsTapped,
            PlayerId(0),
            id,
        ));
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::Not {
                condition: Box::new(StaticCondition::SourceIsTapped),
            },
            PlayerId(0),
            id,
        ));
    }

    // CR 702.171b: `SourceIsSaddled` gates a continuous modification on the
    // saddled designation. Not saddled → no gather; saddled → gathered;
    // off-battlefield → false (CR 110.5d, no designation off the battlefield).
    #[test]
    fn source_is_saddled_gates_continuous_effect() {
        let mut state = setup();
        let id = make_creature(&mut state, "Mount", 2, 2, PlayerId(0));

        // Not saddled → condition false (no bonus).
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsSaddled,
            PlayerId(0),
            id,
        ));

        // Saddled → condition true (regression for "mounts always behave as if saddled").
        state.objects.get_mut(&id).unwrap().is_saddled = true;
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsSaddled,
            PlayerId(0),
            id,
        ));

        // CR 110.5d: off the battlefield there is no saddled designation.
        state.objects.get_mut(&id).unwrap().zone = Zone::Graveyard;
        assert!(state.objects.get(&id).unwrap().is_saddled);
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsSaddled,
            PlayerId(0),
            id,
        ));
    }

    // CR 303.4: `SourceIsEnchanted` gates a continuous modification on at least
    // one Aura being attached to the source. No attachment → false; an Aura
    // (subtype "Aura") attached → true; an Equipment (subtype "Equipment")
    // attached → FALSE (negative twin — the matcher keys solely on the "Aura"
    // subtype, mirroring `SourceIsEquipped`'s "Equipment" check). Discriminating:
    // if the matcher arm or the new variant is reverted this test fails to
    // compile / evaluates wrong, re-exposing the always-true `Unrecognized`
    // fallback (Pillar of War / Thran Golem / Gate Hound / Freewind Equenaut).
    #[test]
    fn source_is_enchanted_gates_on_attached_aura() {
        use crate::game::game_object::AttachTarget;
        let mut state = setup();
        let creature = make_creature(&mut state, "Enchanted Bear", 2, 2, PlayerId(0));

        // No Aura attached → condition false.
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsEnchanted,
            PlayerId(0),
            creature,
        ));

        // Attach an Equipment (NOT an Aura) → still false (negative twin: the
        // matcher must not treat Equipment as enchanting).
        let equipment = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Bonesplitter".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&equipment).unwrap();
            obj.card_types.subtypes.push("Equipment".to_string());
            obj.attached_to = Some(AttachTarget::Object(creature));
        }
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsEnchanted,
            PlayerId(0),
            creature,
        ));

        // Attach an Aura to the source → condition true.
        let aura = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Holy Strength".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.subtypes.push("Aura".to_string());
            obj.attached_to = Some(AttachTarget::Object(creature));
        }
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceIsEnchanted,
            PlayerId(0),
            creature,
        ));
    }

    // CR 301.5 + CR 303.4: `SourceAttachedToCreature` gates a continuous
    // modification on the source Aura/Equipment being attached to a CREATURE
    // host. No attachment → false; attached to a non-creature (e.g. a land) →
    // false; attached to a creature → true. Discriminating: revert the eval arm
    // (or the `as_object` / creature-core-type filter) and the negative cases
    // start reporting true, re-exposing the always-true `Unrecognized` fallback.
    #[test]
    fn source_attached_to_creature_gates() {
        use crate::game::game_object::AttachTarget;
        let mut state = setup();
        let aura = make_creature(&mut state, "Roving Aura", 0, 0, PlayerId(0));

        // No attachment → false.
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceAttachedToCreature,
            PlayerId(0),
            aura,
        ));

        // Attached to a non-creature host (a land) → false.
        let land = make_land(&mut state, "Forest", PlayerId(0));
        state.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Object(land));
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceAttachedToCreature,
            PlayerId(0),
            aura,
        ));

        // Attached to a creature host → true.
        let bear = make_creature(&mut state, "Host Bear", 2, 2, PlayerId(0));
        state.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Object(bear));
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceAttachedToCreature,
            PlayerId(0),
            aura,
        ));
    }

    // CR 608.2c: `SourceMatchesFilter` gates a continuous modification on the
    // source matching a `TargetFilter` (leveler-style cards). A creature filter
    // matches a creature source → true; once the source loses its Creature core
    // type → false. Discriminating: revert the eval arm and the mismatched case
    // reports true, re-exposing the always-true `Unrecognized` fallback.
    #[test]
    fn source_matches_filter_gates() {
        let mut state = setup();
        let creature = make_creature(&mut state, "Figure", 2, 2, PlayerId(0));
        let filter = TargetFilter::Typed(TypedFilter::creature());

        // Source is a creature → matches.
        assert!(evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceMatchesFilter {
                filter: filter.clone(),
            },
            PlayerId(0),
            creature,
        ));

        // Strip the Creature core type → no longer matches a creature filter.
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types
                .core_types
                .retain(|t| *t != CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);
        assert!(!evaluate_condition_for_test(
            &state,
            &StaticCondition::SourceMatchesFilter { filter },
            PlayerId(0),
            creature,
        ));
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
    fn set_basic_land_type_replaces_rules_text_with_intrinsic_mana_ability() {
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
                    player: TargetFilter::Controller,
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
            !land
                .abilities
                .iter()
                .any(|ability| matches!(&*ability.effect, Effect::GainLife { .. })),
            "CR 305.7: Rules-text abilities should be removed"
        );
        assert_eq!(
            count_mana_abilities(land, ManaColor::Red),
            1,
            "CR 305.7: SetBasicLandType Mountain should grant the intrinsic red mana ability"
        );
        assert!(land.card_types.subtypes.contains(&"Mountain".to_string()));
        assert!(
            !land.card_types.subtypes.contains(&"Desert".to_string()),
            "CR 305.7: Old land subtypes should be removed"
        );
    }

    #[test]
    fn song_style_set_card_types_and_basic_land_type_makes_nonland_a_forest() {
        // CR 205.1a + CR 305.7: Song of the Dryads both makes the enchanted
        // permanent a land and sets its basic land subtype, which removes its
        // rules-text abilities and grants the Forest intrinsic mana ability.
        let mut state = setup();
        let p0 = PlayerId(0);

        let creature_id = create_object(
            &mut state,
            CardId(0),
            p0,
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
            Arc::make_mut(&mut obj.base_abilities).push(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            ));
            obj.abilities = Arc::new((*obj.base_abilities).clone());
        }

        let aura_id = create_object(
            &mut state,
            CardId(1),
            p0,
            "Song of the Dryads".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Card)
                            .properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .modifications(vec![
                        ContinuousModification::SetCardTypes {
                            core_types: vec![CoreType::Land],
                        },
                        ContinuousModification::SetBasicLandType {
                            land_type: BasicLandType::Forest,
                        },
                    ]),
            );
        }

        state.objects.get_mut(&aura_id).unwrap().attached_to = Some(creature_id.into());

        evaluate_layers(&mut state);

        let creature = state.objects.get(&creature_id).unwrap();
        assert_eq!(creature.card_types.core_types, vec![CoreType::Land]);
        assert!(creature.card_types.subtypes.contains(&"Forest".to_string()));
        assert!(
            !creature
                .abilities
                .iter()
                .any(|ability| matches!(&*ability.effect, Effect::GainLife { .. })),
            "CR 305.7: rules-text abilities should be removed"
        );
        assert_eq!(
            count_mana_abilities(creature, ManaColor::Green),
            1,
            "CR 305.7: Forest subtype should grant the intrinsic green mana ability"
        );
    }

    #[test]
    fn song_style_aura_applies_to_host_when_attached_via_attach_to() {
        use crate::game::effects::attach::attach_to;

        let mut state = setup();
        let p0 = PlayerId(0);

        let creature_id = create_object(
            &mut state,
            CardId(0),
            p0,
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
            Arc::make_mut(&mut obj.base_abilities).push(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            ));
            obj.abilities = Arc::new((*obj.base_abilities).clone());
        }

        let aura_id = create_object(
            &mut state,
            CardId(1),
            p0,
            "Song of the Dryads".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Card)
                            .properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .modifications(vec![
                        ContinuousModification::SetCardTypes {
                            core_types: vec![CoreType::Land],
                        },
                        ContinuousModification::SetBasicLandType {
                            land_type: BasicLandType::Forest,
                        },
                    ]),
            );
        }

        attach_to(&mut state, aura_id, creature_id);

        let creature = state.objects.get(&creature_id).unwrap();
        assert_eq!(creature.card_types.core_types, vec![CoreType::Land]);
        assert!(creature.card_types.subtypes.contains(&"Forest".to_string()));
        assert!(
            !creature
                .abilities
                .iter()
                .any(|ability| matches!(&*ability.effect, Effect::GainLife { .. })),
            "CR 305.7: attach_to must flush layers so rules-text abilities are removed"
        );
    }

    #[test]
    fn song_of_dryads_from_oracle_strips_host_triggers_and_statics() {
        use crate::game::effects::attach::attach_to;

        let mut scenario = GameScenario::new();
        let host = {
            let mut card = scenario.add_creature(PlayerId(0), "Obuun, Mul Daya Ancestor", 3, 3);
            card.from_oracle_text(
                "At the beginning of combat on your turn, up to one target land you control becomes an X/X Elemental creature with trample and haste until end of turn, where X is Obuun's power. It's still a land.\nLandfall — Whenever a land you control enters, put a +1/+1 counter on target creature.",
            );
            card.with_trigger(TriggerMode::Attacks);
            card.id()
        };
        let song = scenario
            .add_creature(PlayerId(0), "Song of the Dryads", 0, 0)
            .as_enchantment()
            .from_oracle_text("Enchant permanent\nEnchanted permanent is a colorless Forest land.")
            .id();

        let mut state = scenario.build().state().clone();
        state
            .objects
            .get_mut(&song)
            .unwrap()
            .card_types
            .subtypes
            .push("Aura".to_string());

        attach_to(&mut state, song, host);

        let host_obj = state.objects.get(&host).unwrap();
        assert_eq!(host_obj.card_types.core_types, vec![CoreType::Land]);
        assert!(host_obj.card_types.subtypes.contains(&"Forest".to_string()));
        assert!(
            host_obj.trigger_definitions.is_empty(),
            "CR 305.7: printed triggers must be removed"
        );
        assert!(
            host_obj.static_definitions.is_empty(),
            "CR 305.7: printed statics must be removed"
        );
    }

    #[test]
    fn set_chosen_basic_land_type_reads_source_choice() {
        // CR 305.7 + CR 305.6: Phantasmal Terrain / Convincing Mirage. The Aura
        // (source) recorded a chosen basic land type as it entered; its
        // SetChosenBasicLandType static must set the ENCHANTED land's subtype to
        // that chosen type with full replacement semantics — old land subtype and
        // rules-text abilities cleared, intrinsic mana ability for the new type
        // derived (CR 305.6).
        use crate::types::ability::{BasicLandType, ChosenAttribute};

        let mut state = setup();
        let p0 = PlayerId(0);

        // Enchanted land: starts as a Swamp with a rules-text ability.
        let land_id = make_land(&mut state, "Test Swamp", p0);
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.subtypes.push("Swamp".to_string());
            obj.base_card_types = obj.card_types.clone();
            Arc::make_mut(&mut obj.base_abilities).push(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            ));
            obj.abilities = Arc::new((*obj.base_abilities).clone());
        }

        // Source Aura: chose Island as it entered, carries the chosen-type static
        // anchored to its enchanted land.
        let aura_id = create_object(
            &mut state,
            CardId(1),
            p0,
            "Convincing Mirage".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
            obj.attached_to = Some(land_id.into());
            obj.chosen_attributes
                .push(ChosenAttribute::BasicLandType(BasicLandType::Island));
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .modifications(vec![ContinuousModification::SetChosenBasicLandType]),
            );
        }
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .attachments
            .push(aura_id);

        evaluate_layers(&mut state);

        let land = state.objects.get(&land_id).unwrap();
        assert!(
            land.card_types.subtypes.contains(&"Island".to_string()),
            "CR 305.7: enchanted land should gain the source's chosen Island subtype"
        );
        assert!(
            !land.card_types.subtypes.contains(&"Swamp".to_string()),
            "CR 305.7: old land subtype should be removed"
        );
        assert!(
            !land
                .abilities
                .iter()
                .any(|ability| matches!(&*ability.effect, Effect::GainLife { .. })),
            "CR 305.7: rules-text abilities should be removed"
        );
        assert_eq!(
            count_mana_abilities(land, ManaColor::Blue),
            1,
            "CR 305.6: chosen Island should grant the intrinsic blue mana ability"
        );
        assert_eq!(
            count_mana_abilities(land, ManaColor::Black),
            0,
            "CR 305.7: old Swamp mana ability should be gone"
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
    fn omo_everything_counter_grants_all_land_and_creature_types() {
        // CR 205.3i + CR 305.7 + CR 122.1: Omo, Queen of Vesuva.
        // "Each land with an everything counter on it is every land type in
        //  addition to its other types."
        // "Each nonland creature with an everything counter on it is every
        //  creature type."
        // A land and a nonland creature each carrying an `everything` counter
        // gain all land types / all creature types respectively; the basic land
        // types also grant their intrinsic mana ability (CR 305.7). A land and
        // creature WITHOUT the counter gain nothing (FilterProp::Counters
        // affected-set gating drives the layer).
        let mut state = setup();
        state.all_creature_types = vec!["Bear".to_string(), "Goblin".to_string()];
        let p0 = PlayerId(0);

        // Counter filter shared by both statics.
        let counter_prop = FilterProp::Counters {
            counters: CounterMatch::OfType(CounterType::Generic("everything".to_string())),
            comparator: crate::types::ability::Comparator::GE,
            count: QuantityExpr::Fixed { value: 1 },
        };

        // Land WITH an everything counter (gains all land types).
        let land_with = make_land(&mut state, "Plain Land", p0);
        state
            .objects
            .get_mut(&land_with)
            .unwrap()
            .counters
            .insert(CounterType::Generic("everything".to_string()), 1);
        // Land WITHOUT the counter (mutation control — gains nothing).
        let land_without = make_land(&mut state, "Bare Land", p0);

        // Nonland creature WITH an everything counter (gains all creature types).
        let creature_with = make_creature(&mut state, "Test Beast", 2, 2, p0);
        state
            .objects
            .get_mut(&creature_with)
            .unwrap()
            .counters
            .insert(CounterType::Generic("everything".to_string()), 1);
        // Nonland creature WITHOUT the counter (mutation control).
        let creature_without = make_creature(&mut state, "Bare Beast", 2, 2, p0);

        // Omo's two statics as a single source.
        let source_id = create_object(
            &mut state,
            CardId(1),
            p0,
            "Omo, Queen of Vesuva".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::land().properties(vec![counter_prop.clone()]),
                    ))
                    .modifications(vec![ContinuousModification::AddAllLandTypes]),
            );
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature()
                            .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
                            .properties(vec![counter_prop.clone()]),
                    ))
                    .modifications(vec![ContinuousModification::AddAllCreatureTypes]),
            );
        }

        evaluate_layers(&mut state);

        // Land with the counter has all 17 land subtypes (spot-check a spread).
        let land = state.objects.get(&land_with).unwrap();
        for name in ["Forest", "Island", "Desert", "Gate", "Locus"] {
            assert!(
                land.card_types.subtypes.contains(&name.to_string()),
                "Counter land missing land type: {name}"
            );
        }
        // CR 305.7: a basic land type among the 17 grants its intrinsic mana ability.
        assert!(
            has_basic_land_mana_ability(land, ManaColor::Green),
            "Counter land should gain Forest's intrinsic {{T}}: Add {{G}} (CR 305.7)"
        );

        // Creature with the counter gains the global creature types.
        let creature = state.objects.get(&creature_with).unwrap();
        for name in &state.all_creature_types {
            assert!(
                creature.card_types.subtypes.contains(name),
                "Counter creature missing creature type: {name}"
            );
        }

        // Mutation check: objects WITHOUT the counter gain nothing.
        let bare_land = state.objects.get(&land_without).unwrap();
        assert!(
            !bare_land
                .card_types
                .subtypes
                .contains(&"Forest".to_string()),
            "Land without the counter must NOT gain land types"
        );
        assert!(
            !has_basic_land_mana_ability(bare_land, ManaColor::Green),
            "Land without the counter must NOT gain a mana ability"
        );
        let bare_creature = state.objects.get(&creature_without).unwrap();
        assert!(
            !bare_creature
                .card_types
                .subtypes
                .contains(&"Bear".to_string()),
            "Creature without the counter must NOT gain creature types"
        );
    }

    #[test]
    fn remove_all_abilities_removes_basic_land_intrinsic_mana_ability() {
        // CR 613.1d + CR 613.1f: basic land intrinsic abilities are derived
        // after type effects, then ordinary ability-removing effects in layer 6
        // can remove them.
        let mut state = setup();
        let p0 = PlayerId(0);

        let land_id = make_land(&mut state, "Forest", p0);
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.subtypes.push("Forest".to_string());
            obj.base_card_types = obj.card_types.clone();
        }

        let source_id = create_object(
            &mut state,
            CardId(1),
            p0,
            "Blanker".to_string(),
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
                    .modifications(vec![ContinuousModification::RemoveAllAbilities]),
            );
        }

        evaluate_layers(&mut state);

        let land = state.objects.get(&land_id).unwrap();
        assert_eq!(
            count_mana_abilities(land, ManaColor::Green),
            0,
            "Layer 6 RemoveAllAbilities must remove the derived Forest mana ability"
        );
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
                frequency: crate::types::statics::CastFrequency::Unlimited,
                source_id: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
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
            duration: Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            },
            granted_to: PlayerId(0),
            frequency: crate::types::statics::CastFrequency::Unlimited,
            source_id: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_raise: None,
            land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        });
        perms.push(CastingPermission::PlayFromExile {
            duration: Duration::Permanent,
            granted_to: PlayerId(0),
            frequency: crate::types::statics::CastFrequency::Unlimited,
            source_id: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_raise: None,
            land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
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
    fn until_end_of_next_turn_permission_armed_at_untap_expires_at_cleanup() {
        // CR 514.2: a "until the end of your next turn" play-permission (Light Up
        // the Stage class) must SURVIVE the grantee's untap step — armed to
        // UntilEndOfTurn — and expire only at that turn's cleanup, so the exiled
        // cards are playable throughout the next turn. Contrast
        // `until_your_next_turn_prune_expires_for_grantee_only`, where
        // UntilNextTurnOf is removed outright at untap.
        let mut state = setup();
        let exiled = make_exiled_card(&mut state, PlayerId(0));
        state
            .objects
            .get_mut(&exiled)
            .unwrap()
            .casting_permissions
            .push(CastingPermission::PlayFromExile {
                duration: Duration::UntilEndOfNextTurnOf {
                    player: PlayerScope::Controller,
                },
                granted_to: PlayerId(0),
                frequency: crate::types::statics::CastFrequency::Unlimited,
                source_id: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            });

        // Untap step of the grantee's next turn: armed to UntilEndOfTurn, kept.
        prune_until_next_turn_casting_permissions(&mut state, PlayerId(0));
        let perms = &state.objects[&exiled].casting_permissions;
        assert_eq!(
            perms.len(),
            1,
            "permission must survive the untap step (CR 514.2), not be pruned"
        );
        assert!(
            matches!(
                perms[0],
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn,
                    ..
                }
            ),
            "permission must be armed to UntilEndOfTurn at untap, got {:?}",
            perms[0]
        );

        // Cleanup of that turn: the armed permission now expires.
        prune_end_of_turn_casting_permissions(&mut state);
        assert!(
            state.objects[&exiled].casting_permissions.is_empty(),
            "armed permission expires at the cleanup of the grantee's next turn"
        );
    }

    #[test]
    fn until_end_of_next_turn_effect_armed_at_untap_expires_at_cleanup() {
        // CR 514.2: a continuous effect granted "until the end of your next turn"
        // (Slip Out the Back class) must persist through the controller's next
        // turn — armed at untap, pruned at that turn's cleanup.
        let mut state = setup();
        state.add_transient_continuous_effect(
            ObjectId(0),
            PlayerId(0),
            Duration::UntilEndOfNextTurnOf {
                player: PlayerScope::Controller,
            },
            TargetFilter::SelfRef,
            vec![],
            None,
        );

        prune_until_next_turn_effects(&mut state, PlayerId(0));
        assert_eq!(
            state.transient_continuous_effects.len(),
            1,
            "effect must survive the untap step (armed), not be pruned"
        );
        assert_eq!(
            state.transient_continuous_effects[0].duration,
            Duration::UntilEndOfTurn,
            "effect must be armed to UntilEndOfTurn at untap"
        );

        prune_end_of_turn_effects(&mut state);
        assert!(
            state.transient_continuous_effects.is_empty(),
            "armed effect expires at the cleanup of the controller's next turn"
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
                duration: Duration::UntilNextTurnOf {
                    player: PlayerScope::Controller,
                },
                granted_to: PlayerId(0),
                frequency: crate::types::statics::CastFrequency::Unlimited,
                source_id: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            });
        state
            .objects
            .get_mut(&card_b)
            .unwrap()
            .casting_permissions
            .push(CastingPermission::PlayFromExile {
                duration: Duration::UntilNextTurnOf {
                    player: PlayerScope::Controller,
                },
                granted_to: PlayerId(1),
                frequency: crate::types::statics::CastFrequency::Unlimited,
                source_id: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
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
                frequency: crate::types::statics::CastFrequency::Unlimited,
                source_id: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
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

    /// Builds a Doctor Doom permanent on player 0's battlefield whose static
    /// grants itself Indestructible while its controller controls an artifact
    /// creature or a Plan — the exact typed condition the parser now lowers
    /// ("you control an artifact creature or a Plan").
    ///
    /// CR 604.1 + CR 611.3a + CR 702.12b. The condition is the typed
    /// `IsPresent { Or[ Typed{[Artifact,Creature],You,Battlefield},
    /// Typed{[Plan],You,Battlefield} ] }` (see
    /// `parser::oracle_nom::condition::tests::doctor_doom_disjunctive_control_condition_is_typed_not_unrecognized`).
    fn doctor_doom_static() -> StaticDefinition {
        use crate::types::keywords::Keyword;
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Indestructible,
            }])
            .condition(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Artifact, TypeFilter::Creature],
                            controller: Some(ControllerRef::You),
                            properties: vec![FilterProp::InZone {
                                zone: Zone::Battlefield,
                            }],
                        }),
                        TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Subtype("Plan".to_string())],
                            controller: Some(ControllerRef::You),
                            properties: vec![FilterProp::InZone {
                                zone: Zone::Battlefield,
                            }],
                        }),
                    ],
                }),
            })
    }

    fn make_doctor_doom(state: &mut GameState, player: PlayerId) -> ObjectId {
        let doom = make_creature(state, "Doctor Doom", 4, 4, player);
        let s = doctor_doom_static();
        let obj = state.objects.get_mut(&doom).unwrap();
        Arc::make_mut(&mut obj.base_static_definitions).push(s.clone());
        obj.static_definitions.push(s);
        doom
    }

    fn make_artifact_creature(state: &mut GameState, name: &str, player: PlayerId) -> ObjectId {
        let id = make_creature(state, name, 1, 1, player);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
        id
    }

    /// CR 205.3h: a permanent that is `Enchantment — Plan` (e.g. Doom Reigns
    /// Supreme). "Plan" is an enchantment subtype, so the second disjunct
    /// `Typed{[Subtype("Plan")], You, Battlefield}` matches it.
    fn make_plan_enchantment(state: &mut GameState, name: &str, player: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(0),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Plan".to_string());
        obj.base_card_types = obj.card_types.clone();
        id
    }

    /// Arm A (positive): player 0 controls an artifact creature, so the typed
    /// `IsPresent { Or[..] }` condition is true and Doctor Doom has
    /// Indestructible. CR 611.3a (continuous re-evaluation) + CR 702.12b.
    #[test]
    fn doctor_doom_has_indestructible_with_artifact_creature() {
        use crate::types::keywords::Keyword;
        let mut state = setup();
        let doom = make_doctor_doom(&mut state, PlayerId(0));
        make_artifact_creature(&mut state, "Doombot", PlayerId(0));

        evaluate_layers(&mut state);

        assert!(
            state
                .objects
                .get(&doom)
                .unwrap()
                .has_keyword(&Keyword::Indestructible),
            "Doctor Doom must have Indestructible while you control an artifact creature"
        );
    }

    /// Arm B (revert-probe / discriminating): player 0 controls NO artifact
    /// creature and NO Plan, so the condition is false and Doctor Doom does NOT
    /// have Indestructible. PRE-FIX the parser lowered this condition to
    /// `Unrecognized`, which `evaluate_condition` treats as `true` — making the
    /// grant always-on so this assertion would FAIL on revert.
    #[test]
    fn doctor_doom_no_indestructible_without_artifact_creature_or_plan() {
        use crate::types::keywords::Keyword;
        let mut state = setup();
        let doom = make_doctor_doom(&mut state, PlayerId(0));
        // A plain (non-artifact) creature must not satisfy the AND-of-types leg.
        make_creature(&mut state, "Vanilla Bear", 2, 2, PlayerId(0));

        evaluate_layers(&mut state);

        assert!(
            !state
                .objects
                .get(&doom)
                .unwrap()
                .has_keyword(&Keyword::Indestructible),
            "Doctor Doom must NOT have Indestructible with no artifact creature/Plan you control"
        );
    }

    /// Arm C (multi-authority / proves `ControllerRef::You`): the only artifact
    /// creature is controlled by the OPPONENT (player 1), so the `You`-scoped
    /// condition is false and Doctor Doom does NOT have Indestructible. A naive
    /// global "is any artifact creature present" rescan would wrongly grant it.
    /// CR 109.5 ("you" = the static's controller).
    #[test]
    fn doctor_doom_no_indestructible_when_opponent_controls_artifact_creature() {
        use crate::types::keywords::Keyword;
        let mut state = setup();
        let doom = make_doctor_doom(&mut state, PlayerId(0));
        make_artifact_creature(&mut state, "Enemy Doombot", PlayerId(1));

        evaluate_layers(&mut state);

        assert!(
            !state
                .objects
                .get(&doom)
                .unwrap()
                .has_keyword(&Keyword::Indestructible),
            "an opponent-controlled artifact creature must NOT satisfy the You-scoped condition"
        );
    }

    /// Arm D (positive, Subtype("Plan") leg): player 0 controls an
    /// `Enchantment — Plan` permanent and NO artifact creature, so the second
    /// disjunct `Typed{[Subtype("Plan")], You, Battlefield}` is satisfied and
    /// Doctor Doom has Indestructible. This exercises the RUNTIME half — the
    /// layer-eval + filter-match against a hand-built `Subtype("Plan")`
    /// condition and a hand-set `Enchantment — Plan` permanent. It does NOT
    /// exercise the parser or `ENCHANTMENT_SUBTYPES`; the parser-side
    /// discrimination ("a Plan" → `Subtype("Plan")`, not a CoreType axis) lives
    /// in the parser tests (`parse_type_filter_word`,
    /// `doctor_doom_disjunctive_control_condition_is_typed_not_unrecognized`)
    /// and in the end-to-end test below. CR 205.3h.
    #[test]
    fn doctor_doom_has_indestructible_with_plan_enchantment() {
        use crate::types::keywords::Keyword;
        let mut state = setup();
        let doom = make_doctor_doom(&mut state, PlayerId(0));
        make_plan_enchantment(&mut state, "Doom Reigns Supreme", PlayerId(0));

        evaluate_layers(&mut state);

        assert!(
            state
                .objects
                .get(&doom)
                .unwrap()
                .has_keyword(&Keyword::Indestructible),
            "Doctor Doom must have Indestructible while you control a Plan (enchantment subtype)"
        );
    }

    /// End-to-end (parser → layers), the true Plan-as-subtype revert probe:
    /// Doctor Doom's printed indestructible line is PARSED (not hand-built), so
    /// the second disjunct is produced by `parse_type_filter_word` consulting
    /// `ENCHANTMENT_SUBTYPES`. With a real `Enchantment — Plan` permanent and no
    /// artifact creature in play, `evaluate_layers` grants Indestructible only
    /// if the parsed condition's "a Plan" disjunct lowered to
    /// `Subtype("Plan")`. CR 205.3h + CR 604.1 + CR 702.12b.
    ///
    /// DISCRIMINATION: reverting the Plan-as-subtype rework makes the parser
    /// produce the old `CoreType::Plan` axis (or drop "Plan" from
    /// `ENCHANTMENT_SUBTYPES`), so the parsed disjunct no longer matches a
    /// subtype-Plan enchantment, the condition is unsatisfied, and Doctor Doom
    /// does NOT gain Indestructible — flipping this assertion.
    #[test]
    fn doctor_doom_indestructible_parsed_end_to_end_with_plan_enchantment() {
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::keywords::Keyword;
        let mut state = setup();

        // Doctor Doom carries the PARSER-PRODUCED indestructible static.
        let doom = make_creature(&mut state, "Doctor Doom", 4, 4, PlayerId(0));
        let parsed = parse_oracle_text(
            "As long as you control an artifact creature or a Plan, \
             Doctor Doom has indestructible.",
            "Doctor Doom",
            &[],
            &["Creature".into()],
            &[],
        );
        assert_eq!(
            parsed.statics.len(),
            1,
            "the indestructible line parses to exactly one static; got {:?}",
            parsed.statics
        );
        {
            let obj = state.objects.get_mut(&doom).unwrap();
            for s in parsed.statics.iter() {
                Arc::make_mut(&mut obj.base_static_definitions).push(s.clone());
                obj.static_definitions.push(s.clone());
            }
        }

        // A real `Enchantment — Plan` permanent (no artifact creature), so only
        // the Subtype("Plan") disjunct can satisfy the condition.
        make_plan_enchantment(&mut state, "Doom Reigns Supreme", PlayerId(0));

        evaluate_layers(&mut state);

        assert!(
            state
                .objects
                .get(&doom)
                .unwrap()
                .has_keyword(&Keyword::Indestructible),
            "parsed condition's Subtype(\"Plan\") disjunct must match the Plan enchantment"
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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
                player: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
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

        state.layers_dirty.mark_full();
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
        state.layers_dirty.mark_full();
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

        state.layers_dirty.mark_full();
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
        state.layers_dirty.mark_full();
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
        state.layers_dirty.mark_full();
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

    #[test]
    fn add_keyword_undying_installs_and_removes_synthesized_trigger() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let source = make_creature(&mut state, "Mikaeus Stand-In", 1, 1, PlayerId(0));
        let def = StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: bear })
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Undying,
            }]);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&bear).unwrap();
        assert!(obj.keywords.contains(&Keyword::Undying));
        assert_eq!(obj.trigger_definitions.len(), 1);
        let trigger = obj.trigger_definitions.first().unwrap();
        assert!(matches!(trigger.mode, TriggerMode::ChangesZone));
        assert_eq!(trigger.origin, Some(Zone::Battlefield));
        assert_eq!(trigger.destination, Some(Zone::Graveyard));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
        assert!(matches!(
            trigger.condition,
            Some(TriggerCondition::Not { .. })
        ));

        state.battlefield.retain(|&id| id != source);
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&bear).unwrap();
        assert!(!obj.keywords.contains(&Keyword::Undying));
        assert!(obj.trigger_definitions.is_empty());
    }

    #[test]
    fn granted_undying_with_complex_filter_installs_trigger() {
        let mut state = setup();
        state.all_creature_types = vec!["Human".to_string(), "Bear".to_string()];

        // Create a non-Human creature (Bear - not Human)
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let bear_obj = state.objects.get_mut(&bear).unwrap();
        bear_obj.card_types.subtypes.push("Bear".to_string());
        bear_obj.base_card_types.subtypes = bear_obj.card_types.subtypes.clone();

        // Create Mikaeus-like source with complex filter: "Other non-Human creatures you control"
        let mikaeus = make_creature(&mut state, "Mikaeus", 5, 5, PlayerId(0));
        let mikaeus_obj = state.objects.get_mut(&mikaeus).unwrap();
        mikaeus_obj.card_types.subtypes.push("Human".to_string());
        mikaeus_obj.base_card_types.subtypes = mikaeus_obj.card_types.subtypes.clone();

        let def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                        "Human".to_string(),
                    ))))
                    .properties(vec![FilterProp::Another]),
            ))
            .modifications(vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Undying,
                },
            ]);
        mikaeus_obj.static_definitions.push(def);

        evaluate_layers(&mut state);

        // Verify Undying is granted to the non-Human creature
        let bear = state.objects.get(&bear).unwrap();
        assert!(
            bear.keywords.contains(&Keyword::Undying),
            "Bear should have Undying"
        );
        assert_eq!(bear.power, Some(3), "Bear should have +1/+1 from Mikaeus");
        assert_eq!(
            bear.toughness,
            Some(3),
            "Bear should have +1/+1 from Mikaeus"
        );

        // Verify the Undying trigger is installed
        assert_eq!(
            bear.trigger_definitions.len(),
            1,
            "Bear should have Undying trigger"
        );
        let trigger = bear.trigger_definitions.first().unwrap();
        assert!(matches!(trigger.mode, TriggerMode::ChangesZone));
        assert_eq!(trigger.origin, Some(Zone::Battlefield));
        assert_eq!(trigger.destination, Some(Zone::Graveyard));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
    }

    #[test]
    fn granted_undying_with_complex_filter_fires_on_death() {
        let mut state = setup();
        state.all_creature_types = vec!["Human".to_string(), "Bear".to_string()];

        // Create a non-Human creature (Bear - not Human)
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        {
            let bear_obj = state.objects.get_mut(&bear).unwrap();
            bear_obj.card_types.subtypes.push("Bear".to_string());
            bear_obj.base_card_types.subtypes = bear_obj.card_types.subtypes.clone();
        }

        // Create Mikaeus-like source with complex filter: "Other non-Human creatures you control"
        let mikaeus = make_creature(&mut state, "Mikaeus", 5, 5, PlayerId(0));
        let def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                        "Human".to_string(),
                    ))))
                    .properties(vec![FilterProp::Another]),
            ))
            .modifications(vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Undying,
                },
            ]);
        {
            let mikaeus_obj = state.objects.get_mut(&mikaeus).unwrap();
            mikaeus_obj.card_types.subtypes.push("Human".to_string());
            mikaeus_obj.base_card_types.subtypes = mikaeus_obj.card_types.subtypes.clone();
            mikaeus_obj.static_definitions.push(def);
        }

        evaluate_layers(&mut state);

        // Verify Undying is granted and trigger is installed
        assert!(
            state
                .objects
                .get(&bear)
                .unwrap()
                .keywords
                .contains(&Keyword::Undying),
            "Bear should have Undying"
        );
        assert_eq!(
            state.objects.get(&bear).unwrap().trigger_definitions.len(),
            1,
            "Bear should have Undying trigger"
        );

        // Kill the bear by dealing lethal damage
        let mut events = Vec::new();
        state.objects.get_mut(&bear).unwrap().damage_marked = 3;
        crate::game::sba::check_state_based_actions(&mut state, &mut events);

        // Process triggers from the death event
        crate::game::triggers::process_triggers(&mut state, &events);

        // Check if Undying trigger fired - it should be on the stack
        assert!(
            !state.stack.is_empty() || state.pending_trigger.is_some(),
            "Undying trigger should be on stack or pending"
        );

        let origin = state
            .stack
            .last()
            .and_then(|entry| entry.ability())
            .map(|ability| ability.may_trigger_origin)
            .or_else(|| {
                state
                    .pending_trigger
                    .as_ref()
                    .map(|trigger| trigger.may_trigger_origin)
            })
            .flatten();
        assert_eq!(
            origin,
            Some(MayTriggerOrigin::Keyword {
                keyword: KeywordKind::Undying,
            }),
            "LKI-synthesized Undying must keep keyword origin instead of a fake printed index"
        );
    }

    #[test]
    fn printed_and_runtime_granted_parameterized_lki_keywords_both_trigger() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Afterlife Bear", 2, 2, PlayerId(0));
        let printed = Keyword::Afterlife(1);
        let printed_trigger = KeywordTriggerInstaller::triggers_for(&printed)
            .pop()
            .expect("afterlife has a trigger template");
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            obj.keywords.push(printed.clone());
            obj.base_keywords.push(printed);
            obj.trigger_definitions.push(printed_trigger.clone());
            obj.base_trigger_definitions = Arc::new(vec![printed_trigger]);
        }

        let source = make_creature(&mut state, "Afterlife Granter", 1, 1, PlayerId(0));
        let def = StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: bear })
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Afterlife(2),
            }]);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert!(bear_obj.keywords.contains(&Keyword::Afterlife(1)));
        assert!(bear_obj.keywords.contains(&Keyword::Afterlife(2)));
        assert_eq!(
            bear_obj.trigger_definitions.len(),
            2,
            "printed and runtime-granted Afterlife triggers must coexist"
        );

        let mut events = Vec::new();
        state.objects.get_mut(&bear).unwrap().damage_marked = 2;
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        crate::game::triggers::process_triggers(&mut state, &events);

        let crate::types::game_state::WaitingFor::OrderTriggers { player, triggers } =
            state.waiting_for.clone()
        else {
            panic!(
                "printed Afterlife 1 plus runtime-granted Afterlife 2 must produce two orderable triggers, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(player, PlayerId(0));
        assert_eq!(
            triggers.len(),
            2,
            "both printed and runtime-granted LKI keyword triggers must fire"
        );
    }

    #[test]
    fn add_keyword_annihilator_installs_parameterized_attack_trigger() {
        let mut state = setup();
        let attacker = make_creature(&mut state, "Battle-Mace Bearer", 2, 2, PlayerId(0));
        let source = make_creature(&mut state, "Nazgul Battle-Mace Stand-In", 1, 1, PlayerId(0));
        let def = StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: attacker })
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Annihilator(1),
            }]);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&attacker).unwrap();
        assert!(obj.keywords.contains(&Keyword::Annihilator(1)));
        assert_eq!(obj.trigger_definitions.len(), 1);
        let trigger = obj.trigger_definitions.first().unwrap();
        assert!(matches!(trigger.mode, TriggerMode::Attacks));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));

        let execute = trigger.execute.as_deref().expect("execute body required");
        let Effect::Sacrifice {
            target,
            count,
            min_count,
        } = &*execute.effect
        else {
            panic!("annihilator execute must sacrifice permanents");
        };
        assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
        assert_eq!(*min_count, 0);
        let TargetFilter::Typed(filter) = target else {
            panic!("annihilator target must be a typed permanent filter");
        };
        assert_eq!(filter.controller, Some(ControllerRef::DefendingPlayer));
        assert!(filter
            .type_filters
            .iter()
            .any(|filter| matches!(filter, TypeFilter::Permanent)));
    }

    #[test]
    fn add_keyword_annihilator_preserves_printed_and_granted_instances() {
        let mut state = setup();
        let attacker = make_creature(&mut state, "Printed Annihilator", 2, 2, PlayerId(0));
        let source = make_creature(&mut state, "Battle-Mace", 1, 1, PlayerId(0));
        let printed = Keyword::Annihilator(1);
        let printed_trigger = KeywordTriggerInstaller::triggers_for(&printed)
            .pop()
            .expect("annihilator has a trigger template");
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.keywords.push(printed.clone());
            obj.base_keywords.push(printed.clone());
            obj.trigger_definitions.push(printed_trigger.clone());
            Arc::make_mut(&mut obj.base_trigger_definitions).push(printed_trigger);
        }
        let def = StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: attacker })
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: printed,
            }]);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&attacker).unwrap();
        let annihilator_triggers = obj
            .trigger_definitions
            .iter_all()
            .filter(|trigger| matches!(trigger.mode, TriggerMode::Attacks))
            .filter(|trigger| matches!(trigger.valid_card, Some(TargetFilter::SelfRef)))
            .filter(|trigger| {
                matches!(
                    trigger.execute.as_deref().map(|ability| &*ability.effect),
                    Some(Effect::Sacrifice {
                        target: TargetFilter::Typed(filter),
                        count: QuantityExpr::Fixed { value: 1 },
                        ..
                    }) if filter.controller == Some(ControllerRef::DefendingPlayer)
                )
            })
            .count();
        assert_eq!(
            annihilator_triggers, 2,
            "printed Annihilator 1 and granted Annihilator 1 must remain independent trigger instances"
        );
    }

    #[test]
    fn add_dynamic_keyword_annihilator_installs_resolved_attack_trigger() {
        let mut state = setup();
        let attacker = make_creature(&mut state, "Dynamic Annihilator", 2, 2, PlayerId(0));
        let source = make_creature(&mut state, "Variable Battle-Mace", 1, 1, PlayerId(0));
        let def = StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: attacker })
            .modifications(vec![ContinuousModification::AddDynamicKeyword {
                kind: crate::types::keywords::DynamicKeywordKind::Annihilator,
                value: QuantityExpr::Fixed { value: 3 },
            }]);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&attacker).unwrap();
        assert!(obj.keywords.contains(&Keyword::Annihilator(3)));
        let trigger = obj
            .trigger_definitions
            .iter_all()
            .find(|trigger| {
                matches!(
                    trigger.execute.as_deref().map(|ability| &*ability.effect),
                    Some(Effect::Sacrifice {
                        target: TargetFilter::Typed(filter),
                        count: QuantityExpr::Fixed { value: 3 },
                        ..
                    }) if filter.controller == Some(ControllerRef::DefendingPlayer)
                )
            })
            .expect("dynamic Annihilator 3 should install a sacrifice trigger");
        assert!(matches!(trigger.mode, TriggerMode::Attacks));
        assert!(matches!(trigger.valid_card, Some(TargetFilter::SelfRef)));
    }

    #[test]
    fn remove_keyword_undying_removes_synthesized_trigger() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let grant_source = make_creature(&mut state, "Undying Granter", 1, 1, PlayerId(0));
        let remove_source = make_creature(&mut state, "Undying Suppressor", 1, 1, PlayerId(0));
        let grant = StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: bear })
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Undying,
            }]);
        let remove = StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: bear })
            .modifications(vec![ContinuousModification::RemoveKeyword {
                keyword: Keyword::Undying,
            }]);
        state
            .objects
            .get_mut(&grant_source)
            .unwrap()
            .static_definitions
            .push(grant);
        state
            .objects
            .get_mut(&remove_source)
            .unwrap()
            .static_definitions
            .push(remove);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&bear).unwrap();
        assert!(!obj.keywords.contains(&Keyword::Undying));
        assert!(
            !obj.trigger_definitions.iter_all().any(|trigger| {
                KeywordTriggerInstaller::trigger_matches_keyword_kind(trigger, &Keyword::Undying)
            }),
            "RemoveKeyword(Undying) must remove the synthesized dies trigger"
        );
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

    // ---------- Source-attribution side-table tests ----------
    //
    // The attribution side-table is rebuilt each layers pass alongside the
    // derived state (keywords, abilities, P/T). These tests verify the
    // building-block contract: for every `ContinuousModification` that flows
    // through `apply_continuous_effect`, an `EffectRef` lands in the target
    // object's attribution under the right layer bucket. Tests cover (1) the
    // two `EffectRef` variants — Static and Transient — and (2) multi-source
    // accumulation, between-pass clearing, source-name snapshot, and self-
    // grants. Per-`ContinuousModification`-variant coverage would duplicate
    // the per-layer-bucket coverage here; one representative per emission
    // path is enough at this level.

    fn attach_static(state: &mut GameState, source: ObjectId, def: StaticDefinition) {
        let obj = state.objects.get_mut(&source).unwrap();
        Arc::make_mut(&mut obj.base_static_definitions).push(def.clone());
        obj.static_definitions.push(def);
    }

    #[test]
    fn attribution_static_source_keyword_grant() {
        let mut state = setup();
        let granter = make_creature(&mut state, "Akroma's Memorial", 0, 0, PlayerId(0));
        let target = make_creature(&mut state, "Goblin", 1, 1, PlayerId(0));

        attach_static(
            &mut state,
            granter,
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(TypedFilter::creature()))
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }]),
        );

        evaluate_layers(&mut state);

        let target_attr = state.attribution.get(&target).expect("target attributed");
        let ability_layer = target_attr
            .by_layer
            .get(&Layer::Ability)
            .expect("Ability layer entry present");
        assert_eq!(ability_layer.len(), 1, "exactly one grant on target");
        match ability_layer[0] {
            EffectRef::Static {
                source,
                def_index,
                mod_index,
            } => {
                assert_eq!(source, granter);
                assert_eq!(def_index, 0);
                assert_eq!(mod_index, 0, "single-mod static is index 0");
            }
            other => panic!("expected Static EffectRef, got {other:?}"),
        }
    }

    #[test]
    fn attribution_transient_source_pt_grant() {
        let mut state = setup();
        let granter = make_creature(&mut state, "Giant Growth Caster", 0, 0, PlayerId(0));
        let target = make_creature(&mut state, "Goblin", 1, 1, PlayerId(0));

        let id = state.add_transient_continuous_effect(
            granter,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target },
            vec![ContinuousModification::AddPower { value: 3 }],
            None,
        );

        evaluate_layers(&mut state);

        let target_attr = state.attribution.get(&target).expect("target attributed");
        let modify_pt = target_attr
            .by_layer
            .get(&Layer::ModifyPT)
            .expect("ModifyPT bucket present");
        match modify_pt[0] {
            EffectRef::Transient { id: tid, mod_index } => {
                assert_eq!(tid, id);
                assert_eq!(mod_index, 0, "single-mod transient is index 0");
            }
            other => panic!("expected Transient EffectRef, got {other:?}"),
        }
    }

    #[test]
    fn attribution_transient_snapshots_source_name() {
        let mut state = setup();
        let granter = make_creature(&mut state, "Giant Growth", 0, 0, PlayerId(0));
        let target = make_creature(&mut state, "Goblin", 1, 1, PlayerId(0));

        let id = state.add_transient_continuous_effect(
            granter,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target },
            vec![ContinuousModification::AddPower { value: 3 }],
            None,
        );

        let tce = state
            .transient_continuous_effects
            .iter()
            .find(|t| t.id == id)
            .expect("transient effect persisted");
        assert_eq!(
            tce.source_name, "Giant Growth",
            "source_name snapshotted at construction so attribution survives the spell's zone change"
        );
    }

    /// CR 400.7 + CR 603.10: A leaves-the-battlefield trigger that resolves a
    /// transient continuous effect runs AFTER its source has zone-changed out
    /// of `state.objects`. The lki_cache fallback recovers the source name
    /// from the snapshot captured when the source left the battlefield.
    #[test]
    fn attribution_transient_uses_lki_when_source_dead() {
        use crate::types::game_state::LKISnapshot;
        let mut state = setup();
        let dead_source = ObjectId(9999);
        let target = make_creature(&mut state, "Goblin", 1, 1, PlayerId(0));

        // Source is NOT in state.objects — simulating a creature whose LTB
        // trigger is resolving after the zone change has already happened.
        state.lki_cache.insert(
            dead_source,
            LKISnapshot {
                name: "Mortician Beetle".to_string(),
                power: Some(1),
                toughness: Some(1),
                base_power: Some(1),
                base_toughness: Some(1),
                mana_value: 1,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: std::collections::HashMap::new(),
            },
        );

        let id = state.add_transient_continuous_effect(
            dead_source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target },
            vec![ContinuousModification::AddPower { value: 1 }],
            None,
        );

        let tce = state
            .transient_continuous_effects
            .iter()
            .find(|t| t.id == id)
            .expect("transient effect persisted");
        assert_eq!(
            tce.source_name, "Mortician Beetle",
            "CR 400.7: lki_cache fallback recovers source name when source has left battlefield"
        );
    }

    #[test]
    fn attribution_multiple_sources_accumulate() {
        let mut state = setup();
        let lord_a = make_creature(&mut state, "Lord A", 2, 2, PlayerId(0));
        let lord_b = make_creature(&mut state, "Lord B", 2, 2, PlayerId(0));
        let target = make_creature(&mut state, "Goblin", 1, 1, PlayerId(0));

        let anthem = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::creature()))
            .modifications(vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ]);
        attach_static(&mut state, lord_a, anthem.clone());
        attach_static(&mut state, lord_b, anthem);

        evaluate_layers(&mut state);

        let modify_pt = state
            .attribution
            .get(&target)
            .and_then(|a| a.by_layer.get(&Layer::ModifyPT))
            .expect("ModifyPT bucket present");
        // Each lord contributes 2 modifications (power + toughness), and both
        // lords plus the target itself are creatures that the anthem affects —
        // so the target receives 4 distinct grants from the two sources.
        let sources: Vec<ObjectId> = modify_pt
            .iter()
            .filter_map(|r| match r {
                EffectRef::Static { source, .. } => Some(*source),
                _ => None,
            })
            .collect();
        assert!(
            sources.contains(&lord_a) && sources.contains(&lord_b),
            "attribution should record both lords as distinct sources, got {sources:?}",
        );
    }

    #[test]
    fn attribution_clears_between_passes() {
        let mut state = setup();
        let granter = make_creature(&mut state, "Anthem", 0, 0, PlayerId(0));
        let target = make_creature(&mut state, "Goblin", 1, 1, PlayerId(0));

        attach_static(
            &mut state,
            granter,
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(TypedFilter::creature()))
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }]),
        );
        evaluate_layers(&mut state);
        assert!(state.attribution.contains_key(&target));

        // Remove the granter from the battlefield. The attribution side-table
        // must rebuild fresh on the next layers pass and drop the stale entry.
        let granter_idx = state
            .battlefield
            .iter()
            .position(|id| *id == granter)
            .unwrap();
        state.battlefield.remove(granter_idx);
        evaluate_layers(&mut state);

        assert!(
            state
                .attribution
                .get(&target)
                .is_none_or(|a| !a.by_layer.contains_key(&Layer::Ability)),
            "attribution from a no-longer-on-battlefield source must not linger across passes"
        );
    }

    #[test]
    fn attribution_self_grant_is_emitted_engine_side() {
        // CR 113.3c + CR 604.3: A creature with a static ability that grants
        // itself a keyword (e.g., "this creature has trample") produces an
        // `ActiveContinuousEffect` whose source and target are the same
        // ObjectId. The engine emits attribution unconditionally; filtering
        // self-grants for display is a frontend concern, not an engine one.
        let mut state = setup();
        let creature = make_creature(&mut state, "Intrinsic Trampler", 2, 2, PlayerId(0));

        attach_static(
            &mut state,
            creature,
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Trample,
                }]),
        );

        evaluate_layers(&mut state);

        let attr = state
            .attribution
            .get(&creature)
            .expect("self-grant produces an attribution entry");
        let ability_layer = attr
            .by_layer
            .get(&Layer::Ability)
            .expect("Ability bucket present");
        match ability_layer[0] {
            EffectRef::Static { source, .. } => assert_eq!(
                source, creature,
                "self-grant source equals target — frontend filters this case for display",
            ),
            other => panic!("expected Static EffectRef, got {other:?}"),
        }
    }

    #[test]
    fn attribution_distinguishes_modifications_within_one_source() {
        // Akroma's Memorial pattern: one StaticDefinition with multiple keyword
        // grants. Each grant must produce a distinct EffectRef whose
        // `mod_index` lets the FE recover the specific ContinuousModification.
        let mut state = setup();
        let granter = make_creature(&mut state, "Akroma's Memorial", 0, 0, PlayerId(0));
        let target = make_creature(&mut state, "Goblin", 1, 1, PlayerId(0));

        attach_static(
            &mut state,
            granter,
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(TypedFilter::creature()))
                .modifications(vec![
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Flying,
                    },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Trample,
                    },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Vigilance,
                    },
                ]),
        );

        evaluate_layers(&mut state);

        let ability_layer = state
            .attribution
            .get(&target)
            .and_then(|a| a.by_layer.get(&Layer::Ability))
            .expect("Ability bucket present");
        let mod_indices: Vec<usize> = ability_layer
            .iter()
            .filter_map(|r| match r {
                EffectRef::Static { mod_index, .. } => Some(*mod_index),
                _ => None,
            })
            .collect();
        assert_eq!(
            mod_indices,
            vec![0, 1, 2],
            "each modification within the multi-mod source records its own mod_index",
        );
    }

    #[test]
    fn attribution_records_removal_modifications() {
        // RemoveKeyword flows through the same apply path as AddKeyword, so it
        // produces attribution. The FE distinguishes grant from removal by
        // dereferencing the EffectRef to the actual ContinuousModification.
        let mut state = setup();
        // Target with a base keyword we'll strip.
        let target = make_creature(&mut state, "Loses Flying", 2, 2, PlayerId(0));
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .base_keywords
            .push(Keyword::Flying);

        let stripper = make_creature(&mut state, "Hush", 0, 0, PlayerId(0));
        attach_static(
            &mut state,
            stripper,
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(TypedFilter::creature()))
                .modifications(vec![ContinuousModification::RemoveKeyword {
                    keyword: Keyword::Flying,
                }]),
        );

        evaluate_layers(&mut state);

        let ability_layer = state
            .attribution
            .get(&target)
            .and_then(|a| a.by_layer.get(&Layer::Ability))
            .expect("Ability bucket present for removal target");
        assert!(
            ability_layer
                .iter()
                .any(|r| matches!(r, EffectRef::Static { source, .. } if *source == stripper)),
            "RemoveKeyword produces an attribution entry just like AddKeyword",
        );
    }

    #[test]
    fn attribution_layer_copy_records_source() {
        // Layer 1 / CR 613.1a copy effects flow through the same
        // `apply_continuous_effect` chokepoint via the early copy-effects loop
        // in `evaluate_layers`. Verify that path emits attribution too.
        let mut state = setup();
        let source = make_creature(&mut state, "Mirror Source", 3, 3, PlayerId(0));
        let target = make_creature(&mut state, "Clone Target", 1, 1, PlayerId(0));

        let copy_values = crate::types::ability::CopiableValues {
            name: "Mirror Source".to_string(),
            mana_cost: ManaCost::default(),
            color: vec![],
            card_types: state.objects[&source].card_types.clone(),
            power: Some(3),
            toughness: Some(3),
            loyalty: None,
            keywords: vec![],
            abilities: Default::default(),
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
        };
        let _ = state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target },
            vec![ContinuousModification::CopyValues {
                values: Box::new(copy_values),
                display_source: crate::game::game_object::DisplaySource::Card,
                printed_ref: None,
                token_image_ref: None,
            }],
            None,
        );

        evaluate_layers(&mut state);

        let copy_layer = state
            .attribution
            .get(&target)
            .and_then(|a| a.by_layer.get(&Layer::Copy))
            .expect("Copy layer bucket present");
        assert_eq!(copy_layer.len(), 1, "exactly one Copy-layer attribution");
    }

    /// CR 113.3d + CR 604.1 + CR 611.2c + CR 613.1f: When a host static grants
    /// the equipped creature a quoted continuous static whose own affected
    /// scope is independent of the recipient ("Other commanders you control
    /// get +2/+2 and have lifelink"), the recipient must (a) hold the inner
    /// static on its `static_definitions` after layer evaluation, and (b) the
    /// inner static must then buff every matching object on the battlefield —
    /// driven through the actual `evaluate_layers` pipeline, not a hand-rolled
    /// expected state. This is the runtime end of the Dancer's Chakrams class.
    #[test]
    fn granted_static_ability_applies_inner_scope_to_other_objects() {
        let mut state = setup();

        // Equipped creature (the recipient of the granted static).
        let equipped = make_creature(&mut state, "Hero Token", 1, 1, PlayerId(0));
        // Another commander the equipped creature's controller controls.
        // The inner static is "Other commanders you control get +2/+2 and have
        // lifelink" — controller is the recipient's controller (PlayerId(0)),
        // and `Another` excludes the recipient itself even though the recipient
        // is also a commander in this scenario.
        let other_cmdr = make_creature(&mut state, "Other Commander", 3, 3, PlayerId(0));
        // A non-commander creature the same player controls — should NOT be buffed.
        let non_cmdr = make_creature(&mut state, "Plain Bear", 2, 2, PlayerId(0));
        // An opponent's commander — should NOT be buffed (controller mismatch).
        let opp_cmdr = make_creature(&mut state, "Opp Commander", 4, 4, PlayerId(1));

        // Mark the commanders.
        for &id in &[equipped, other_cmdr, opp_cmdr] {
            state.objects.get_mut(&id).unwrap().is_commander = true;
        }

        // Drive the production parser end-to-end so the test exercises the
        // exact shape Dancer's Chakrams produces (Permanent-typed inner filter,
        // ControllerRef::You + IsCommander + Another), not a hand-rolled
        // approximation. Closes the parser ↔ runtime loop in one test.
        let parsed = crate::parser::oracle_static::parse_quoted_ability_modifications(
            r#""Other commanders you control get +2/+2 and have lifelink.""#,
        );
        let inner_static = match parsed.as_slice() {
            [ContinuousModification::GrantStaticAbility { definition }] => (**definition).clone(),
            other => panic!("expected single GrantStaticAbility, got {:?}", other),
        };

        // The Equipment itself, with a static affecting EquippedBy that grants
        // the inner static. We don't model the full Dancer's Chakrams clause
        // here — only the granted-static piece, which is what this PR adds.
        let equipment = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Dancer's Chakrams".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&equipment).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.attached_to = Some(equipped.into());
            obj.timestamp = ts;

            let equipped_creature = TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
            );
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(equipped_creature)
                    .modifications(vec![ContinuousModification::GrantStaticAbility {
                        definition: Box::new(inner_static.clone()),
                    }]),
            );
        }
        state
            .objects
            .get_mut(&equipped)
            .unwrap()
            .attachments
            .push(equipment);

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        // (a) The recipient holds the inner static after layer evaluation.
        let recipient = state.objects.get(&equipped).unwrap();
        assert!(
            recipient
                .static_definitions
                .iter_all()
                .any(|sd| sd == &inner_static),
            "Equipped creature must hold the granted inner static after layer 6"
        );

        // (b) The other commander you control is buffed +2/+2 and has lifelink.
        let oc = state.objects.get(&other_cmdr).unwrap();
        assert_eq!(oc.power, Some(5), "Other commander: 3 base + 2 granted");
        assert_eq!(oc.toughness, Some(5), "Other commander: 3 base + 2 granted");
        assert!(
            oc.has_keyword(&Keyword::Lifelink),
            "Other commander must have lifelink from granted static"
        );

        // (c) The recipient itself is NOT buffed by the inner static
        // (`FilterProp::Another` excludes self).
        assert_eq!(
            recipient.power,
            Some(1),
            "Recipient is excluded by `Another` — power unchanged"
        );
        assert_eq!(
            recipient.toughness,
            Some(1),
            "Recipient toughness unchanged"
        );
        assert!(
            !recipient.has_keyword(&Keyword::Lifelink),
            "Recipient is excluded by `Another` — no lifelink"
        );

        // (d) Non-commander you control: not buffed (filter mismatch).
        let nc = state.objects.get(&non_cmdr).unwrap();
        assert_eq!(nc.power, Some(2), "Non-commander unaffected");
        assert_eq!(nc.toughness, Some(2), "Non-commander unaffected");
        assert!(
            !nc.has_keyword(&Keyword::Lifelink),
            "Non-commander no lifelink"
        );

        // (e) Opponent's commander: not buffed (controller mismatch).
        let opc = state.objects.get(&opp_cmdr).unwrap();
        assert_eq!(opc.power, Some(4), "Opponent commander unaffected");
        assert_eq!(opc.toughness, Some(4), "Opponent commander unaffected");
        assert!(
            !opc.has_keyword(&Keyword::Lifelink),
            "Opponent commander no lifelink"
        );
    }

    /// Adds a creature subtype to an object and re-snapshots its base card types so
    /// the layer reset preserves the printed subtype.
    fn add_subtype(state: &mut GameState, id: ObjectId, subtype: &str) {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.subtypes.push(subtype.to_string());
        obj.base_card_types = obj.card_types.clone();
    }

    fn recipient_filter_condition(text: &str) -> StaticCondition {
        let (rest, condition) = crate::parser::oracle_nom::condition::parse_condition(text)
            .expect("recipient condition should parse");
        assert_eq!(rest, "", "condition should fully consume: {text:?}");
        condition
    }

    /// CR 611.3a: SelfRef self-static "has defender as long as it's a Wall" — the
    /// anaphoric "it" binds to the source itself. A Wall creature therefore keeps
    /// Defender (Mistform Wall regression guard). Drives the real `evaluate_layers`.
    #[test]
    fn recipient_selfref_wall_keeps_defender() {
        let mut state = setup();
        let wall = make_creature(&mut state, "Mistform Wall", 0, 4, PlayerId(0));
        add_subtype(&mut state, wall, "Wall");

        let condition = recipient_filter_condition("as long as it's a Wall");
        let def = StaticDefinition::continuous()
            .condition(condition)
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Defender,
            }]);
        {
            let obj = state.objects.get_mut(&wall).unwrap();
            Arc::make_mut(&mut obj.base_static_definitions).push(def.clone());
            obj.static_definitions.push(def);
        }

        evaluate_layers(&mut state);
        assert!(
            state.objects[&wall].has_keyword(&Keyword::Defender),
            "Wall recipient matches the gate — Defender must be granted"
        );
    }

    /// CR 611.3a: SelfRef self-static gated "as long as it's a Wall" on a NON-Wall
    /// creature — recipient is the source, which is not a Wall, so the gate fails
    /// and the keyword is NOT granted. Complements the positive case above.
    #[test]
    fn recipient_selfref_nonwall_no_defender() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Grizzly Bears", 2, 2, PlayerId(0));

        let condition = recipient_filter_condition("as long as it's a Wall");
        let def = StaticDefinition::continuous()
            .condition(condition)
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Defender,
            }]);
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            Arc::make_mut(&mut obj.base_static_definitions).push(def.clone());
            obj.static_definitions.push(def);
        }

        evaluate_layers(&mut state);
        assert!(
            !state.objects[&bear].has_keyword(&Keyword::Defender),
            "Non-Wall recipient fails the gate — Defender must NOT be granted"
        );
    }

    /// CR 611.3a: per-recipient gating — an anthem-style static affecting all
    /// creatures, gated "as long as it's a Zombie", buffs ONLY the Zombie recipient
    /// and leaves the non-Zombie creature untouched. This is the Depala/Earth Surge
    /// correctness case: the gate is re-evaluated per affected object, not once for
    /// the source. Drives the real `evaluate_layers`.
    #[test]
    fn recipient_per_object_anthem_buffs_only_matching() {
        let mut state = setup();

        let anthem = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Zombie Lord".to_string(),
            Zone::Battlefield,
        );
        let anthem_ts = state.next_timestamp();
        let condition = recipient_filter_condition("as long as it's a Zombie");
        {
            let obj = state.objects.get_mut(&anthem).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = anthem_ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .condition(condition)
                    .affected(TargetFilter::Typed(TypedFilter::creature()))
                    .modifications(vec![
                        ContinuousModification::AddPower { value: 1 },
                        ContinuousModification::AddToughness { value: 1 },
                    ]),
            );
        }

        let zombie = make_creature(&mut state, "Zombie", 2, 2, PlayerId(0));
        add_subtype(&mut state, zombie, "Zombie");
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        evaluate_layers(&mut state);

        let zombie_obj = &state.objects[&zombie];
        assert_eq!(zombie_obj.power, Some(3), "Zombie recipient is buffed");
        assert_eq!(zombie_obj.toughness, Some(3), "Zombie recipient is buffed");

        let bear_obj = &state.objects[&bear];
        assert_eq!(
            bear_obj.power,
            Some(2),
            "Non-Zombie recipient must NOT be buffed (per-recipient gate)"
        );
        assert_eq!(
            bear_obj.toughness,
            Some(2),
            "Non-Zombie recipient must NOT be buffed (per-recipient gate)"
        );
    }

    /// CR 205.1a + CR 613.1d (Layer 4) + CR 105.3 + CR 613.1e (Layer 5):
    /// End-to-end confirmation of the Frogify class — a non-additive "is a 1/1
    /// blue Frog creature" Aura *replaces* the enchanted creature's card types,
    /// creature subtypes, and color, rather than adding to them. A Red Human
    /// Wizard 2/2 becomes exactly a 1/1 blue Frog creature with no residual
    /// Human/Wizard subtypes and no residual red color.
    #[test]
    fn frogify_aura_replaces_subtypes_color_and_type() {
        let mut state = setup();
        // RemoveAllSubtypes{Creature} resolves creature-type membership against
        // state.all_creature_types — the wipe and the new Frog must be known.
        state.all_creature_types = vec![
            "Human".to_string(),
            "Wizard".to_string(),
            "Frog".to_string(),
        ];

        let creature = make_creature(&mut state, "Human Wizard", 2, 2, PlayerId(0));
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.subtypes.push("Human".to_string());
            obj.card_types.subtypes.push("Wizard".to_string());
            obj.color = vec![ManaColor::Red];
            obj.base_card_types = obj.card_types.clone();
            obj.base_color = obj.color.clone();
        }

        // Create the Frogify Aura carrying the modifications the parser now emits.
        let aura = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Frogify".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.attached_to = Some(creature.into());
            obj.timestamp = ts;

            let enchanted_creature = TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            );
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(enchanted_creature)
                    .modifications(vec![
                        ContinuousModification::RemoveAllAbilities,
                        ContinuousModification::SetCardTypes {
                            core_types: vec![CoreType::Creature],
                        },
                        ContinuousModification::SetColor {
                            colors: vec![ManaColor::Blue],
                        },
                        ContinuousModification::SetPower { value: 1 },
                        ContinuousModification::SetToughness { value: 1 },
                        ContinuousModification::RemoveAllSubtypes {
                            set: SubtypeSet::Creature,
                        },
                        ContinuousModification::AddSubtype {
                            subtype: "Frog".to_string(),
                        },
                    ]),
            );
        }
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .attachments
            .push(aura);

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let c = state.objects.get(&creature).unwrap();
        assert_eq!(c.power, Some(1), "Frogify sets base power to 1");
        assert_eq!(c.toughness, Some(1), "Frogify sets base toughness to 1");
        assert_eq!(
            c.color,
            vec![ManaColor::Blue],
            "non-additive color must replace Red with Blue"
        );
        assert_eq!(
            c.card_types.subtypes,
            vec!["Frog".to_string()],
            "Human and Wizard must be wiped, only Frog remains: {:?}",
            c.card_types.subtypes
        );
        assert_eq!(
            c.card_types.core_types,
            vec![CoreType::Creature],
            "card types must be replaced with exactly Creature: {:?}",
            c.card_types.core_types
        );
    }

    /// CR 613.7a + CR 613.8a: A single static ability's modifications share one
    /// timestamp and apply in WRITTEN order; "depend on" (CR 613.8a) only
    /// sequences effects from DISTINCT generators and must never reorder one
    /// static's own clauses. This is the discriminating regression guard for
    /// the Frogify/Aura type-clearing bug.
    ///
    /// The graph is deliberately PARTIAL and ASYMMETRIC so the CR 613.8b
    /// cycle-fallback (which would otherwise silently return the pre-sorted
    /// written order and mask the bug) cannot rescue it. Two Type-layer (CR
    /// 613.1d) clauses share the static's one type-referencing filter:
    ///   0. `RemoveAllSubtypes{Creature}` — NOT in `depends_on`'s
    ///      `b_changes_types` set (it is a bulk wipe, not an Add/Remove of a
    ///      named type).
    ///   1. `AddSubtype{Frog}`            — IS in `b_changes_types`.
    ///
    /// With the guard suppressed, `depends_on` yields exactly ONE directed
    /// edge: `depends_on(RemoveAllSubtypes, AddSubtype) == true` (b adds a
    /// type, a's filter references a type) while the reverse is `false`
    /// (RemoveAllSubtypes is not a `b_changes_types` variant). One edge, no
    /// cycle → the toposort REORDERS `AddSubtype{Frog}` ahead of the wipe, so
    /// Frog is added then immediately wiped → subtypes become EMPTY.
    ///
    /// With the guard intact the intra-static edge is suppressed, the toposort
    /// falls through to the `mod_index` pre-sort (written) order: the wipe
    /// clears Human/Wizard FIRST, then `AddSubtype{Frog}` survives → exactly
    /// `[Frog]`. The assertion therefore passes ONLY when the guard preserves
    /// written order, and fails if the dependency reorder is allowed.
    #[test]
    fn same_static_modifications_apply_in_written_order() {
        let mut state = setup();
        state.all_creature_types = vec![
            "Human".to_string(),
            "Wizard".to_string(),
            "Frog".to_string(),
        ];

        // Creature with pre-existing creature subtypes that RemoveAllSubtypes
        // must wipe.
        let creature = make_creature(&mut state, "Human Wizard", 2, 2, PlayerId(0));
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.subtypes.push("Human".to_string());
            obj.card_types.subtypes.push("Wizard".to_string());
            obj.base_card_types = obj.card_types.clone();
        }

        // One static carrying, in written order:
        //   RemoveAllSubtypes{Creature} -> wipes Human, Wizard (applied FIRST)
        //   AddSubtype{Frog}            -> added AFTER the wipe, MUST survive
        // Written order is Frogify-correct; a dependency reorder would put the
        // Add before the wipe and erase Frog.
        let aura = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "TypeClearingAura".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.attached_to = Some(creature.into());
            obj.timestamp = ts;

            let enchanted_creature = TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            );
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(enchanted_creature)
                    .modifications(vec![
                        ContinuousModification::RemoveAllSubtypes {
                            set: SubtypeSet::Creature,
                        },
                        ContinuousModification::AddSubtype {
                            subtype: "Frog".to_string(),
                        },
                    ]),
            );
        }
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .attachments
            .push(aura);

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let c = state.objects.get(&creature).unwrap();
        assert_eq!(
            c.card_types.subtypes,
            vec!["Frog".to_string()],
            "written-order wipe-then-add must yield exactly [Frog]; a dependency \
             reorder of the Add ahead of RemoveAllSubtypes would erase Frog and \
             leave the subtype list empty: {:?}",
            c.card_types.subtypes
        );
    }

    /// CR 113.6c + CR 611.3a: Grist's "as long as ~ isn't on the battlefield"
    /// parses to `Not(SourceInZone { Battlefield })`. Its truth depends only on
    /// the SOURCE's own zone, never on board population, so the escalation
    /// classifier must report it population-INDEPENDENT — the load-bearing fact
    /// that keeps a colorless-Insect entry off the full-eval path on the real
    /// Scute board. (The parser side is covered in `oracle_nom::condition`; this
    /// guards the classifier that consumes the parsed shape.)
    #[test]
    fn grist_source_zone_condition_is_not_population_dependent() {
        let grist = StaticCondition::Not {
            condition: Box::new(StaticCondition::SourceInZone {
                zone: Zone::Battlefield,
            }),
        };
        assert!(
            !static_condition_uses_object_population(&grist),
            "Not(SourceInZone) gates on the source's own zone, not board \
             population — must not force escalation"
        );
        // And the bare affirmative reading is equally population-independent.
        assert!(!static_condition_uses_object_population(
            &StaticCondition::SourceInZone {
                zone: Zone::Battlefield,
            }
        ));
    }

    /// Build an anthem-style "creatures you control get +1/+1" continuous-static
    /// permanent (a generator) on the battlefield for `player`. Sets only
    /// `static_definitions`; `sync_missing_base_characteristics` (run at the top
    /// of the Step-1 reset) copies it into `base_static_definitions`, so the
    /// generator survives the per-pass reset.
    fn make_anthem(state: &mut GameState, name: &str, player: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(0),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.base_card_types = obj.card_types.clone();
        obj.timestamp = ts;
        obj.static_definitions.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ))
                .modifications(vec![
                    ContinuousModification::AddPower { value: 1 },
                    ContinuousModification::AddToughness { value: 1 },
                ]),
        );
        id
    }

    /// GAP-1 / GAP-B discriminating regression test (FIX B): a SECOND anthem that
    /// ENTERS the battlefield between layer evaluations must be picked up by the
    /// static-source index for its own pass.
    ///
    /// A PRE-EXISTING generator (anthem A) is seeded so the index is genuinely
    /// NON-EMPTY after the first `evaluate_layers` — this DISARMS the empty-index
    /// direct-scan fallback in `for_each_static_effect_source`. A vanilla seed
    /// would leave the index empty, the fallback would fire, and the entered
    /// anthem would be seen even on the buggy end-of-pass placement, so the test
    /// would NOT discriminate (this is exactly the FIX-B requirement).
    ///
    /// With the correct TOP-of-pass rebuild, the rebuild at the top of the
    /// (escalated) full eval includes anthem B before the gather, so BOTH A's and
    /// B's +1/+1 buffs apply → creature shows +2/+2. On the buggy end-of-pass
    /// rebuild placement (toggled via `REBUILD_STATIC_INDEX_AT_TOP = false`), the
    /// non-empty index from the previous pass is stale (missing B) during B's own
    /// pass, so only A's buff applies → creature shows +1/+1 → the test FAILS.
    #[test]
    fn entered_second_anthem_applies_with_preexisting_generator() {
        let mut state = setup();

        // PRE-EXISTING generator (anthem A) + a creature for the anthems to buff.
        make_anthem(&mut state, "Anthem A", PlayerId(0));
        let creature = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        // First full eval: index becomes {A} — NON-EMPTY, fallback disarmed.
        evaluate_layers(&mut state);
        assert!(
            !state.static_source_index.battlefield_sources.is_empty(),
            "pre-existing anthem A must populate the index so the empty-index \
             fallback is disarmed (FIX B)"
        );
        let after_a = state.objects.get(&creature).unwrap();
        assert_eq!(after_a.power, Some(3), "anthem A alone gives +1/+1");
        assert_eq!(after_a.toughness, Some(3));

        // Enter a SECOND anthem B via the real entry path. B is itself a
        // generator, so `entered_object_blocks_incremental` escalates the flush
        // to a full `evaluate_layers`, whose top-of-pass rebuild must include B.
        let b = make_anthem(&mut state, "Anthem B", PlayerId(0));
        mark_layers_entered(&mut state, b);
        flush_layers(&mut state);

        // BOTH anthems' buffs must apply: 2/2 base + A(+1/+1) + B(+1/+1) = 4/4.
        let after_b = state.objects.get(&creature).unwrap();
        assert_eq!(
            after_b.power,
            Some(4),
            "creature must receive BOTH anthem A's and the just-entered anthem \
             B's +1/+1 (the entered generator must be in the index for its own \
             pass — top-of-pass rebuild)"
        );
        assert_eq!(after_b.toughness, Some(4));
    }

    /// Sticker-granted continuous statics must participate in the same source
    /// index as printed generators, even when another generator already makes
    /// the index non-empty and therefore disarms the direct-scan fallback.
    #[test]
    fn sticker_static_source_is_indexed_with_preexisting_generator() {
        let mut state = setup();

        // Pre-existing generator disarms the empty-index fallback.
        make_anthem(&mut state, "Anthem A", PlayerId(0));
        let creature = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        crate::game::stickers::set_player_sticker_sheets(
            &mut state,
            PlayerId(0),
            &["Vampire Champion Fury".to_string()],
        );
        let hellbent = crate::game::stickers::available_sticker_candidates(
            &state,
            PlayerId(0),
            Some(crate::types::stickers::StickerKind::Ability),
            None,
            true,
        )
        .into_iter()
        .find(|candidate| {
            matches!(
                &candidate.sticker,
                crate::types::stickers::AppliedSticker::Ability { text, .. }
                    if text
                        == "Hellbent — This creature gets +3/+3 as long as you have no cards in hand."
            )
        })
        .expect("hellbent sticker available");
        let mut events = Vec::new();
        crate::game::stickers::apply_selected_sticker(
            &mut state,
            PlayerId(0),
            creature,
            hellbent.sticker,
            hellbent.pay_ticket,
            &mut events,
        );

        evaluate_layers(&mut state);

        assert!(
            state
                .static_source_index
                .battlefield_sources
                .iter()
                .any(|&id| id == creature),
            "sticker-granted continuous static source must be indexed in the \
             same pass, even when another generator keeps the fallback disabled"
        );
        let creature_obj = state.objects.get(&creature).unwrap();
        assert_eq!(creature_obj.power, Some(6));
        assert_eq!(creature_obj.toughness, Some(6));
    }

    /// FIX-B counterpart: the SAME scenario under the buggy end-of-pass rebuild
    /// placement MUST fail to apply anthem B's buff, proving the test above
    /// genuinely discriminates the placement (red on end-of-pass).
    #[test]
    fn entered_second_anthem_is_dropped_on_end_of_pass_rebuild() {
        // The toggle is THREAD-LOCAL, so flipping it affects only THIS test's
        // thread — concurrently-scheduled parallel tests read their own default
        // `true` and are unaffected. catch_unwind restores it for cleanliness in
        // case this thread is reused by a later test on the same worker.
        REBUILD_STATIC_INDEX_AT_TOP.with(|t| t.set(false));
        let result = std::panic::catch_unwind(|| {
            let mut state = setup();
            make_anthem(&mut state, "Anthem A", PlayerId(0));
            let creature = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
            evaluate_layers(&mut state);
            assert!(!state.static_source_index.battlefield_sources.is_empty());

            let b = make_anthem(&mut state, "Anthem B", PlayerId(0));
            mark_layers_entered(&mut state, b);
            flush_layers(&mut state);
            state.objects.get(&creature).unwrap().power
        });
        REBUILD_STATIC_INDEX_AT_TOP.with(|t| t.set(true));

        let power = result.expect("scenario should not panic");
        // On the buggy end-of-pass placement, the stale (non-empty) index is
        // missing B during B's pass, so only A's +1/+1 applies → 3, NOT 4.
        assert_eq!(
            power,
            Some(3),
            "end-of-pass placement must DROP the entered anthem B's buff — this \
             is the bug the top-of-pass rebuild fixes; if this is Some(4) the \
             test no longer discriminates the placement"
        );
    }

    /// Set + order identity: the index-driven gather must produce the same
    /// `collect_shared_active_continuous_effects` vector (element-for-element) as
    /// a full battlefield scan, including for a board with an unrelated vanilla
    /// permanent that the index correctly excludes.
    #[test]
    fn index_gather_matches_full_scan_set_and_order() {
        let mut state = setup();
        make_anthem(&mut state, "Anthem A", PlayerId(0));
        make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        // Unrelated vanilla permanent (NOT a generator) — must not appear as a
        // source in either path.
        make_creature(&mut state, "Vanilla", 1, 1, PlayerId(0));

        evaluate_layers(&mut state);

        // Index-driven gather (index is populated by the eval above).
        let indexed = collect_shared_active_continuous_effects(&state);

        // Force the empty-index direct-scan fallback by clearing the index, then
        // gather again — this exercises the full battlefield + command scan path.
        state.static_source_index = StaticSourceIndex::default();
        let full_scan = collect_shared_active_continuous_effects(&state);

        assert_eq!(
            indexed.len(),
            full_scan.len(),
            "index-driven and full-scan gathers must produce the same number of \
             effects"
        );
        for (i, (a, b)) in indexed.iter().zip(full_scan.iter()).enumerate() {
            assert_eq!(
                a.source_id, b.source_id,
                "effect {i}: source_id must match between index and full-scan"
            );
            assert_eq!(
                a.layer, b.layer,
                "effect {i}: layer must match between index and full-scan"
            );
        }
    }

    /// CR 611.3a + CR 613: End-to-end runtime confirmation that Shield of the
    /// Oversoul's color-conditional grants apply correctly on multicolored
    /// creatures. A green/white creature must receive BOTH clauses (+2/+2,
    /// flying, indestructible), a mono-green creature only the green clause
    /// (+1/+1, indestructible), and a mono-red creature neither. Regression
    /// test for issue #2674.
    #[test]
    fn shield_of_the_oversoul_color_conditional_on_multicolor_creature() {
        let mut state = setup();

        // Parse Shield of the Oversoul's two static lines via the production parser.
        let white_def = crate::parser::oracle_static::parse_static_line(
            "As long as enchanted creature is white, it gets +1/+1 and has flying.",
        )
        .expect("white clause must parse");
        let green_def = crate::parser::oracle_static::parse_static_line(
            "As long as enchanted creature is green, it gets +1/+1 and has indestructible.",
        )
        .expect("green clause must parse");

        // --- Multicolored (green/white) creature ---
        let gw_creature = make_creature(&mut state, "Wilt-Leaf Liege", 4, 4, PlayerId(0));
        {
            let obj = state.objects.get_mut(&gw_creature).unwrap();
            obj.color = vec![ManaColor::Green, ManaColor::White];
            obj.base_color = obj.color.clone();
        }

        let aura_gw = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Shield of the Oversoul (GW)".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&aura_gw).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.attached_to = Some(gw_creature.into());
            obj.timestamp = ts;
            obj.static_definitions.push(white_def.clone());
            obj.static_definitions.push(green_def.clone());
        }
        state
            .objects
            .get_mut(&gw_creature)
            .unwrap()
            .attachments
            .push(aura_gw);

        // --- Mono-green creature ---
        let g_creature = make_creature(&mut state, "Llanowar Elves", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&g_creature).unwrap();
            obj.color = vec![ManaColor::Green];
            obj.base_color = obj.color.clone();
        }

        let aura_g = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Shield of the Oversoul (G)".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&aura_g).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.attached_to = Some(g_creature.into());
            obj.timestamp = ts;
            obj.static_definitions.push(white_def.clone());
            obj.static_definitions.push(green_def.clone());
        }
        state
            .objects
            .get_mut(&g_creature)
            .unwrap()
            .attachments
            .push(aura_g);

        // --- Mono-red creature (no match) ---
        let r_creature = make_creature(&mut state, "Goblin Guide", 2, 2, PlayerId(0));
        {
            let obj = state.objects.get_mut(&r_creature).unwrap();
            obj.color = vec![ManaColor::Red];
            obj.base_color = obj.color.clone();
        }

        let aura_r = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Shield of the Oversoul (R)".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&aura_r).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.attached_to = Some(r_creature.into());
            obj.timestamp = ts;
            obj.static_definitions.push(white_def);
            obj.static_definitions.push(green_def);
        }
        state
            .objects
            .get_mut(&r_creature)
            .unwrap()
            .attachments
            .push(aura_r);

        // Run the full layer pipeline.
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        // Assert: multicolored (GW) creature gets both clauses.
        let gw = state.objects.get(&gw_creature).unwrap();
        assert_eq!(
            gw.power,
            Some(6),
            "GW creature: 4 base + 1 (white) + 1 (green) = 6"
        );
        assert_eq!(
            gw.toughness,
            Some(6),
            "GW creature: 4 base + 1 (white) + 1 (green) = 6"
        );
        assert!(
            gw.has_keyword(&Keyword::Flying),
            "GW creature must gain flying from white clause"
        );
        assert!(
            gw.has_keyword(&Keyword::Indestructible),
            "GW creature must gain indestructible from green clause"
        );

        // Assert: mono-green creature gets only the green clause.
        let g = state.objects.get(&g_creature).unwrap();
        assert_eq!(g.power, Some(2), "Mono-G creature: 1 base + 1 (green) = 2");
        assert_eq!(
            g.toughness,
            Some(2),
            "Mono-G creature: 1 base + 1 (green) = 2"
        );
        assert!(
            !g.has_keyword(&Keyword::Flying),
            "Mono-G creature must NOT gain flying (not white)"
        );
        assert!(
            g.has_keyword(&Keyword::Indestructible),
            "Mono-G creature must gain indestructible from green clause"
        );

        // Assert: mono-red creature gets nothing.
        let r = state.objects.get(&r_creature).unwrap();
        assert_eq!(r.power, Some(2), "Mono-R creature: 2 base, no bonus");
        assert_eq!(r.toughness, Some(2), "Mono-R creature: 2 base, no bonus");
        assert!(
            !r.has_keyword(&Keyword::Flying),
            "Mono-R creature must NOT gain flying"
        );
        assert!(
            !r.has_keyword(&Keyword::Indestructible),
            "Mono-R creature must NOT gain indestructible"
        );
    }
}
