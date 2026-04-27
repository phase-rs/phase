use std::collections::HashMap;

#[cfg(test)]
use crate::types::ability::FilterProp;
use crate::types::ability::{
    ControllerRef, DamageKindFilter, EffectKind, TargetFilter, TargetRef, TriggerDefinition,
    TypedFilter,
};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{GameState, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::triggers::TriggerMatcher;

pub fn trigger_matcher(mode: TriggerMode) -> Option<TriggerMatcher> {
    Some(match mode {
        TriggerMode::ChangesZone => match_changes_zone,
        TriggerMode::ChangesZoneAll => match_changes_zone_all,
        TriggerMode::DamageDone
        | TriggerMode::DamageDoneOnce
        | TriggerMode::DamageAll
        | TriggerMode::DamageDealtOnce => match_damage_done,
        TriggerMode::DamageDoneOnceByController => match_damage_done_once_by_controller,
        TriggerMode::SpellCast | TriggerMode::SpellCastOrCopy | TriggerMode::SpellCopy => {
            match_spell_cast
        }
        TriggerMode::Attacks => match_attacks,
        TriggerMode::AttackersDeclared | TriggerMode::AttackersDeclaredOneTarget => {
            match_attackers_declared
        }
        TriggerMode::Blocks => match_blocks,
        TriggerMode::BlockersDeclared => match_blockers_declared,
        TriggerMode::Countered => match_countered,
        TriggerMode::CounterAdded
        | TriggerMode::CounterAddedOnce
        | TriggerMode::CounterAddedAll => match_counter_added,
        TriggerMode::CounterRemoved | TriggerMode::CounterRemovedOnce => match_counter_removed,
        TriggerMode::Taps | TriggerMode::TapAll => match_taps,
        TriggerMode::Untaps | TriggerMode::UntapAll => match_untaps,
        TriggerMode::LifeGained => match_life_gained,
        TriggerMode::LifeLost | TriggerMode::LifeLostAll => match_life_lost,
        TriggerMode::Drawn => match_drawn,
        TriggerMode::Discarded | TriggerMode::DiscardedAll => match_discarded,
        TriggerMode::Sacrificed | TriggerMode::SacrificedOnce => match_sacrificed,
        TriggerMode::Destroyed => match_destroyed,
        TriggerMode::TokenCreated | TriggerMode::TokenCreatedOnce => match_token_created,
        TriggerMode::TurnBegin => match_turn_begin,
        TriggerMode::Phase => match_phase,
        TriggerMode::BecomesTarget | TriggerMode::BecomesTargetOnce => match_becomes_target,
        TriggerMode::LandPlayed => match_land_played,
        TriggerMode::ManaAdded => match_mana_added,
        TriggerMode::SearchedLibrary
        | TriggerMode::Scry
        | TriggerMode::Surveil
        | TriggerMode::CollectEvidence
        | TriggerMode::PlayerPerformedAction => match_player_action,
        TriggerMode::LeavesBattlefield => match_leaves_battlefield,
        TriggerMode::BecomesBlocked => match_becomes_blocked,
        TriggerMode::YouAttack => match_you_attack,
        TriggerMode::DamageReceived => match_damage_received,
        TriggerMode::ExcessDamage => match_excess_damage,
        TriggerMode::ExcessDamageAll => match_excess_damage_all,
        TriggerMode::AttackerBlocked
        | TriggerMode::AttackerBlockedOnce
        | TriggerMode::AttackerBlockedByCreature => match_attacker_blocked,
        TriggerMode::AttackerUnblocked | TriggerMode::AttackerUnblockedOnce => {
            match_attacker_unblocked
        }
        TriggerMode::Milled | TriggerMode::MilledOnce | TriggerMode::MilledAll => match_milled,
        TriggerMode::Exiled => match_exiled,
        TriggerMode::Attached => match_attached,
        TriggerMode::Unattach => match_unattach,
        TriggerMode::Cycled => match_cycled,
        TriggerMode::CycledOrDiscarded => match_cycled_or_discarded,
        TriggerMode::Shuffled => match_shuffled,
        TriggerMode::Revealed => match_revealed,
        TriggerMode::TapsForMana => match_taps_for_mana,
        TriggerMode::ChangesController => match_changes_controller,
        TriggerMode::Transformed => match_transformed,
        TriggerMode::Fight | TriggerMode::FightOnce => match_fight,
        TriggerMode::Immediate | TriggerMode::Always => match_always,
        TriggerMode::Explored => match_explored,
        TriggerMode::TurnFaceUp => match_turn_face_up,
        TriggerMode::ManifestDread => match_manifest_dread,
        TriggerMode::DayTimeChanges => match_day_time_changes,
        TriggerMode::CommitCrime => match_commit_crime,
        TriggerMode::CaseSolved => match_case_solved,
        TriggerMode::ClassLevelGained => match_class_level_gained,
        TriggerMode::BecomeMonarch => match_become_monarch,
        TriggerMode::RolledDie | TriggerMode::RolledDieOnce => match_rolled_die,
        TriggerMode::FlippedCoin => match_flipped_coin,
        TriggerMode::RingTemptsYou => match_ring_tempts_you,
        TriggerMode::DungeonCompleted => match_dungeon_completed,
        TriggerMode::RoomEntered => match_room_entered,
        TriggerMode::TakesInitiative => match_takes_initiative,
        TriggerMode::Exploited => match_exploited,
        TriggerMode::BecomeMonstrous => match_become_monstrous,
        TriggerMode::ManaExpend => match_mana_expend,
        TriggerMode::EntersOrAttacks => match_enters_or_attacks,
        TriggerMode::AttacksOrBlocks => match_attacks_or_blocks,
        TriggerMode::Crewed | TriggerMode::BecomesCrewed => match_vehicle_crewed,
        TriggerMode::Stationed => match_stationed,
        TriggerMode::Saddled | TriggerMode::BecomesSaddled => match_saddled,
        TriggerMode::Crews => match_crews,
        TriggerMode::Saddles => match_saddles,
        TriggerMode::SaddlesOrCrews => match_saddles_or_crews,
        TriggerMode::NinjutsuActivated => match_ninjutsu_activated,
        TriggerMode::Firebend => match_firebend,
        TriggerMode::Airbend => match_airbend,
        TriggerMode::Earthbend => match_earthbend,
        TriggerMode::Waterbend => match_waterbend,
        TriggerMode::ElementalBend => match_elemental_bend,
        TriggerMode::DamagePreventedOnce
        | TriggerMode::AbilityCast
        | TriggerMode::AbilityResolves
        | TriggerMode::AbilityTriggered
        | TriggerMode::SpellAbilityCast
        | TriggerMode::SpellAbilityCopy
        | TriggerMode::CounterPlayerAddedAll
        | TriggerMode::CounterTypeAddedAll
        | TriggerMode::PayLife
        | TriggerMode::PayCumulativeUpkeep
        | TriggerMode::PayEcho
        | TriggerMode::PhaseIn
        | TriggerMode::PhaseOut
        | TriggerMode::PhaseOutAll
        | TriggerMode::NewGame
        | TriggerMode::LosesGame
        | TriggerMode::Championed
        | TriggerMode::Exerted
        | TriggerMode::Evolved
        | TriggerMode::Enlisted
        | TriggerMode::Adapt
        | TriggerMode::Foretell
        | TriggerMode::Investigated
        | TriggerMode::PlanarDice
        | TriggerMode::PlaneswalkedFrom
        | TriggerMode::PlaneswalkedTo
        | TriggerMode::ChaosEnsues
        | TriggerMode::Clashed
        | TriggerMode::Copied
        | TriggerMode::ConjureAll
        | TriggerMode::Vote
        | TriggerMode::BecomeRenowned
        | TriggerMode::Proliferate
        | TriggerMode::Abandoned
        | TriggerMode::ClaimPrize
        | TriggerMode::CrankContraption
        | TriggerMode::Devoured
        | TriggerMode::Discover
        | TriggerMode::Forage
        | TriggerMode::FullyUnlock
        | TriggerMode::GiveGift
        | TriggerMode::Mentored
        | TriggerMode::Mutates
        | TriggerMode::SeekAll
        | TriggerMode::SetInMotion
        | TriggerMode::Specializes
        | TriggerMode::Trains
        | TriggerMode::UnlockDoor
        | TriggerMode::VisitAttraction
        | TriggerMode::BecomesPlotted => match_unimplemented,
        // CR 603.8: State triggers are not event-based — they are checked separately
        // in the priority pipeline, not through the event-matching trigger system.
        TriggerMode::StateCondition => return None,
        TriggerMode::Unknown(_) => return None,
    })
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
    r.insert(TriggerMode::SearchedLibrary, match_player_action);
    r.insert(TriggerMode::Scry, match_player_action);
    r.insert(TriggerMode::Surveil, match_player_action);
    r.insert(TriggerMode::CollectEvidence, match_player_action);
    r.insert(TriggerMode::PlayerPerformedAction, match_player_action);

    // Zone-based: leaves the battlefield
    r.insert(TriggerMode::LeavesBattlefield, match_leaves_battlefield);

    // Combat: becomes blocked, you attack
    r.insert(TriggerMode::BecomesBlocked, match_becomes_blocked);
    r.insert(TriggerMode::YouAttack, match_you_attack);

    // Damage: is dealt damage
    r.insert(TriggerMode::DamageReceived, match_damage_received);

    // CR 120.10: Excess damage triggers
    r.insert(TriggerMode::ExcessDamage, match_excess_damage);
    r.insert(TriggerMode::ExcessDamageAll, match_excess_damage_all);

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
    r.insert(TriggerMode::CycledOrDiscarded, match_cycled_or_discarded);
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
    // CR 701.62: Manifest Dread actor-side trigger.
    r.insert(TriggerMode::ManifestDread, match_manifest_dread);

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

    // CR 701.54: Ring tempts you trigger
    r.insert(TriggerMode::RingTemptsYou, match_ring_tempts_you);

    // CR 309 / CR 701.49: Dungeon triggers
    r.insert(TriggerMode::DungeonCompleted, match_dungeon_completed);
    r.insert(TriggerMode::RoomEntered, match_room_entered);
    // CR 725: Initiative triggers
    r.insert(TriggerMode::TakesInitiative, match_takes_initiative);

    // CR 702.110a: Exploit trigger matcher
    r.insert(TriggerMode::Exploited, match_exploited);

    // CR 701.37a: "When ~ becomes monstrous" — self-trigger on Monstrosity resolution.
    r.insert(TriggerMode::BecomeMonstrous, match_become_monstrous);

    // CR 700.14: Expend trigger — cumulative mana spent on spells
    r.insert(TriggerMode::ManaExpend, match_mana_expend);

    // Compound: enters or attacks — fires on ETB or attack events
    r.insert(TriggerMode::EntersOrAttacks, match_enters_or_attacks);

    // Compound: attacks or blocks — fires on attack or block events
    r.insert(TriggerMode::AttacksOrBlocks, match_attacks_or_blocks);

    // Remaining trigger modes: recognized but not yet matched against events.
    let unimplemented_modes = [
        TriggerMode::DamagePreventedOnce,
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
        // TriggerMode::TakesInitiative — moved to real matcher above
        TriggerMode::LosesGame,
        TriggerMode::Championed,
        TriggerMode::Exerted,
        // TriggerMode::Crewed — moved to real matcher below
        // TriggerMode::Saddled — moved to real matcher below
        TriggerMode::Evolved,
        TriggerMode::Enlisted,
        TriggerMode::Adapt,
        TriggerMode::Foretell,
        TriggerMode::Investigated,
        // TriggerMode::DungeonCompleted — moved to real matcher above
        // TriggerMode::RoomEntered — moved to real matcher above
        TriggerMode::PlanarDice,
        TriggerMode::PlaneswalkedFrom,
        TriggerMode::PlaneswalkedTo,
        TriggerMode::ChaosEnsues,
        TriggerMode::Clashed,
        TriggerMode::Copied,
        TriggerMode::ConjureAll,
        TriggerMode::Vote,
        TriggerMode::BecomeRenowned,
        TriggerMode::Proliferate,
        TriggerMode::Abandoned,
        TriggerMode::ClaimPrize,
        TriggerMode::CrankContraption,
        TriggerMode::Devoured,
        TriggerMode::Discover,
        TriggerMode::Forage,
        TriggerMode::FullyUnlock,
        TriggerMode::GiveGift,
        TriggerMode::Mentored,
        TriggerMode::Mutates,
        TriggerMode::SeekAll,
        TriggerMode::SetInMotion,
        TriggerMode::Specializes,
        // TriggerMode::Stationed — moved to real matcher below
        TriggerMode::Trains,
        TriggerMode::UnlockDoor,
        TriggerMode::VisitAttraction,
        // TriggerMode::BecomesCrewed — moved to real matcher below
        TriggerMode::BecomesPlotted,
        // TriggerMode::BecomesSaddled — moved to real matcher below
    ];

    for mode in unimplemented_modes {
        r.insert(mode, match_unimplemented);
    }

    // CR 702.122d: Crew trigger matchers
    r.insert(TriggerMode::Crewed, match_vehicle_crewed);
    r.insert(TriggerMode::BecomesCrewed, match_vehicle_crewed);

    // CR 702.184a: Station trigger matcher — "Whenever ~ is stationed" fires
    // when the station ability resolves for this specific Spacecraft.
    r.insert(TriggerMode::Stationed, match_stationed);

    // CR 702.171a + CR 702.171b: Saddle trigger matchers — "Whenever ~ is
    // saddled" fires when the saddle ability resolves for this specific Mount.
    r.insert(TriggerMode::Saddled, match_saddled);
    r.insert(TriggerMode::BecomesSaddled, match_saddled);

    // CR 702.122 + CR 702.171c: Actor-side Saddle/Crew matchers — consult
    // `valid_card` against event.creatures via matches_target_filter so that
    // compound subjects (e.g., Tiana) fire on the non-self branch.
    r.insert(TriggerMode::Crews, match_crews);
    r.insert(TriggerMode::Saddles, match_saddles);
    r.insert(TriggerMode::SaddlesOrCrews, match_saddles_or_crews);

    // CR 702.49a: Ninjutsu activation trigger
    r.insert(TriggerMode::NinjutsuActivated, match_ninjutsu_activated);

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
    player_matches_filter(filter, state, player_id, source_id)
}

/// Check if a player matches a TargetFilter directly.
/// Shared implementation used by both `valid_player_matches` (from trigger.valid_target)
/// and `match_damage_done` (from explicit damage target filter).
fn player_matches_filter(
    filter: &TargetFilter,
    state: &GameState,
    player_id: PlayerId,
    source_id: ObjectId,
) -> bool {
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
    match filter {
        TargetFilter::None => false,
        TargetFilter::Player => false,
        TargetFilter::Controller => false,
        // SpecificPlayer scopes to a player, not an object — never matches an object.
        TargetFilter::SpecificPlayer { .. } => false,
        TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::DefendingPlayer
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::StackAbility
        | TargetFilter::StackSpell
        | TargetFilter::Owner => false,
        TargetFilter::Any
        | TargetFilter::SelfRef
        | TargetFilter::Typed(_)
        | TargetFilter::Not { .. }
        | TargetFilter::Or { .. }
        | TargetFilter::And { .. }
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::HasChosenName
        | TargetFilter::Named { .. } => super::filter::matches_target_filter(
            state,
            object_id,
            filter,
            &super::filter::FilterContext::from_source(state, source_id),
        ),
    }
}

// ---------------------------------------------------------------------------
// Core Trigger Matchers (~20 with real logic)
// ---------------------------------------------------------------------------

// CR 603.6: ZoneChange triggers when an object enters or leaves a zone.
pub(super) fn match_changes_zone(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::ZoneChanged {
        object_id: _,
        from,
        to,
        record,
    } = event
    {
        // CR 603.10a: Check origin zone(s). Disjunctive `origin_zones` takes
        // precedence over single-zone `origin` when non-empty — this supports
        // "put into exile from your library and/or your graveyard" where the
        // source zone can be one of several zones.
        //
        // CR 111.1 + CR 603.6a: `from = None` means the object was created
        // directly in `to` (token creation / emblem). Any trigger that names
        // a specific origin zone cannot match such an event; a trigger with
        // no origin filter (e.g. Elvish Vanguard's "whenever another Elf
        // enters") falls through these guards and matches.
        match from {
            Some(from_zone) => {
                if !trigger.origin_zones.is_empty() {
                    if !trigger.origin_zones.contains(from_zone) {
                        return false;
                    }
                } else if let Some(origin) = &trigger.origin {
                    if origin != from_zone {
                        return false;
                    }
                }
            }
            None => {
                if !trigger.origin_zones.is_empty() || trigger.origin.is_some() {
                    return false;
                }
            }
        }
        // Check destination zone using typed field
        if let Some(destination) = &trigger.destination {
            if destination != to {
                return false;
            }
        }
        // Check valid_card filter
        if let Some(filter) = &trigger.valid_card {
            let ctx = super::filter::FilterContext::from_source(state, source_id);
            if !super::filter::matches_target_filter_on_zone_change_record(
                state, record, filter, &ctx,
            ) {
                return false;
            }
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

// CR 603.6d: DamageDone trigger fires on damage dealt events.
pub(super) fn match_damage_done(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::DamageDealt {
        source_id: dmg_source,
        target,
        is_combat,
        ..
    } = event
    {
        // Check if trigger requires damage from a specific source
        if !valid_source_matches(trigger, state, *dmg_source, source_id) {
            return false;
        }
        // CR 120.3: Check damage kind filter (combat/noncombat/any)
        match trigger.damage_kind {
            DamageKindFilter::Any => {}
            DamageKindFilter::CombatOnly if !is_combat => return false,
            DamageKindFilter::NoncombatOnly if *is_combat => return false,
            _ => {}
        }
        // Check valid_target for damage target filtering (e.g. "to an opponent")
        if let Some(ref vt) = trigger.valid_target {
            match target {
                TargetRef::Player(pid) => {
                    if !player_matches_filter(vt, state, *pid, source_id) {
                        return false;
                    }
                }
                TargetRef::Object(oid) => {
                    if !target_filter_matches_object(state, *oid, vt, source_id) {
                        return false;
                    }
                }
            }
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

// CR 603.6a: SpellCast trigger fires when a spell is placed on the stack.
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
        // CR 115.9c: Check "that targets only [X]" constraint against the spell's actual targets.
        if let Some(targets_only_filter) = trigger
            .valid_card
            .as_ref()
            .and_then(super::filter::extract_targets_only)
        {
            if !stack_entry_targets_only(state, *object_id, &targets_only_filter, source_id) {
                return false;
            }
        }
        // CR 115.9b: Check "that targets [X]" constraint (.any() semantics).
        if let Some(targets_filter) = trigger
            .valid_card
            .as_ref()
            .and_then(super::filter::extract_targets)
        {
            if !stack_entry_targets_any(state, *object_id, &targets_filter, source_id) {
                return false;
            }
        }
        valid_player_matches(trigger, state, *controller, source_id)
    } else {
        false
    }
}

// CR 508.1a + CR 603.2: Attacks trigger fires when a creature is declared as an attacker.
pub(super) fn match_attacks(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    !matching_attack_events(event, trigger, source_id, state).is_empty()
}

pub(super) fn matching_attack_events(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> Vec<GameEvent> {
    if let GameEvent::AttackersDeclared {
        attacker_ids,
        defending_player,
        attacks,
        ..
    } = event
    {
        // Find which attacker(s) satisfy the creature filter
        let attacker_matches = |id: &ObjectId| -> bool {
            if trigger.valid_card.is_some() {
                valid_card_matches(trigger, state, *id, source_id)
            } else {
                *id == source_id
            }
        };

        attacker_ids
            .iter()
            .filter_map(|id| {
                if !attacker_matches(id) {
                    return None;
                }
                let target = attacks
                    .iter()
                    .find_map(|(attacker_id, target)| (*attacker_id == *id).then_some(*target))
                    .unwrap_or(crate::game::combat::AttackTarget::Player(*defending_player));
                if !attack_target_matches(trigger, state, target, *defending_player, source_id) {
                    return None;
                }
                Some(GameEvent::AttackersDeclared {
                    attacker_ids: vec![*id],
                    defending_player: attack_target_defending_player(
                        state,
                        target,
                        *defending_player,
                    ),
                    attacks: vec![(*id, target)],
                })
            })
            .collect()
    } else {
        Vec::new()
    }
}

fn attack_target_matches(
    trigger: &TriggerDefinition,
    state: &GameState,
    target: crate::game::combat::AttackTarget,
    fallback_defending_player: PlayerId,
    source_id: ObjectId,
) -> bool {
    if let Some(filter) = trigger.attack_target_filter.as_ref() {
        let type_matches = matches!(
            (filter, target),
            (
                crate::types::triggers::AttackTargetFilter::Player,
                crate::game::combat::AttackTarget::Player(_)
            ) | (
                crate::types::triggers::AttackTargetFilter::Planeswalker,
                crate::game::combat::AttackTarget::Planeswalker(_)
            ) | (
                crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker,
                crate::game::combat::AttackTarget::Player(_)
                    | crate::game::combat::AttackTarget::Planeswalker(_)
            ) | (
                crate::types::triggers::AttackTargetFilter::Battle,
                crate::game::combat::AttackTarget::Battle(_)
            )
        );
        if !type_matches {
            return false;
        }
    }

    if trigger.valid_target.is_some() {
        let defending_player =
            attack_target_defending_player(state, target, fallback_defending_player);
        valid_player_matches(trigger, state, defending_player, source_id)
    } else {
        true
    }
}

fn attack_target_defending_player(
    state: &GameState,
    target: crate::game::combat::AttackTarget,
    fallback_defending_player: PlayerId,
) -> PlayerId {
    match target {
        crate::game::combat::AttackTarget::Player(player) => player,
        crate::game::combat::AttackTarget::Planeswalker(object_id) => state
            .objects
            .get(&object_id)
            .map(|object| object.controller)
            .unwrap_or(fallback_defending_player),
        crate::game::combat::AttackTarget::Battle(object_id) => state
            .objects
            .get(&object_id)
            .and_then(|object| object.protector())
            .unwrap_or(fallback_defending_player),
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
        counter_type,
        ..
    } = event
    {
        if !valid_card_matches(trigger, state, *object_id, source_id) {
            return false;
        }
        // CR 310.11b + CR 714.2a-mirror: Apply counter filter (type + optional
        // "crossed zero" threshold). Used by the Siege victory trigger
        // "When the last defense counter is removed from this permanent".
        // A threshold of Some(0) means "fire only when the current count
        // dropped to 0" — i.e., the last counter was just removed.
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
                if threshold == 0 {
                    // "Last counter removed" — fire only when post-removal count is 0.
                    if current != 0 {
                        return false;
                    }
                } else {
                    return false;
                }
            }
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
    if let GameEvent::PermanentTapped {
        object_id,
        caused_by,
    } = event
    {
        // If valid_card is set, check the tapped object matches (e.g. "opponent's creature")
        if trigger.valid_card.is_some() {
            if !valid_card_matches(trigger, state, *object_id, source_id) {
                return false;
            }
            // CR 701.21: "you tap an untapped creature an opponent controls" requires
            // an external cause. Only apply caused_by gating when the trigger explicitly
            // filters for opponent-controlled objects.
            let requires_opponent = matches!(
                &trigger.valid_card,
                Some(TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::Opponent),
                    ..
                }))
            );
            if requires_opponent {
                match caused_by {
                    Some(cause_id) => {
                        // The cause must be controlled by the trigger's controller
                        let trigger_controller =
                            state.objects.get(&source_id).map(|o| o.controller);
                        let cause_controller = state.objects.get(cause_id).map(|o| o.controller);
                        if trigger_controller != cause_controller {
                            return false;
                        }
                    }
                    None => {
                        // Self-initiated tap — doesn't qualify as "you tap opponent's creature"
                        return false;
                    }
                }
            }
            true
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

pub(super) fn match_player_action(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::PlayerPerformedAction { player_id, action } = event else {
        return false;
    };
    if !valid_player_matches(trigger, state, *player_id, source_id) {
        return false;
    }

    match trigger.mode {
        TriggerMode::SearchedLibrary => *action == PlayerActionKind::SearchedLibrary,
        TriggerMode::Scry => *action == PlayerActionKind::Scry,
        TriggerMode::Surveil => *action == PlayerActionKind::Surveil,
        TriggerMode::CollectEvidence => *action == PlayerActionKind::CollectEvidence,
        TriggerMode::PlayerPerformedAction => trigger
            .player_actions
            .as_ref()
            .is_some_and(|actions| actions.contains(action)),
        _ => false,
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

// CR 111.1 + CR 603.2: TokenCreated triggers fire on token-creation events.
// The token is already on the battlefield when the event is emitted (CR 111.7),
// so `state.objects[object_id]` carries the token's real controller and card
// types — used to evaluate the trigger's `valid_card` (type filter) and
// `valid_target` (controller-scope filter, e.g., `ControllerRef::You`).
pub(super) fn match_token_created(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::TokenCreated { object_id, .. } = event else {
        return false;
    };
    if !valid_card_matches(trigger, state, *object_id, source_id) {
        return false;
    }
    // CR 111.10: The token's controller is the player who created it.
    if let Some(token_controller) = state.objects.get(object_id).map(|o| o.controller) {
        if !valid_player_matches(trigger, state, token_controller, source_id) {
            return false;
        }
    }
    true
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

// CR 603.4: Match when the trigger's source becomes the target of a spell or ability.
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

    // CR 115.1a: Check source filter — "of a spell" restricts to StackEntryKind::Spell
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

/// CR 716.2a: "When this Class becomes level N" triggers.
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
        if *from != Some(Zone::Library) || *to != Zone::Graveyard {
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
/// CR 303.4 + CR 301.5: Only fires when the host was an object that left the
/// battlefield. A player host (Curse cycle) leaves via game-loss, which is a
/// different SBA path (CR 704.5m for the Aura) — not modeled by this matcher.
pub(super) fn match_unattach(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match event {
        GameEvent::ZoneChanged {
            object_id, from, ..
        } if *from == Some(Zone::Battlefield) => {
            // Check if source was attached to the object that left
            state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.attached_to)
                .and_then(|t| t.as_object())
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

/// CR 702.29d: CycledOrDiscarded — fires on either Cycled or Discarded events.
/// Per CR 702.29d, triggers only once when a card is cycled (cycling is a form of discarding).
pub(super) fn match_cycled_or_discarded(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match event {
        GameEvent::Cycled { object_id, .. } | GameEvent::Discarded { object_id, .. } => {
            valid_card_matches(trigger, state, *object_id, source_id)
        }
        _ => false,
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
        tapped_for_mana,
        ..
    } = event
    {
        // Only fire for actual mana ability activations (tap costs), not for mana
        // produced by triggered abilities, effects, convoke, or doublers.
        // This prevents infinite loops (e.g., Badgermole Cub's trigger producing
        // mana that re-triggers itself).
        if !tapped_for_mana {
            return false;
        }

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

/// CR 712.14: Transformed trigger — fires when an object transforms.
/// Uses `GameEvent::Transformed { object_id }` which carries the actual transforming object.
/// If `valid_source` is set (e.g., `SelfRef` for "~ transforms"), only fires when the
/// transforming object matches.
///
/// Note: We intentionally do NOT match `EffectResolved { kind: Transform }` because its
/// `source_id` is the ability source, not the transforming object — they differ for
/// external transforms (e.g., card A transforms card B).
pub(super) fn match_transformed(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::Transformed { object_id } = event {
        valid_source_matches(trigger, state, *object_id, source_id)
    } else {
        false
    }
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

/// CR 702.110a: "When this creature exploits" = source is the exploiter.
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

/// CR 701.37a: "When ~ becomes monstrous" — self-trigger only.
/// Fires when EffectResolved::Monstrosity is emitted for this source.
pub(super) fn match_become_monstrous(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Monstrosity,
            source_id: sid,
        } if *sid == source_id
    )
}

/// CR 708 + CR 701.40b + CR 701.58b: TurnFaceUp fires when a face-down
/// permanent is turned face up. Uses `GameEvent::TurnedFaceUp` emitted by
/// `crate::game::morph::turn_face_up`.
///
/// Filters:
/// - `valid_card` gates the turned-up object (e.g. "a creature", "a permanent").
/// - `valid_target` gates the controller of the turned-up object
///   (e.g. `ControllerRef::You` for "whenever you turn a permanent face up").
pub(super) fn match_turn_face_up(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::TurnedFaceUp { object_id } = event else {
        return false;
    };
    // CR 603.2a: Filter on the face-up object when a subject filter is present
    // (e.g. "a creature"). No filter → any face-up permanent matches.
    if trigger.valid_card.is_some() && !valid_card_matches(trigger, state, *object_id, source_id) {
        return false;
    }
    // CR 603.2a: Filter on controller of the face-up object for actor-side
    // forms ("whenever you turn a permanent face up").
    if let Some(ref vt) = trigger.valid_target {
        let Some(flipped_controller) = state.objects.get(object_id).map(|o| o.controller) else {
            return false;
        };
        return player_matches_filter(vt, state, flipped_controller, source_id);
    }
    true
}

/// CR 701.62 + CR 701.62b: ManifestDread fires after a player finishes resolving
/// the "manifest dread" keyword action. Uses `GameEvent::EffectResolved`
/// emitted by `crate::game::effects::manifest_dread`.
///
/// `valid_target` gates the controller performing the action (e.g.
/// `ControllerRef::You` for "whenever you manifest dread").
pub(super) fn match_manifest_dread(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::EffectResolved {
        kind: EffectKind::ManifestDread,
        source_id: triggering_source,
    } = event
    else {
        return false;
    };
    let Some(actor) = state.objects.get(triggering_source).map(|o| o.controller) else {
        return false;
    };
    if let Some(ref vt) = trigger.valid_target {
        return player_matches_filter(vt, state, actor, source_id);
    }
    true
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
        object_id, from, ..
    } = event
    {
        if *from != Some(Zone::Battlefield) {
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
        if matches!(trigger.damage_kind, DamageKindFilter::CombatOnly) && !is_combat {
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

/// CR 120.10: ExcessDamage — fires when the trigger source deals excess damage to a permanent.
pub(super) fn match_excess_damage(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::DamageDealt { source_id: src, excess, .. }
        if *excess > 0 && *src == source_id)
}

/// CR 120.10: ExcessDamageAll — fires when any source deals excess damage to a permanent.
pub(super) fn match_excess_damage_all(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::DamageDealt { excess, .. } if *excess > 0)
}

/// YouAttack: fires once when a player declares attackers matching the trigger's
/// player-scope filter.
///
/// CR 508.1m + CR 603.2c: If `trigger.valid_target` is set, the matcher resolves
/// the attacking player (the common controller of the attackers — CR 506.2 / CR
/// 508.1) and checks it against the filter (e.g. `ControllerRef::Opponent` for
/// "another player attacks"). With no filter, the legacy "you attack" semantics
/// apply: fire when any attacker is controlled by the trigger's source controller.
pub(super) fn match_you_attack(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::AttackersDeclared { attacker_ids, .. } = event else {
        return false;
    };
    if attacker_ids.is_empty() {
        return false;
    }
    // CR 506.2: the active player is the attacking player; all attackers in
    // a single AttackersDeclared batch share one controller.
    let Some(attacking_player) = attacker_ids
        .iter()
        .find_map(|id| state.objects.get(id).map(|o| o.controller))
    else {
        return false;
    };
    if trigger.valid_target.is_some() {
        valid_player_matches(trigger, state, attacking_player, source_id)
    } else {
        let source_controller = state.objects.get(&source_id).map(|o| o.controller);
        Some(attacking_player) == source_controller
    }
}

/// CR 725.1: Matches when a player becomes the monarch.
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

/// CR 701.54d: Match "the Ring tempts you" events.
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

/// CR 309.7: Match dungeon completion events.
pub(super) fn match_dungeon_completed(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::DungeonCompleted { player_id, .. } = event {
        valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

/// CR 309.4c: Match room entry events.
pub(super) fn match_room_entered(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::RoomEntered { player_id, .. } = event {
        valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

/// CR 725.2: Match "takes the initiative" events.
pub(super) fn match_takes_initiative(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::InitiativeTaken { player_id } = event {
        valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

/// CR 702.49a: Matches when a player activates a ninjutsu-family ability.
/// The trigger fires for the controller of the trigger source when they activate
/// any ninjutsu variant (ninjutsu, commander ninjutsu, sneak).
pub(super) fn match_ninjutsu_activated(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::NinjutsuActivated { player_id, .. } = event {
        // Fire when the ninjutsu was activated by the trigger source's controller
        state
            .objects
            .get(&source_id)
            .map(|obj| obj.controller == *player_id)
            .unwrap_or(false)
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
// CR 702.122d: Crew trigger matchers
// ---------------------------------------------------------------------------

/// CR 702.122d: Matches when a Vehicle's crew ability resolves.
/// Both `Crewed` and `BecomesCrewed` are semantically identical — different Oracle text
/// phrasings for the same trigger condition.
pub(super) fn match_vehicle_crewed(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::VehicleCrewed { vehicle_id, .. } if *vehicle_id == source_id)
}

/// CR 702.184a: Matches when a Spacecraft's station ability resolves.
/// Fires for "Whenever ~ is stationed" on the specific Spacecraft only —
/// other Spacecraft being stationed never triggers this.
pub(super) fn match_stationed(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::Stationed { spacecraft_id, .. } if *spacecraft_id == source_id)
}

/// CR 702.171a + CR 702.171b: Matches when a Mount's saddle ability resolves.
/// Both `Saddled` and `BecomesSaddled` are semantically identical — different
/// Oracle phrasings for the same trigger condition, consistent with how
/// `Crewed` / `BecomesCrewed` share `match_vehicle_crewed`.
pub(super) fn match_saddled(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::Saddled { mount_id, .. } if *mount_id == source_id)
}

/// CR 702.122: Actor-side crew trigger — fires when any creature in the crew
/// ability's tapped-cost list matches the trigger's `valid_card` filter.
/// For self-only triggers (Gearshift Ace: "Whenever ~ crews a Vehicle"), the
/// filter is `SelfRef` and reduces to a source_id membership check. For
/// compound-subject triggers (Tiana: "Tiana or another legendary creature
/// you control crews a Vehicle"), the filter's Or-branches are evaluated
/// against each creature via `matches_target_filter`.
pub(super) fn match_crews(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::VehicleCrewed { creatures, .. } = event else {
        return false;
    };
    match_actor_against_filter(creatures, trigger, source_id, state)
}

/// CR 702.171c: Actor-side saddle trigger — analogous to `match_crews`.
pub(super) fn match_saddles(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::Saddled { creatures, .. } = event else {
        return false;
    };
    match_actor_against_filter(creatures, trigger, source_id, state)
}

/// CR 702.122 + CR 702.171c: Compound actor-side trigger — fires on either
/// saddling a Mount OR crewing a Vehicle.
pub(super) fn match_saddles_or_crews(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match_saddles(event, trigger, source_id, state) || match_crews(event, trigger, source_id, state)
}

/// Shared helper: checks whether any object_id in `actors` matches the trigger's
/// `valid_card` filter. Falls back to `source_id` membership if `valid_card` is
/// `None` (pre-filter trigger definitions, e.g., Forge-format ingest).
fn match_actor_against_filter(
    actors: &[ObjectId],
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match &trigger.valid_card {
        None => actors.contains(&source_id),
        Some(filter) => {
            let ctx = super::filter::FilterContext::from_source(state, source_id);
            actors
                .iter()
                .any(|&cid| super::filter::matches_target_filter(state, cid, filter, &ctx))
        }
    }
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

/// CR 700.14: Expend N — fires when cumulative mana spent on spells this turn
/// crosses the threshold for the first time.
/// prev < threshold <= new_cumulative means we just crossed it.
/// The crossing math guarantees at-most-once-per-turn without needing OncePerTurn.
pub(super) fn match_mana_expend(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::ManaExpended {
        player_id,
        amount_spent,
        new_cumulative,
    } = event
    {
        let threshold = trigger.expend_threshold.unwrap_or(0);
        let prev = new_cumulative.saturating_sub(*amount_spent);
        // CR 700.14: Fires when crossing the threshold
        if prev >= threshold || *new_cumulative < threshold {
            return false;
        }
        // Check that this player is the trigger's controller
        valid_player_is_controller(state, *player_id, source_id)
    } else {
        false
    }
}

/// Check that a player is the controller of the trigger source.
fn valid_player_is_controller(state: &GameState, player_id: PlayerId, source_id: ObjectId) -> bool {
    state
        .objects
        .get(&source_id)
        .map(|o| o.controller == player_id)
        .unwrap_or(false)
}

/// CR 115.9c: Check that a stack entry's targets ALL match the given filter.
/// A spell with no targets does not satisfy "targets only X" (it doesn't target at all).
fn stack_entry_targets_only(
    state: &GameState,
    stack_object_id: ObjectId,
    constraint: &TargetFilter,
    source_id: ObjectId,
) -> bool {
    let entry = state.stack.iter().find(|e| e.id == stack_object_id);
    let Some(entry) = entry else {
        return false;
    };
    let Some(ability) = entry.ability() else {
        return false;
    };
    // A spell with no targets doesn't "target only X" — it doesn't target at all.
    if ability.targets.is_empty() {
        return false;
    }
    let source_controller = state.objects.get(&source_id).map(|o| o.controller);
    let ctx = super::filter::FilterContext::from_source(state, source_id);
    ability.targets.iter().all(|t| match t {
        TargetRef::Object(id) => super::filter::matches_target_filter(state, *id, constraint, &ctx),
        TargetRef::Player(pid) => {
            super::filter::player_matches_target_filter(constraint, *pid, source_controller)
        }
    })
}

/// CR 115.9b: Check that a stack entry has at least one target matching the filter.
/// A spell with no targets does not satisfy "that targets X" (it doesn't target at all).
fn stack_entry_targets_any(
    state: &GameState,
    stack_object_id: ObjectId,
    constraint: &TargetFilter,
    source_id: ObjectId,
) -> bool {
    let entry = state.stack.iter().find(|e| e.id == stack_object_id);
    let Some(entry) = entry else {
        return false;
    };
    let Some(ability) = entry.ability() else {
        return false;
    };
    if ability.targets.is_empty() {
        return false;
    }
    let source_controller = state.objects.get(&source_id).map(|o| o.controller);
    let ctx = super::filter::FilterContext::from_source(state, source_id);
    ability.targets.iter().any(|t| match t {
        TargetRef::Object(id) => super::filter::matches_target_filter(state, *id, constraint, &ctx),
        TargetRef::Player(pid) => {
            super::filter::player_matches_target_filter(constraint, *pid, source_controller)
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::parser::oracle_trigger::parse_trigger_line;
    use crate::types::ability::{
        ControllerRef, QuantityExpr, ResolvedAbility, TargetFilter, TriggerDefinition, TypeFilter,
        TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::events::{GameEvent, PlayerActionKind};
    use crate::types::game_state::{
        CastingVariant, GameState, StackEntry, StackEntryKind, ZoneChangeRecord,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    #[test]
    fn trigger_matcher_covers_registry_entries() {
        let registry = build_trigger_registry();
        for mode in registry.keys() {
            assert!(
                trigger_matcher(mode.clone()).is_some(),
                "missing direct matcher for {mode:?}"
            );
        }
    }

    /// Helper to create a minimal TriggerDefinition with typed fields.
    fn make_trigger(mode: TriggerMode) -> TriggerDefinition {
        TriggerDefinition::new(mode)
    }

    #[test]
    fn attacks_trigger_filters_defender_and_splits_matching_attackers() {
        let mut state = setup();
        let decree = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Marchesa's Decree".to_string(),
            Zone::Battlefield,
        );
        let attacker_to_player = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Attacker A".to_string(),
            Zone::Battlefield,
        );
        let attacker_to_planeswalker = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Attacker B".to_string(),
            Zone::Battlefield,
        );
        let own_attacker_elsewhere = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Own Attacker".to_string(),
            Zone::Battlefield,
        );
        let planeswalker = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Planeswalker".to_string(),
            Zone::Battlefield,
        );
        for id in [
            attacker_to_player,
            attacker_to_planeswalker,
            own_attacker_elsewhere,
        ] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let mut trigger = make_trigger(TriggerMode::Attacks);
        trigger.valid_card = Some(TargetFilter::Typed(TypedFilter::creature()));
        trigger.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
        trigger.attack_target_filter =
            Some(crate::types::triggers::AttackTargetFilter::PlayerOrPlaneswalker);

        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![
                attacker_to_player,
                attacker_to_planeswalker,
                own_attacker_elsewhere,
            ],
            defending_player: PlayerId(0),
            attacks: vec![
                (
                    attacker_to_player,
                    crate::game::combat::AttackTarget::Player(PlayerId(0)),
                ),
                (
                    attacker_to_planeswalker,
                    crate::game::combat::AttackTarget::Planeswalker(planeswalker),
                ),
                (
                    own_attacker_elsewhere,
                    crate::game::combat::AttackTarget::Player(PlayerId(1)),
                ),
            ],
        };

        let matched = matching_attack_events(&event, &trigger, decree, &state);
        assert_eq!(matched.len(), 2);
        assert!(matched.iter().all(|event| matches!(
            event,
            GameEvent::AttackersDeclared { attacker_ids, .. } if attacker_ids.len() == 1
        )));
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

    #[test]
    fn changes_zone_etb_matches() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        // Origin: any (None means any), Destination: Battlefield
        trigger.destination = Some(Zone::Battlefield);

        let event = zone_changed_event(
            ObjectId(5),
            Zone::Hand,
            Zone::Battlefield,
            Vec::new(),
            Vec::new(),
        );
        assert!(match_changes_zone(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn searched_library_matches_you_scope() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Search Elemental".to_string(),
            Zone::Battlefield,
        );
        let trigger = parse_trigger_line(
            "Whenever you search your library, scry 1.",
            "Search Elemental",
        );
        let event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(0),
            action: PlayerActionKind::SearchedLibrary,
        };
        assert!(match_player_action(&event, &trigger, source_id, &state));
    }

    #[test]
    fn searched_library_rejects_controller_for_opponent_scope() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Archivist of Oghma".to_string(),
            Zone::Battlefield,
        );
        let trigger = parse_trigger_line(
            "Whenever an opponent searches their library, you gain 1 life and draw a card.",
            "Archivist of Oghma",
        );
        let event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(0),
            action: PlayerActionKind::SearchedLibrary,
        };
        assert!(!match_player_action(&event, &trigger, source_id, &state));
    }

    #[test]
    fn searched_library_matches_opponent_scope() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Wan Shi Tong, Librarian".to_string(),
            Zone::Battlefield,
        );
        let trigger = parse_trigger_line(
            "Whenever an opponent searches their library, put a +1/+1 counter on Wan Shi Tong and draw a card.",
            "Wan Shi Tong, Librarian",
        );
        let event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(1),
            action: PlayerActionKind::SearchedLibrary,
        };
        assert!(match_player_action(&event, &trigger, source_id, &state));
    }

    #[test]
    fn multi_action_trigger_matches_allowed_action() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(13),
            PlayerId(0),
            "River Song".to_string(),
            Zone::Battlefield,
        );
        let trigger = parse_trigger_line(
            "Whenever an opponent scries, surveils, or searches their library, put a +1/+1 counter on River Song. Then River Song deals damage to that player equal to its power.",
            "River Song",
        );
        let event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(1),
            action: PlayerActionKind::Surveil,
        };
        assert!(match_player_action(&event, &trigger, source_id, &state));
    }

    #[test]
    fn multi_action_trigger_rejects_disallowed_action() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(14),
            PlayerId(0),
            "Matoya, Archon Elder".to_string(),
            Zone::Battlefield,
        );
        let trigger = parse_trigger_line(
            "Whenever you scry or surveil, draw a card.",
            "Matoya, Archon Elder",
        );
        let event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(0),
            action: PlayerActionKind::SearchedLibrary,
        };
        assert!(!match_player_action(&event, &trigger, source_id, &state));
    }

    #[test]
    fn changes_zone_dies_matches() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);

        let event = zone_changed_event(
            ObjectId(5),
            Zone::Battlefield,
            Zone::Graveyard,
            Vec::new(),
            Vec::new(),
        );
        assert!(match_changes_zone(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn changes_zone_attached_to_matches_via_record_snapshot() {
        // CR 603.10a + CR 603.6e + CR 702.6: Skullclamp's "whenever equipped
        // creature dies" fires off the dying creature's zone-change record.
        // The record's `attachments` snapshot captures Skullclamp before SBA
        // (CR 704.5n) clears the live `attached_to` pointer. `AttachedTo`
        // matches when the snapshot contains the trigger source.
        use crate::types::ability::AttachmentKind;
        use crate::types::game_state::AttachmentSnapshot;

        let mut state = setup();
        let skullclamp = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Skullclamp".to_string(),
            Zone::Battlefield,
        );
        let creature = ObjectId(99);

        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::AttachedTo);

        // Event: equipped creature dies; snapshot carries Skullclamp as an
        // Equipment attachment that was on the creature at the instant of
        // the zone change.
        let event = GameEvent::ZoneChanged {
            object_id: creature,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                attachments: vec![AttachmentSnapshot {
                    object_id: skullclamp,
                    controller: PlayerId(0),
                    kind: AttachmentKind::Equipment,
                }],
                ..ZoneChangeRecord::test_minimal(creature, Some(Zone::Battlefield), Zone::Graveyard)
            }),
        };

        assert!(match_changes_zone(&event, &trigger, skullclamp, &state));
    }

    #[test]
    fn changes_zone_attached_to_no_match_when_not_attached() {
        // CR 603.10a: An unequipped Skullclamp observing a different creature
        // die must not trigger — the record's attachment snapshot does not
        // contain the Equipment.
        let mut state = setup();
        let skullclamp = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Skullclamp".to_string(),
            Zone::Battlefield,
        );
        let creature = ObjectId(99);

        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::AttachedTo);

        // No attachments on the dying creature — attachments snapshot empty.
        let event = GameEvent::ZoneChanged {
            object_id: creature,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord::test_minimal(
                creature,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            )),
        };

        assert!(!match_changes_zone(&event, &trigger, skullclamp, &state));
    }

    #[test]
    fn changes_zone_attached_to_matches_aura_look_back() {
        // CR 603.6e + CR 603.10a: "Whenever enchanted creature dies" — the
        // Aura's trigger source resolves identically to Equipment, via the
        // attachments snapshot.
        use crate::types::ability::AttachmentKind;
        use crate::types::game_state::AttachmentSnapshot;

        let mut state = setup();
        let aura = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Aura".to_string(),
            Zone::Battlefield,
        );
        let creature = ObjectId(42);

        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::AttachedTo);

        let event = GameEvent::ZoneChanged {
            object_id: creature,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                attachments: vec![AttachmentSnapshot {
                    object_id: aura,
                    controller: PlayerId(0),
                    kind: AttachmentKind::Aura,
                }],
                ..ZoneChangeRecord::test_minimal(creature, Some(Zone::Battlefield), Zone::Graveyard)
            }),
        };

        assert!(match_changes_zone(&event, &trigger, aura, &state));
    }

    #[test]
    fn changes_zone_wrong_destination_no_match() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.destination = Some(Zone::Battlefield);

        let event = zone_changed_event(
            ObjectId(5),
            Zone::Hand,
            Zone::Graveyard,
            Vec::new(),
            Vec::new(),
        );
        assert!(!match_changes_zone(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn changes_zone_origin_zones_matches_library_source() {
        // CR 603.10a: Laelia-style — source can be library OR graveyard.
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZoneAll);
        trigger.origin_zones = vec![Zone::Library, Zone::Graveyard];
        trigger.destination = Some(Zone::Exile);

        let event = zone_changed_event(
            ObjectId(5),
            Zone::Library,
            Zone::Exile,
            Vec::new(),
            Vec::new(),
        );
        assert!(match_changes_zone(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn changes_zone_origin_zones_matches_graveyard_source() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZoneAll);
        trigger.origin_zones = vec![Zone::Library, Zone::Graveyard];
        trigger.destination = Some(Zone::Exile);

        let event = zone_changed_event(
            ObjectId(5),
            Zone::Graveyard,
            Zone::Exile,
            Vec::new(),
            Vec::new(),
        );
        assert!(match_changes_zone(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn changes_zone_origin_zones_rejects_unlisted_source() {
        // Hand → Exile should NOT fire a "put into exile from library/graveyard" trigger.
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZoneAll);
        trigger.origin_zones = vec![Zone::Library, Zone::Graveyard];
        trigger.destination = Some(Zone::Exile);

        let event =
            zone_changed_event(ObjectId(5), Zone::Hand, Zone::Exile, Vec::new(), Vec::new());
        assert!(!match_changes_zone(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn changes_zone_origin_zones_takes_precedence_over_origin() {
        // When origin_zones is non-empty, the single-zone `origin` field is ignored.
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZoneAll);
        trigger.origin = Some(Zone::Battlefield); // would otherwise block this
        trigger.origin_zones = vec![Zone::Library, Zone::Graveyard];
        trigger.destination = Some(Zone::Exile);

        let event = zone_changed_event(
            ObjectId(5),
            Zone::Library,
            Zone::Exile,
            Vec::new(),
            Vec::new(),
        );
        assert!(match_changes_zone(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn changes_zone_uses_event_snapshot_for_subtype_filters() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(15),
            PlayerId(0),
            "Ygra".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::default().with_type(TypeFilter::Subtype("Food".to_string())),
        ));

        let event = zone_changed_event(
            ObjectId(77),
            Zone::Battlefield,
            Zone::Graveyard,
            vec![CoreType::Creature, CoreType::Artifact],
            vec!["Food"],
        );
        assert!(match_changes_zone(&event, &trigger, source_id, &state));
    }

    #[test]
    fn changes_zone_uses_event_snapshot_for_power_filter() {
        // CR 603.10: "Whenever a creature with power 4 or greater dies" must read
        // event-time power from the zone-change snapshot, not from the post-move
        // object (which has left the battlefield and no longer has a power).
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Big Death Trigger".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![crate::types::ability::FilterProp::PowerGE {
                value: crate::types::ability::QuantityExpr::Fixed { value: 4 },
            }],
        )));

        let base_event = zone_changed_event(
            ObjectId(500),
            Zone::Battlefield,
            Zone::Graveyard,
            vec![CoreType::Creature],
            Vec::new(),
        );
        // A 5/5 dying should fire the trigger.
        let event_5 = match base_event {
            GameEvent::ZoneChanged {
                object_id,
                from,
                to,
                record,
            } => GameEvent::ZoneChanged {
                object_id,
                from,
                to,
                record: Box::new(ZoneChangeRecord {
                    power: Some(5),
                    toughness: Some(5),
                    ..*record
                }),
            },
            _ => unreachable!(),
        };
        assert!(match_changes_zone(&event_5, &trigger, source_id, &state));

        // A 2/2 dying should not fire.
        let event_2 = GameEvent::ZoneChanged {
            object_id: ObjectId(501),
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                power: Some(2),
                toughness: Some(2),
                ..ZoneChangeRecord::test_minimal(
                    ObjectId(501),
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            }),
        };
        assert!(!match_changes_zone(&event_2, &trigger, source_id, &state));
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
            excess: 0,
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
            attackers: vec![crate::game::combat::AttackerInfo::attacking_player(
                attacker,
                PlayerId(1),
            )],
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
        let event = zone_changed_event(
            ObjectId(5),
            Zone::Battlefield,
            Zone::Exile,
            Vec::new(),
            Vec::new(),
        );
        let trigger = make_trigger(TriggerMode::Exiled);
        assert!(match_exiled(&event, &trigger, ObjectId(5), &state));
    }

    #[test]
    fn exiled_does_not_match_other_zones() {
        let state = setup();
        let event = zone_changed_event(
            ObjectId(5),
            Zone::Battlefield,
            Zone::Graveyard,
            Vec::new(),
            Vec::new(),
        );
        let trigger = make_trigger(TriggerMode::Exiled);
        assert!(!match_exiled(&event, &trigger, ObjectId(5), &state));
    }

    #[test]
    fn milled_matches_library_to_graveyard() {
        let state = setup();
        let event = zone_changed_event(
            ObjectId(5),
            Zone::Library,
            Zone::Graveyard,
            Vec::new(),
            Vec::new(),
        );
        let trigger = make_trigger(TriggerMode::Milled);
        assert!(match_milled(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn milled_does_not_match_hand_to_graveyard() {
        let state = setup();
        let event = zone_changed_event(
            ObjectId(5),
            Zone::Hand,
            Zone::Graveyard,
            Vec::new(),
            Vec::new(),
        );
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
            tapped_for_mana: true,
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
        state.objects.get_mut(&aura).unwrap().attached_to = Some(enchanted_land.into());

        let event = GameEvent::ManaAdded {
            player_id: PlayerId(0),
            mana_type: crate::types::mana::ManaType::Green,
            source_id: enchanted_land,
            tapped_for_mana: true,
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
            tapped_for_mana: true,
        };

        let mut trigger = make_trigger(TriggerMode::TapsForMana);
        trigger.valid_target = Some(TargetFilter::Controller);
        trigger.valid_card = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)));
        assert!(!match_taps_for_mana(&event, &trigger, source, &state));
    }

    #[test]
    fn taps_for_mana_ignores_non_mana_ability_production() {
        let state = setup();
        let source = ObjectId(5);
        // Mana produced by a triggered ability effect, not a mana ability activation
        let event = GameEvent::ManaAdded {
            player_id: PlayerId(0),
            mana_type: crate::types::mana::ManaType::Green,
            source_id: source,
            tapped_for_mana: false,
        };
        let trigger = make_trigger(TriggerMode::TapsForMana);
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
            nth_in_step: 1,
        };
        assert!(match_drawn(&opponent_event, &trigger, source, &state));

        let controller_event = GameEvent::CardDrawn {
            player_id: PlayerId(0),
            object_id: ObjectId(21),
            nth_in_step: 1,
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
            .insert(crate::types::counter::CounterType::Lore, 1);

        let event = GameEvent::CounterAdded {
            object_id: saga_id,
            counter_type: crate::types::counter::CounterType::Lore,
            count: 1,
        };

        // Trigger for chapter 1 (threshold=1) should fire: 0 < 1 <= 1
        let trigger_ch1 = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::types::counter::CounterType::Lore,
                threshold: Some(1),
            });
        assert!(match_counter_added(&event, &trigger_ch1, saga_id, &state));

        // Trigger for chapter 2 (threshold=2) should NOT fire: 0 < 2, but 2 > 1
        let trigger_ch2 = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::types::counter::CounterType::Lore,
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
            .insert(crate::types::counter::CounterType::Lore, 2);

        let event = GameEvent::CounterAdded {
            object_id: saga_id,
            counter_type: crate::types::counter::CounterType::Lore,
            count: 2, // Added 2 at once
        };

        // Both chapter 1 (threshold=1) and chapter 2 (threshold=2) should fire
        // because previous=0, current=2, so 0 < 1 <= 2 and 0 < 2 <= 2
        let trigger_ch1 = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::types::counter::CounterType::Lore,
                threshold: Some(1),
            });
        assert!(match_counter_added(&event, &trigger_ch1, saga_id, &state));

        let trigger_ch2 = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::types::counter::CounterType::Lore,
                threshold: Some(2),
            });
        assert!(match_counter_added(&event, &trigger_ch2, saga_id, &state));

        // Chapter 3 should NOT fire: 0 < 3 but 3 > 2
        let trigger_ch3 = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::types::counter::CounterType::Lore,
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
            .insert(crate::types::counter::CounterType::Plus1Plus1, 1);

        // +1/+1 counter added, but trigger filters for lore
        let event = GameEvent::CounterAdded {
            object_id: saga_id,
            counter_type: crate::types::counter::CounterType::Plus1Plus1,
            count: 1,
        };

        let trigger = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::types::counter::CounterType::Lore,
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
            .insert(crate::types::counter::CounterType::Lore, 1);

        let event = GameEvent::CounterAdded {
            object_id: saga_id,
            counter_type: crate::types::counter::CounterType::Lore,
            count: 1,
        };

        // Filter with no threshold fires on any addition of the matching type
        let trigger = TriggerDefinition::new(TriggerMode::CounterAdded)
            .valid_card(TargetFilter::SelfRef)
            .counter_filter(CounterTriggerFilter {
                counter_type: crate::types::counter::CounterType::Lore,
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
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(100),
                ability: Some(ResolvedAbility::new(
                    crate::types::ability::Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: crate::types::ability::TargetFilter::Controller,
                        up_to: false,
                    },
                    vec![],
                    spell_id,
                    PlayerId(0),
                )),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        (state, spell_id)
    }

    fn setup_with_ability_on_stack() -> (GameState, ObjectId) {
        let mut state = setup();
        let ability_id = ObjectId(60);
        state.stack.push_back(StackEntry {
            id: ability_id,
            source_id: ObjectId(10),
            controller: PlayerId(1),
            kind: StackEntryKind::ActivatedAbility {
                source_id: ObjectId(10),
                ability: ResolvedAbility::new(
                    crate::types::ability::Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: crate::types::ability::TargetFilter::Controller,
                        up_to: false,
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

    // ── Work Item 3: DamageKindFilter ─────────────────────────────

    #[test]
    fn damage_kind_any_passes_both() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::DamageDone);

        for is_combat in [true, false] {
            let event = GameEvent::DamageDealt {
                source_id: ObjectId(1),
                target: TargetRef::Player(PlayerId(0)),
                amount: 3,
                is_combat,
                excess: 0,
            };
            assert!(match_damage_done(&event, &trigger, ObjectId(1), &state));
        }
    }

    #[test]
    fn damage_kind_combat_only_rejects_noncombat() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::DamageDone);
        trigger.damage_kind = DamageKindFilter::CombatOnly;

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            excess: 0,
        };
        assert!(!match_damage_done(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn damage_kind_noncombat_only_rejects_combat() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::DamageDone);
        trigger.damage_kind = DamageKindFilter::NoncombatOnly;

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: true,
            excess: 0,
        };
        assert!(!match_damage_done(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn damage_kind_noncombat_only_accepts_noncombat() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::DamageDone);
        trigger.damage_kind = DamageKindFilter::NoncombatOnly;

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            excess: 0,
        };
        assert!(match_damage_done(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn damage_done_valid_target_opponent_rejects_self() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            String::new(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::DamageDone);
        trigger.damage_kind = DamageKindFilter::NoncombatOnly;
        trigger.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));

        // Damage to controller (self) — should NOT match
        let event = GameEvent::DamageDealt {
            source_id,
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            excess: 0,
        };
        assert!(!match_damage_done(&event, &trigger, source_id, &state));

        // Damage to opponent — should match
        let event_opp = GameEvent::DamageDealt {
            source_id,
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            excess: 0,
        };
        assert!(match_damage_done(&event_opp, &trigger, source_id, &state));
    }

    // ── Work Item 4: Transforms Into Self ─────────────────────────

    #[test]
    fn transformed_self_ref_matches_own_transform() {
        let mut state = setup();
        // Create the object so SelfRef filter can look it up in state.objects
        create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Werewolf".to_string(),
            Zone::Battlefield,
        );
        let obj_id = state.objects.keys().next().copied().unwrap();

        let mut trigger = make_trigger(TriggerMode::Transformed);
        trigger.valid_source = Some(TargetFilter::SelfRef);

        let event = GameEvent::Transformed { object_id: obj_id };
        // Source is the trigger's own permanent — matches when source_id equals object_id
        assert!(match_transformed(&event, &trigger, obj_id, &state));
        // Different object — does not match
        assert!(!match_transformed(&event, &trigger, ObjectId(99), &state));
    }

    // ── Work Item 5: Tap Opponent's Creature ─────────────────────

    #[test]
    fn tap_opponent_creature_via_effect_fires() {
        let mut state = setup();
        // Trigger source on P0's battlefield
        let trigger_src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hylda".to_string(),
            Zone::Battlefield,
        );
        // Opponent's creature
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        // Your source (the thing that tapped the creature)
        let your_source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Frost Breath".to_string(),
            Zone::Battlefield,
        );
        // Add creature type to opponent's object
        if let Some(obj) = state.objects.get_mut(&opp_creature) {
            obj.card_types.core_types.push(CoreType::Creature);
        }

        let mut trigger = make_trigger(TriggerMode::Taps);
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::Opponent),
        ));

        // Tapped by your effect — should fire
        let event = GameEvent::PermanentTapped {
            object_id: opp_creature,
            caused_by: Some(your_source),
        };
        assert!(match_taps(&event, &trigger, trigger_src, &state));
    }

    #[test]
    fn tap_opponent_creature_self_initiated_does_not_fire() {
        let mut state = setup();
        let trigger_src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hylda".to_string(),
            Zone::Battlefield,
        );
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        if let Some(obj) = state.objects.get_mut(&opp_creature) {
            obj.card_types.core_types.push(CoreType::Creature);
        }

        let mut trigger = make_trigger(TriggerMode::Taps);
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::Opponent),
        ));

        // Self-initiated tap (e.g. mana ability) — should NOT fire
        let event = GameEvent::PermanentTapped {
            object_id: opp_creature,
            caused_by: None,
        };
        assert!(!match_taps(&event, &trigger, trigger_src, &state));
    }

    #[test]
    fn tap_own_creature_does_not_fire_opponent_trigger() {
        let mut state = setup();
        let trigger_src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hylda".to_string(),
            Zone::Battlefield,
        );
        let own_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "My Bear".to_string(),
            Zone::Battlefield,
        );
        if let Some(obj) = state.objects.get_mut(&own_creature) {
            obj.card_types.core_types.push(CoreType::Creature);
        }

        let mut trigger = make_trigger(TriggerMode::Taps);
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::Opponent),
        ));

        // Tapping your own creature — doesn't match opponent filter
        let event = GameEvent::PermanentTapped {
            object_id: own_creature,
            caused_by: Some(trigger_src),
        };
        assert!(!match_taps(&event, &trigger, trigger_src, &state));
    }

    #[test]
    fn tap_no_opponent_filter_ignores_caused_by() {
        // "Whenever a creature becomes tapped" (no opponent filter) should
        // fire regardless of who caused the tap.
        let mut state = setup();
        let trigger_src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Trigger Source".to_string(),
            Zone::Battlefield,
        );
        let any_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        if let Some(obj) = state.objects.get_mut(&any_creature) {
            obj.card_types.core_types.push(CoreType::Creature);
        }

        let mut trigger = make_trigger(TriggerMode::Taps);
        // Creature filter WITHOUT opponent controller restriction
        trigger.valid_card = Some(TargetFilter::Typed(TypedFilter::creature()));

        // Opponent taps their own creature (self-initiated) — should still fire
        let event = GameEvent::PermanentTapped {
            object_id: any_creature,
            caused_by: None,
        };
        assert!(match_taps(&event, &trigger, trigger_src, &state));

        // Opponent's creature tapped by opponent's source — should fire
        let opp_source = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opp Source".to_string(),
            Zone::Battlefield,
        );
        let event2 = GameEvent::PermanentTapped {
            object_id: any_creature,
            caused_by: Some(opp_source),
        };
        assert!(match_taps(&event2, &trigger, trigger_src, &state));
    }

    // ── Work Item 6: Expend ───────────────────────────────────────

    #[test]
    fn expend_threshold_crossing() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            String::new(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::ManaExpend);
        trigger.expend_threshold = Some(4);

        // Spend 2, cumulative=2 → below threshold → no fire
        let event1 = GameEvent::ManaExpended {
            player_id: PlayerId(0),
            amount_spent: 2,
            new_cumulative: 2,
        };
        assert!(!match_mana_expend(&event1, &trigger, source_id, &state));

        // Spend 3 more, cumulative=5 → crossed 4 → fire
        let event2 = GameEvent::ManaExpended {
            player_id: PlayerId(0),
            amount_spent: 3,
            new_cumulative: 5,
        };
        assert!(match_mana_expend(&event2, &trigger, source_id, &state));
    }

    #[test]
    fn expend_threshold_exact_crossing() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            String::new(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::ManaExpend);
        trigger.expend_threshold = Some(4);

        // Spend 5 at once, cumulative=5 → crossed 4 from 0 → fire
        let event = GameEvent::ManaExpended {
            player_id: PlayerId(0),
            amount_spent: 5,
            new_cumulative: 5,
        };
        assert!(match_mana_expend(&event, &trigger, source_id, &state));
    }

    #[test]
    fn expend_already_crossed_no_refire() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            String::new(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::ManaExpend);
        trigger.expend_threshold = Some(4);

        // Already at cumulative 5, spend 2 more → 7. Did NOT cross 4 this time.
        let event = GameEvent::ManaExpended {
            player_id: PlayerId(0),
            amount_spent: 2,
            new_cumulative: 7,
        };
        assert!(!match_mana_expend(&event, &trigger, source_id, &state));
    }

    #[test]
    fn expend_wrong_player_no_fire() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            String::new(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::ManaExpend);
        trigger.expend_threshold = Some(4);

        // Opponent spends mana — should not fire for our trigger
        let event = GameEvent::ManaExpended {
            player_id: PlayerId(1),
            amount_spent: 5,
            new_cumulative: 5,
        };
        assert!(!match_mana_expend(&event, &trigger, source_id, &state));
    }

    #[test]
    fn expend_multiple_thresholds() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            String::new(),
            Zone::Battlefield,
        );

        // Expend 4 trigger
        let mut trigger4 = make_trigger(TriggerMode::ManaExpend);
        trigger4.expend_threshold = Some(4);

        // Expend 8 trigger
        let mut trigger8 = make_trigger(TriggerMode::ManaExpend);
        trigger8.expend_threshold = Some(8);

        // Spend 5, cumulative=5 → crosses 4, not 8
        let event1 = GameEvent::ManaExpended {
            player_id: PlayerId(0),
            amount_spent: 5,
            new_cumulative: 5,
        };
        assert!(match_mana_expend(&event1, &trigger4, source_id, &state));
        assert!(!match_mana_expend(&event1, &trigger8, source_id, &state));

        // Spend 4 more, cumulative=9 → crosses 8
        let event2 = GameEvent::ManaExpended {
            player_id: PlayerId(0),
            amount_spent: 4,
            new_cumulative: 9,
        };
        assert!(!match_mana_expend(&event2, &trigger4, source_id, &state));
        assert!(match_mana_expend(&event2, &trigger8, source_id, &state));
    }

    // --- CR 115.9c: TargetsOnly helper tests ---

    #[test]
    fn extract_targets_only_from_typed_filter() {
        let filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant).properties(vec![
            FilterProp::TargetsOnly {
                filter: Box::new(TargetFilter::SelfRef),
            },
        ]));
        let result = crate::game::filter::extract_targets_only(&filter);
        assert_eq!(result, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn extract_targets_only_from_or_filter() {
        let filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant).properties(vec![
                    FilterProp::TargetsOnly {
                        filter: Box::new(TargetFilter::SelfRef),
                    },
                ])),
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery).properties(vec![
                    FilterProp::TargetsOnly {
                        filter: Box::new(TargetFilter::SelfRef),
                    },
                ])),
            ],
        };
        let result = crate::game::filter::extract_targets_only(&filter);
        assert_eq!(result, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn extract_targets_only_returns_none_when_absent() {
        let filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature));
        let result = crate::game::filter::extract_targets_only(&filter);
        assert_eq!(result, None);
    }

    #[test]
    fn player_matches_target_filter_you() {
        let filter = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You));
        assert!(crate::game::filter::player_matches_target_filter(
            &filter,
            PlayerId(0),
            Some(PlayerId(0))
        ));
        assert!(!crate::game::filter::player_matches_target_filter(
            &filter,
            PlayerId(1),
            Some(PlayerId(0))
        ));
    }

    #[test]
    fn player_matches_target_filter_self_ref_is_false() {
        // SelfRef refers to objects, not players
        assert!(!crate::game::filter::player_matches_target_filter(
            &TargetFilter::SelfRef,
            PlayerId(0),
            Some(PlayerId(0))
        ));
    }

    // ── ExcessDamage trigger matchers ─────────────────────────────

    #[test]
    fn excess_damage_matches_own_source() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::ExcessDamage);

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Object(ObjectId(2)),
            amount: 5,
            is_combat: false,
            excess: 3,
        };
        assert!(match_excess_damage(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn excess_damage_rejects_different_source() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::ExcessDamage);

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(2),
            target: TargetRef::Object(ObjectId(3)),
            amount: 5,
            is_combat: false,
            excess: 3,
        };
        assert!(!match_excess_damage(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn excess_damage_rejects_zero_excess() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::ExcessDamage);

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Object(ObjectId(2)),
            amount: 2,
            is_combat: false,
            excess: 0,
        };
        assert!(!match_excess_damage(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn excess_damage_all_matches_any_source() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::ExcessDamageAll);

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(99),
            target: TargetRef::Object(ObjectId(2)),
            amount: 5,
            is_combat: true,
            excess: 1,
        };
        assert!(match_excess_damage_all(
            &event,
            &trigger,
            ObjectId(1),
            &state
        ));
    }

    #[test]
    fn excess_damage_all_rejects_zero_excess() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::ExcessDamageAll);

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(99),
            target: TargetRef::Object(ObjectId(2)),
            amount: 2,
            is_combat: false,
            excess: 0,
        };
        assert!(!match_excess_damage_all(
            &event,
            &trigger,
            ObjectId(1),
            &state
        ));
    }

    // ---------------------------------------------------------------------------
    // CR 702.184a: Station trigger matcher tests
    // ---------------------------------------------------------------------------

    #[test]
    fn stationed_matches_when_spacecraft_id_matches() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::Stationed);
        let event = GameEvent::Stationed {
            spacecraft_id: ObjectId(42),
            creature_id: ObjectId(7),
            counters_added: 3,
        };
        assert!(match_stationed(&event, &trigger, ObjectId(42), &state));
    }

    #[test]
    fn stationed_rejects_when_spacecraft_id_differs() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::Stationed);
        let event = GameEvent::Stationed {
            spacecraft_id: ObjectId(99),
            creature_id: ObjectId(7),
            counters_added: 3,
        };
        // The trigger is bound to ObjectId(42), but the event is about ObjectId(99) —
        // it must NOT fire (no cross-Spacecraft triggering).
        assert!(!match_stationed(&event, &trigger, ObjectId(42), &state));
    }

    #[test]
    fn stationed_rejects_non_station_event() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::Stationed);
        let event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(42),
            creatures: vec![ObjectId(7)],
        };
        // Crew events don't trigger station listeners.
        assert!(!match_stationed(&event, &trigger, ObjectId(42), &state));
    }

    // ---------------------------------------------------------------------------
    // CR 702.122 + CR 702.171c: Actor-side Saddle/Crew matcher tests.
    // These guard the compound-subject generalization: the matcher consults
    // `trigger.valid_card` against event.creatures via `matches_target_filter`,
    // so compound subjects (e.g. Tiana) fire on the non-self branch.
    // ---------------------------------------------------------------------------

    /// Insert a creature at a specific object id with an explicit controller and
    /// (optionally) the Legendary supertype. Helper for actor-filter tests.
    fn add_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        legendary: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            crate::types::identifiers::CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        if legendary {
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Legendary);
        }
        id
    }

    #[test]
    fn match_crews_fires_on_self_actor() {
        // Gearshift Ace shape: "Whenever ~ crews a Vehicle". valid_card = SelfRef.
        let mut state = setup();
        let ace = add_creature(&mut state, PlayerId(0), "Gearshift Ace", false);
        let mut trigger = make_trigger(TriggerMode::Crews);
        trigger.valid_card = Some(TargetFilter::SelfRef);

        let event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(999),
            creatures: vec![ace],
        };
        assert!(match_crews(&event, &trigger, ace, &state));
    }

    #[test]
    fn match_crews_fires_on_compound_non_self_branch() {
        // C5 CRITICAL regression guard. Tiana shape: compound subject
        // Or { SelfRef, Typed(Creature, Legendary, Controller::You, [Another]) }.
        // When a DIFFERENT legendary creature the controller owns crews the Vehicle,
        // the trigger MUST still fire via the Typed branch — source_id membership
        // alone is not enough.
        let mut state = setup();
        let tiana = add_creature(&mut state, PlayerId(0), "Tiana, Angelic Mechanic", true);
        let other_legendary = add_creature(&mut state, PlayerId(0), "Other Legendary", true);

        let mut trigger = make_trigger(TriggerMode::Crews);
        trigger.valid_card = Some(TargetFilter::Or {
            filters: vec![
                TargetFilter::SelfRef,
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![
                            FilterProp::HasSupertype {
                                value: crate::types::card_type::Supertype::Legendary,
                            },
                            FilterProp::Another,
                        ]),
                ),
            ],
        });

        let event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(999),
            creatures: vec![other_legendary],
        };
        // source_id = tiana (trigger owner); actor = other_legendary (not source).
        // Must fire via the Typed Legendary branch.
        assert!(match_crews(&event, &trigger, tiana, &state));
    }

    #[test]
    fn match_crews_does_not_fire_when_actor_does_not_match_filter() {
        // Negative: compound-subject filter requires Legendary + You-controlled.
        // A non-legendary creature (even if controlled by You) must NOT match.
        let mut state = setup();
        let tiana = add_creature(&mut state, PlayerId(0), "Tiana, Angelic Mechanic", true);
        let bear = add_creature(&mut state, PlayerId(0), "Grizzly Bears", false);

        let mut trigger = make_trigger(TriggerMode::Crews);
        trigger.valid_card = Some(TargetFilter::Or {
            filters: vec![
                TargetFilter::SelfRef,
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![
                            FilterProp::HasSupertype {
                                value: crate::types::card_type::Supertype::Legendary,
                            },
                            FilterProp::Another,
                        ]),
                ),
            ],
        });

        let event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(999),
            creatures: vec![bear],
        };
        assert!(!match_crews(&event, &trigger, tiana, &state));
    }

    #[test]
    fn match_saddles_or_crews_fires_on_either_event_type() {
        // Canyon Vaulter shape: the compound matcher must fire on both Saddled and
        // VehicleCrewed events.
        let mut state = setup();
        let vaulter = add_creature(&mut state, PlayerId(0), "Canyon Vaulter", false);
        let mut trigger = make_trigger(TriggerMode::SaddlesOrCrews);
        trigger.valid_card = Some(TargetFilter::SelfRef);

        let crew_event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(999),
            creatures: vec![vaulter],
        };
        let saddle_event = GameEvent::Saddled {
            mount_id: ObjectId(998),
            creatures: vec![vaulter],
        };
        assert!(match_saddles_or_crews(
            &crew_event,
            &trigger,
            vaulter,
            &state
        ));
        assert!(match_saddles_or_crews(
            &saddle_event,
            &trigger,
            vaulter,
            &state
        ));
    }

    /// Stamp the given object with `CoreType::Creature` so that
    /// `TypeFilter::Permanent` / `TypeFilter::Creature` match against it.
    fn make_creature(state: &mut GameState, id: ObjectId) {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.card_types.core_types.push(CoreType::Creature);
        }
    }

    #[test]
    fn any_player_sacrifices_permanent_fires_for_controller_and_opponent() {
        // CR 603 + CR 701.21: "Whenever a player sacrifices a permanent" fires when
        // ANY player sacrifices a matching permanent — no controller restriction.
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Merchant of Venom".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, source_id);
        let trigger = parse_trigger_line(
            "Whenever a player sacrifices a permanent, put a +1/+1 counter on this creature.",
            "Merchant of Venom",
        );
        // Fires when controller (PlayerId(0)) sacrifices a permanent they own.
        let sacrificed_by_you = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Your Permanent".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, sacrificed_by_you);
        let event_you = GameEvent::PermanentSacrificed {
            object_id: sacrificed_by_you,
            player_id: PlayerId(0),
        };
        assert!(match_sacrificed(&event_you, &trigger, source_id, &state));

        // Fires when opponent (PlayerId(1)) sacrifices their permanent.
        let sacrificed_by_opp = create_object(
            &mut state,
            CardId(102),
            PlayerId(1),
            "Opponent Permanent".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, sacrificed_by_opp);
        let event_opp = GameEvent::PermanentSacrificed {
            object_id: sacrificed_by_opp,
            player_id: PlayerId(1),
        };
        assert!(match_sacrificed(&event_opp, &trigger, source_id, &state));
    }

    #[test]
    fn any_player_sacrifices_another_permanent_excludes_source() {
        // CR 109.1 + CR 603 + CR 701.21: Mazirek's "another permanent" carries
        // FilterProp::Another, which excludes the source from firing its own trigger
        // when the source itself is sacrificed.
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Mazirek, Kraul Death Priest".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, source_id);
        let trigger = parse_trigger_line(
            "Whenever a player sacrifices another permanent, put a +1/+1 counter on each creature you control.",
            "Mazirek, Kraul Death Priest",
        );

        // A different permanent being sacrificed → fires.
        let other_perm = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Other Permanent".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, other_perm);
        let event_other = GameEvent::PermanentSacrificed {
            object_id: other_perm,
            player_id: PlayerId(0),
        };
        assert!(match_sacrificed(&event_other, &trigger, source_id, &state));

        // Mazirek itself being sacrificed → does NOT fire (self-exclusion via Another).
        let event_self = GameEvent::PermanentSacrificed {
            object_id: source_id,
            player_id: PlayerId(0),
        };
        assert!(!match_sacrificed(&event_self, &trigger, source_id, &state));

        // Opponent sacrificing their own permanent also fires (any-player scope).
        let opp_perm = create_object(
            &mut state,
            CardId(202),
            PlayerId(1),
            "Opponent Permanent".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, opp_perm);
        let event_opp = GameEvent::PermanentSacrificed {
            object_id: opp_perm,
            player_id: PlayerId(1),
        };
        assert!(match_sacrificed(&event_opp, &trigger, source_id, &state));
    }

    // CR 603.2 + CR 701.21: "Whenever you sacrifice a <subtype>" — the valid_card
    // filter must consult the sacrificed object's subtypes and its controller.
    // Astrid Peth shape: "Whenever you sacrifice a Clue or Food, ~ explores."
    #[test]
    fn sacrifice_subtype_trigger_fires_when_controller_sacs_matching_subtype() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Astrid Peth".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, source_id);
        let trigger = parse_trigger_line(
            "Whenever you sacrifice a Clue or Food, ~ explores.",
            "Astrid Peth",
        );

        // You sacrifice a Food token → fires.
        let food = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Food Token".to_string(),
            Zone::Graveyard,
        );
        if let Some(obj) = state.objects.get_mut(&food) {
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Food".to_string());
            obj.is_token = true;
        }
        let food_event = GameEvent::PermanentSacrificed {
            object_id: food,
            player_id: PlayerId(0),
        };
        assert!(match_sacrificed(&food_event, &trigger, source_id, &state));

        // You sacrifice a Clue token → fires (disjunction branch).
        let clue = create_object(
            &mut state,
            CardId(302),
            PlayerId(0),
            "Clue Token".to_string(),
            Zone::Graveyard,
        );
        if let Some(obj) = state.objects.get_mut(&clue) {
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Clue".to_string());
            obj.is_token = true;
        }
        let clue_event = GameEvent::PermanentSacrificed {
            object_id: clue,
            player_id: PlayerId(0),
        };
        assert!(match_sacrificed(&clue_event, &trigger, source_id, &state));
    }

    #[test]
    fn sacrifice_subtype_trigger_rejects_non_matching_subtype() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(310),
            PlayerId(0),
            "Astrid Peth".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, source_id);
        let trigger = parse_trigger_line(
            "Whenever you sacrifice a Clue or Food, ~ explores.",
            "Astrid Peth",
        );

        // You sacrifice a Treasure (different subtype) → does NOT fire.
        let treasure = create_object(
            &mut state,
            CardId(311),
            PlayerId(0),
            "Treasure Token".to_string(),
            Zone::Graveyard,
        );
        if let Some(obj) = state.objects.get_mut(&treasure) {
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Treasure".to_string());
            obj.is_token = true;
        }
        let event = GameEvent::PermanentSacrificed {
            object_id: treasure,
            player_id: PlayerId(0),
        };
        assert!(!match_sacrificed(&event, &trigger, source_id, &state));

        // You sacrifice a plain creature (no Food subtype) → does NOT fire.
        let creature = create_object(
            &mut state,
            CardId(312),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Graveyard,
        );
        make_creature(&mut state, creature);
        let event = GameEvent::PermanentSacrificed {
            object_id: creature,
            player_id: PlayerId(0),
        };
        assert!(!match_sacrificed(&event, &trigger, source_id, &state));
    }

    #[test]
    fn sacrifice_subtype_trigger_rejects_opponent_sacrifice() {
        // CR 109.4: "you sacrifice" scopes to the source's controller. An opponent
        // sacrificing a matching token must NOT fire the controller's trigger.
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(320),
            PlayerId(0),
            "Astrid Peth".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, source_id);
        let trigger = parse_trigger_line(
            "Whenever you sacrifice a Clue or Food, ~ explores.",
            "Astrid Peth",
        );

        // Opponent sacrifices their Food → does NOT fire.
        let opp_food = create_object(
            &mut state,
            CardId(321),
            PlayerId(1),
            "Opponent Food".to_string(),
            Zone::Graveyard,
        );
        if let Some(obj) = state.objects.get_mut(&opp_food) {
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Food".to_string());
            obj.is_token = true;
        }
        let event = GameEvent::PermanentSacrificed {
            object_id: opp_food,
            player_id: PlayerId(1),
        };
        assert!(!match_sacrificed(&event, &trigger, source_id, &state));
    }

    #[test]
    fn sacrifice_blood_token_trigger_honors_token_property() {
        // CR 111.1 + CR 603.2 + CR 701.21: "Whenever you sacrifice a Blood token"
        // parses with FilterProp::Token, so a non-token object that happens to be a
        // Blood (hypothetical; future-proofs the filter composition) must NOT match.
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(330),
            PlayerId(0),
            "Vampire".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, source_id);
        let trigger = parse_trigger_line(
            "Whenever you sacrifice a Blood token, you gain 1 life.",
            "Vampire",
        );

        // Controller sacrifices a Blood token → fires.
        let blood_token = create_object(
            &mut state,
            CardId(331),
            PlayerId(0),
            "Blood Token".to_string(),
            Zone::Graveyard,
        );
        if let Some(obj) = state.objects.get_mut(&blood_token) {
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Blood".to_string());
            obj.is_token = true;
        }
        let event = GameEvent::PermanentSacrificed {
            object_id: blood_token,
            player_id: PlayerId(0),
        };
        assert!(match_sacrificed(&event, &trigger, source_id, &state));

        // Controller sacrifices a non-token artifact (no Blood subtype) → no fire.
        let artifact = create_object(
            &mut state,
            CardId(332),
            PlayerId(0),
            "Random Artifact".to_string(),
            Zone::Graveyard,
        );
        if let Some(obj) = state.objects.get_mut(&artifact) {
            obj.card_types.core_types.push(CoreType::Artifact);
        }
        let event = GameEvent::PermanentSacrificed {
            object_id: artifact,
            player_id: PlayerId(0),
        };
        assert!(!match_sacrificed(&event, &trigger, source_id, &state));
    }

    // CR 701.62 + CR 701.62b: Manifest Dread actor-side trigger.
    #[test]
    fn match_manifest_dread_fires_for_controller() {
        let mut state = setup();
        let trigger_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Paranormal Analyst".to_string(),
            Zone::Battlefield,
        );
        // A separate object acts as the effect source (could be the same, usually is).
        let dread_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Dread Source".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::ManifestDread);
        trigger.valid_target = Some(TargetFilter::Controller);

        let event = GameEvent::EffectResolved {
            kind: EffectKind::ManifestDread,
            source_id: dread_source,
        };
        assert!(match_manifest_dread(
            &event,
            &trigger,
            trigger_source,
            &state
        ));

        // Non-manifest-dread effect should not fire.
        let other = GameEvent::EffectResolved {
            kind: EffectKind::Manifest,
            source_id: dread_source,
        };
        assert!(!match_manifest_dread(
            &other,
            &trigger,
            trigger_source,
            &state
        ));
    }

    #[test]
    fn match_manifest_dread_filters_by_controller() {
        let mut state = setup();
        let trigger_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Paranormal Analyst".to_string(),
            Zone::Battlefield,
        );
        // Opponent performs the manifest-dread action.
        let opp_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Dread Source".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::ManifestDread);
        trigger.valid_target = Some(TargetFilter::Controller);

        let event = GameEvent::EffectResolved {
            kind: EffectKind::ManifestDread,
            source_id: opp_source,
        };
        // "Whenever you manifest dread" should not fire when the opponent
        // triggers the effect.
        assert!(!match_manifest_dread(
            &event,
            &trigger,
            trigger_source,
            &state
        ));
    }

    // CR 708 + CR 701.40b: TurnFaceUp matcher consumes `GameEvent::TurnedFaceUp`
    // and filters on both the face-up object and its controller.
    #[test]
    fn match_turn_face_up_fires_on_turned_face_up_event() {
        let mut state = setup();
        let trigger_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Growing Dread".to_string(),
            Zone::Battlefield,
        );
        let flipped = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Manifested Creature".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::TurnFaceUp);
        trigger.valid_card = Some(TargetFilter::Any);
        trigger.valid_target = Some(TargetFilter::Controller);

        let event = GameEvent::TurnedFaceUp { object_id: flipped };
        assert!(match_turn_face_up(&event, &trigger, trigger_source, &state));
    }

    #[test]
    fn match_turn_face_up_rejects_opponent_controller_for_you_filter() {
        let mut state = setup();
        let trigger_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Growing Dread".to_string(),
            Zone::Battlefield,
        );
        let flipped = create_object(
            &mut state,
            CardId(2),
            PlayerId(1), // opponent's manifest
            "Opponent Manifested".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::TurnFaceUp);
        trigger.valid_target = Some(TargetFilter::Controller);

        let event = GameEvent::TurnedFaceUp { object_id: flipped };
        assert!(!match_turn_face_up(
            &event,
            &trigger,
            trigger_source,
            &state
        ));
    }

    #[test]
    fn match_actor_against_filter_falls_back_to_source_id_when_valid_card_is_none() {
        // Forge-format ingest produces trigger defs without valid_card. The matcher
        // must degrade gracefully to a source_id membership check.
        let state = setup();
        let trigger = make_trigger(TriggerMode::Crews); // valid_card defaults to None
        let source = ObjectId(42);

        let event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(999),
            creatures: vec![source],
        };
        assert!(match_crews(&event, &trigger, source, &state));

        let wrong_event = GameEvent::VehicleCrewed {
            vehicle_id: ObjectId(999),
            creatures: vec![ObjectId(7)],
        };
        assert!(!match_crews(&wrong_event, &trigger, source, &state));
    }
}
