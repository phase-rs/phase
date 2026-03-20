use std::collections::HashMap;

use crate::types::ability::{
    ControllerRef, EffectKind, FilterProp, TargetFilter, TargetRef, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::triggers::TriggerMatcher;

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

    // CR 722: Monarch triggers
    r.insert(TriggerMode::BecomeMonarch, match_become_monarch);

    // CR 706: Die rolling triggers
    r.insert(TriggerMode::RolledDie, match_rolled_die);
    r.insert(TriggerMode::RolledDieOnce, match_rolled_die);

    // CR 705: Coin flipping triggers
    r.insert(TriggerMode::FlippedCoin, match_flipped_coin);

    // CR 701.52: Ring tempts you trigger
    r.insert(TriggerMode::RingTemptsYou, match_ring_tempts_you);

    // CR 702.110c: Exploit trigger matcher
    r.insert(TriggerMode::Exploited, match_exploited);

    // Compound: enters or attacks — fires on ETB or attack events
    r.insert(TriggerMode::EntersOrAttacks, match_enters_or_attacks);

    // Compound: attacks or blocks — fires on attack or block events
    r.insert(TriggerMode::AttacksOrBlocks, match_attacks_or_blocks);

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
        TriggerMode::TakesInitiative,
        TriggerMode::LosesGame,
        TriggerMode::Championed,
        TriggerMode::Exerted,
        TriggerMode::Crewed,
        TriggerMode::Saddled,
        TriggerMode::Evolved,
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
        TriggerMode::Clashed,
        TriggerMode::Copied,
        TriggerMode::ConjureAll,
        TriggerMode::Vote,
        TriggerMode::BecomeRenowned,
        TriggerMode::BecomeMonstrous,
        TriggerMode::Proliferate,
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
    ];

    for mode in unimplemented_modes {
        r.insert(mode, match_unimplemented);
    }

    // Avatar crossover: bending trigger matchers
    r.insert(TriggerMode::Firebend, match_firebend);
    r.insert(TriggerMode::Airbend, match_airbend);
    r.insert(TriggerMode::Earthbend, match_earthbend);
    r.insert(TriggerMode::Waterbend, match_waterbend);
    r.insert(TriggerMode::ElementalBend, match_elemental_bend);

    r
}

// ---------------------------------------------------------------------------
// Helper: check ValidCard filter using either typed TargetFilter or string filter
// ---------------------------------------------------------------------------

/// Check if the trigger's valid_card filter matches the given object.
/// Uses the TargetFilter typed field if set; otherwise no filter (passes).
pub(super) fn valid_card_matches(
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
pub(super) fn valid_source_matches(
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

pub(super) fn valid_player_matches(
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
            controller: Some(ControllerRef::You),
            ..
        }) => trigger_controller == Some(player_id),
        TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            ..
        }) => trigger_controller.is_some_and(|controller| controller != player_id),
        _ => true,
    }
}

/// Basic runtime matching of a TargetFilter against a game object.
/// Handles the common filter patterns used in triggers.
pub(super) fn target_filter_matches_object(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> bool {
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

pub(super) fn match_changes_zone(
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

pub(super) fn match_changes_zone_all(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    // ChangesZoneAll triggers for any card changing zones, same logic
    match_changes_zone(event, trigger, source_id, state)
}

pub(super) fn match_damage_done(
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

pub(super) fn match_damage_done_once_by_controller(
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
                controller: Some(ControllerRef::You),
                ..
            }) if trigger_controller != Some(*player_id) => {
                return false;
            }
            TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
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

pub(super) fn match_spell_cast(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::SpellCast {
        controller,
        object_id,
        ..
    } = event
    {
        // Check valid_card filter on the cast spell
        if trigger.valid_card.is_some()
            && !valid_card_matches(trigger, state, *object_id, source_id)
        {
            return false;
        }
        valid_player_matches(trigger, state, *controller, source_id)
    } else {
        false
    }
}

pub(super) fn match_attacks(
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

/// Compound matcher for "Whenever ~ enters or attacks" — fires on either
/// a ZoneChanged-to-Battlefield event or an AttackersDeclared event for the source.
pub(super) fn match_enters_or_attacks(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match event {
        GameEvent::ZoneChanged { to, .. } if *to == Zone::Battlefield => {
            match_changes_zone(event, trigger, source_id, state)
        }
        GameEvent::AttackersDeclared { .. } => match_attacks(event, trigger, source_id, state),
        _ => false,
    }
}

/// Compound matcher for "Whenever ~ attacks or blocks" — fires on either
/// an AttackersDeclared event or a BlockersDeclared event for the source.
pub(super) fn match_attacks_or_blocks(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match event {
        GameEvent::AttackersDeclared { .. } => match_attacks(event, trigger, source_id, state),
        GameEvent::BlockersDeclared { .. } => match_blocks(event, trigger, source_id, state),
        _ => false,
    }
}

pub(super) fn match_attackers_declared(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::AttackersDeclared { .. })
}

pub(super) fn match_blocks(
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

pub(super) fn match_blockers_declared(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::BlockersDeclared { .. })
}

pub(super) fn match_countered(
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

pub(super) fn match_counter_added(
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

pub(super) fn match_counter_removed(
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

pub(super) fn match_taps(
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

pub(super) fn match_untaps(
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

pub(super) fn match_life_gained(
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

pub(super) fn match_life_lost(
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

pub(super) fn match_drawn(
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

pub(super) fn match_discarded(
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

pub(super) fn match_sacrificed(
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

pub(super) fn match_destroyed(
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

pub(super) fn match_token_created(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::TokenCreated { .. })
}

pub(super) fn match_turn_begin(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::TurnStarted { .. })
}

pub(super) fn match_phase(
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
pub(super) fn match_becomes_target(
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
pub(super) fn match_commit_crime(
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
pub(super) fn match_case_solved(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::CaseSolved { object_id } if *object_id == source_id)
}

/// CR 716.5: "When this Class becomes level N" triggers.
pub(super) fn match_class_level_gained(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::ClassLevelGained { object_id, .. } if *object_id == source_id)
}

pub(super) fn match_land_played(
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

pub(super) fn match_mana_added(
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
pub(super) fn match_attacker_blocked(
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
pub(super) fn match_attacker_unblocked(
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
pub(super) fn match_milled(
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
pub(super) fn match_exiled(
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
pub(super) fn match_attached(
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
pub(super) fn match_unattach(
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
pub(super) fn match_cycled(
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
pub(super) fn match_shuffled(
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
pub(super) fn match_revealed(
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
pub(super) fn match_taps_for_mana(
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
pub(super) fn match_changes_controller(
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
pub(super) fn match_transformed(
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
pub(super) fn match_fight(
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
pub(super) fn match_always(
    _event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    true
}

/// Explored: fires when a creature explores.
pub(super) fn match_explored(
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

/// CR 702.110c: "When this creature exploits" = source is the exploiter.
pub(super) fn match_exploited(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(
        event,
        GameEvent::CreatureExploited { exploiter, .. } if *exploiter == source_id
    )
}

/// TurnFaceUp: fires when a face-down creature is turned face up.
pub(super) fn match_turn_face_up(
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
pub(super) fn match_day_time_changes(
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
pub(super) fn match_leaves_battlefield(
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
pub(super) fn match_becomes_blocked(
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
pub(super) fn match_damage_received(
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
pub(super) fn match_you_attack(
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

/// CR 722: Matches when a player becomes the monarch.
/// Fires for "when you become the monarch" / "whenever a player becomes the monarch".
pub(super) fn match_become_monarch(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::MonarchChanged { player_id } = event {
        valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

///// CR 706: Match die roll events.
pub(super) fn match_rolled_die(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::DieRolled { player_id, .. } = event {
        valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

/// CR 705: Match coin flip events.
pub(super) fn match_flipped_coin(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CoinFlipped { player_id, .. } = event {
        valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

/// CR 701.52: Match "the Ring tempts you" events.
pub(super) fn match_ring_tempts_you(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::RingTemptsYou { player_id } = event {
        // The trigger fires for the controller of the source that has this trigger.
        let source_controller = state
            .objects
            .get(&_source_id)
            .map(|obj| obj.controller)
            .unwrap_or(PlayerId(255));
        *player_id == source_controller
    } else {
        false
    }
}

pub(super) fn match_unimplemented(
    _event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Avatar crossover: Bending trigger matchers
// ---------------------------------------------------------------------------

/// Matches GameEvent::Firebend for the controller of this trigger's source.
pub(super) fn match_firebend(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::Firebend { controller, .. } = event {
        let source_controller = state
            .objects
            .get(&_source_id)
            .map(|obj| obj.controller)
            .unwrap_or(PlayerId(255));
        *controller == source_controller
    } else {
        false
    }
}

/// Matches GameEvent::Airbend for the controller of this trigger's source.
pub(super) fn match_airbend(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::Airbend { controller, .. } = event {
        let source_controller = state
            .objects
            .get(&_source_id)
            .map(|obj| obj.controller)
            .unwrap_or(PlayerId(255));
        *controller == source_controller
    } else {
        false
    }
}

/// Matches GameEvent::Earthbend for the controller of this trigger's source.
pub(super) fn match_earthbend(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::Earthbend { controller, .. } = event {
        let source_controller = state
            .objects
            .get(&_source_id)
            .map(|obj| obj.controller)
            .unwrap_or(PlayerId(255));
        *controller == source_controller
    } else {
        false
    }
}

/// Matches GameEvent::Waterbend for the controller of this trigger's source.
pub(super) fn match_waterbend(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::Waterbend { controller, .. } = event {
        let source_controller = state
            .objects
            .get(&_source_id)
            .map(|obj| obj.controller)
            .unwrap_or(PlayerId(255));
        *controller == source_controller
    } else {
        false
    }
}

/// Matches any of the four bending GameEvents (for Avatar Aang's "whenever you
/// firebend, airbend, earthbend, or waterbend" trigger).
pub(super) fn match_elemental_bend(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    state: &GameState,
) -> bool {
    let controller = match event {
        GameEvent::Firebend { controller, .. }
        | GameEvent::Airbend { controller, .. }
        | GameEvent::Earthbend { controller, .. }
        | GameEvent::Waterbend { controller, .. } => controller,
        _ => return false,
    };
    let source_controller = state
        .objects
        .get(&_source_id)
        .map(|obj| obj.controller)
        .unwrap_or(PlayerId(255));
    *controller == source_controller
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        QuantityExpr, ResolvedAbility, TargetFilter, TriggerDefinition, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind};
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
            object_id: ObjectId(10),
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
                casting_variant: CastingVariant::Normal,
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
