use std::collections::HashMap;

use crate::types::ability::{
    AbilityTag, CoinFlipResult, ControllerRef, DamageKindFilter, DestinationConstraint, EffectKind,
    OriginConstraint, TargetFilter, TargetRef, TriggerDefinition, TypedFilter,
};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::triggers::TriggerMatcher;

pub fn trigger_matcher(mode: TriggerMode) -> Option<TriggerMatcher> {
    Some(match mode {
        // CR 702.100a: Evolve — fires when a creature the trigger controller
        // controls enters the battlefield. build_evolve_trigger sets
        // .destination(Battlefield); valid_card filtering and the power/toughness
        // intervening-if (CR 603.4) are handled downstream by
        // zone_change_clause_matches / check_trigger_condition respectively.
        TriggerMode::ChangesZone | TriggerMode::Evolve => match_changes_zone,
        TriggerMode::Evolved => match_evolved,
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
        TriggerMode::Phase | TriggerMode::PayEcho | TriggerMode::PayCumulativeUpkeep => match_phase,
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
        TriggerMode::Clashed => match_clash,
        TriggerMode::RingTemptsYou => match_ring_tempts_you,
        TriggerMode::DungeonCompleted => match_dungeon_completed,
        TriggerMode::RoomEntered => match_room_entered,
        TriggerMode::UnlockDoor => match_unlock_door,
        TriggerMode::FullyUnlock => match_fully_unlock,
        TriggerMode::TakesInitiative => match_takes_initiative,
        TriggerMode::Exploited => match_exploited,
        TriggerMode::BecomeRenowned => match_become_renowned,
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
        TriggerMode::KeywordAbilityActivated(_) => match_keyword_ability_activated,
        TriggerMode::AbilityActivated => match_ability_activated,
        TriggerMode::Firebend => match_firebend,
        TriggerMode::Airbend => match_airbend,
        TriggerMode::Earthbend => match_earthbend,
        TriggerMode::Waterbend => match_waterbend,
        TriggerMode::ElementalBend => match_elemental_bend,
        TriggerMode::BecomesPlotted => match_becomes_plotted,
        // CR 104.3a: "Whenever a player loses the game" — dedicated matcher.
        TriggerMode::LosesGame => match_loses_game,
        TriggerMode::DamagePreventedOnce
        | TriggerMode::AbilityCast
        | TriggerMode::AbilityResolves
        | TriggerMode::AbilityTriggered
        | TriggerMode::SpellAbilityCast
        | TriggerMode::SpellAbilityCopy
        | TriggerMode::CounterPlayerAddedAll
        | TriggerMode::CounterTypeAddedAll
        | TriggerMode::PayLife
        | TriggerMode::PhaseIn
        | TriggerMode::PhaseOut
        | TriggerMode::PhaseOutAll
        | TriggerMode::NewGame
        | TriggerMode::Championed
        | TriggerMode::Exerted
        | TriggerMode::Enlisted
        | TriggerMode::Adapt
        | TriggerMode::Foretell
        | TriggerMode::Investigated
        | TriggerMode::PlanarDice
        | TriggerMode::PlaneswalkedFrom
        | TriggerMode::PlaneswalkedTo
        | TriggerMode::ChaosEnsues
        | TriggerMode::Copied
        | TriggerMode::ConjureAll
        | TriggerMode::Vote
        | TriggerMode::Abandoned
        | TriggerMode::ClaimPrize
        | TriggerMode::CrankContraption
        | TriggerMode::Devoured
        | TriggerMode::Discover
        | TriggerMode::Forage
        | TriggerMode::GiveGift
        | TriggerMode::Mentored
        | TriggerMode::Mutates
        | TriggerMode::Proliferate
        | TriggerMode::SeekAll
        | TriggerMode::SetInMotion
        | TriggerMode::Specializes
        | TriggerMode::Trains
        | TriggerMode::VisitAttraction => match_unimplemented,
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
    r.insert(TriggerMode::PayEcho, match_phase);
    // CR 702.24a: Cumulative upkeep — at-upkeep tax trigger; same matcher shape as Echo.
    r.insert(TriggerMode::PayCumulativeUpkeep, match_phase);
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

    // CR 701.30: Clash trigger
    r.insert(TriggerMode::Clashed, match_clash);

    // CR 701.54: Ring tempts you trigger
    r.insert(TriggerMode::RingTemptsYou, match_ring_tempts_you);

    // CR 309 / CR 701.49: Dungeon triggers
    r.insert(TriggerMode::DungeonCompleted, match_dungeon_completed);
    r.insert(TriggerMode::RoomEntered, match_room_entered);
    r.insert(TriggerMode::UnlockDoor, match_unlock_door);
    r.insert(TriggerMode::FullyUnlock, match_fully_unlock);
    r.insert(TriggerMode::BecomesPlotted, match_becomes_plotted);
    // CR 725: Initiative triggers
    r.insert(TriggerMode::TakesInitiative, match_takes_initiative);

    // CR 104.3a: "Whenever a player loses the game" — player-loss trigger.
    r.insert(TriggerMode::LosesGame, match_loses_game);

    // CR 702.110a: Exploit trigger matcher
    r.insert(TriggerMode::Exploited, match_exploited);

    // CR 701.37b: "When ~ becomes monstrous" — self-trigger on Monstrosity resolution.
    r.insert(TriggerMode::BecomeMonstrous, match_become_monstrous);
    // CR 702.112b: "When ~ becomes renowned" — self-trigger on Renown resolution.
    r.insert(TriggerMode::BecomeRenowned, match_become_renowned);

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
        TriggerMode::PhaseIn,
        TriggerMode::PhaseOut,
        TriggerMode::PhaseOutAll,
        TriggerMode::NewGame,
        // TriggerMode::TakesInitiative — moved to real matcher above
        // TriggerMode::LosesGame — moved to real matcher above
        TriggerMode::Championed,
        TriggerMode::Exerted,
        // TriggerMode::Crewed — moved to real matcher below
        // TriggerMode::Saddled — moved to real matcher below
        // TriggerMode::Evolve — moved to real matcher below
        // TriggerMode::Evolved — moved to real matcher below
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
        TriggerMode::Copied,
        TriggerMode::ConjureAll,
        TriggerMode::Vote,
        TriggerMode::Abandoned,
        TriggerMode::ClaimPrize,
        TriggerMode::CrankContraption,
        TriggerMode::Devoured,
        TriggerMode::Discover,
        TriggerMode::Forage,
        TriggerMode::GiveGift,
        TriggerMode::Mentored,
        TriggerMode::Mutates,
        TriggerMode::SeekAll,
        TriggerMode::SetInMotion,
        TriggerMode::Specializes,
        // TriggerMode::Stationed — moved to real matcher below
        TriggerMode::Trains,
        TriggerMode::VisitAttraction,
        // TriggerMode::BecomesCrewed — moved to real matcher below
        // TriggerMode::BecomesPlotted — moved to real matcher above
        // TriggerMode::BecomesSaddled — moved to real matcher below
    ];

    for mode in unimplemented_modes {
        r.insert(mode, match_unimplemented);
    }

    // CR 702.100a: Evolve — fires when a creature the trigger controller
    // controls enters the battlefield. build_evolve_trigger sets
    // .destination(Battlefield); valid_card filtering and the power/toughness
    // intervening-if (CR 603.4) are handled downstream by
    // zone_change_clause_matches / check_trigger_condition respectively.
    r.insert(TriggerMode::Evolve, match_changes_zone);
    // CR 702.100b: "Whenever [a creature] evolves" fires only when the
    // evolve ability's resolution actually put one or more +1/+1 counters on it.
    r.insert(TriggerMode::Evolved, match_evolved);

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
    // CR 702.107a + CR 702.142b + CR 702.177a: keyword ability activation triggers
    for tag in [AbilityTag::Boast, AbilityTag::Exhaust, AbilityTag::Outlast] {
        r.insert(
            TriggerMode::KeywordAbilityActivated(tag),
            match_keyword_ability_activated,
        );
    }
    // CR 602.1 + CR 605.1a: generic non-mana ability activation trigger
    // (Burning-Tree Shaman, Flamescroll Celebrant).
    r.insert(TriggerMode::AbilityActivated, match_ability_activated);

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
        TargetFilter::AttachedTo => {
            state
                .objects
                .get(&source_id)
                .and_then(|source| source.attached_to)
                .and_then(|host| host.as_player())
                == Some(player_id)
        }
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
        // CR 118.12a: unless-payer population — never matches an object.
        TargetFilter::AllPlayers => false,
        TargetFilter::Controller => false,
        // CR 109.5: OriginalController is a player reference, not an object.
        TargetFilter::OriginalController => false,
        TargetFilter::ScopedPlayer => false,
        // SpecificPlayer scopes to a player, not an object — never matches an object.
        TargetFilter::SpecificPlayer { .. } => false,
        TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::DefendingPlayer
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::Owner => false,
        TargetFilter::Any
        | TargetFilter::SelfRef
        | TargetFilter::SourceOrPaired
        | TargetFilter::Typed(_)
        | TargetFilter::Not { .. }
        | TargetFilter::Or { .. }
        | TargetFilter::And { .. }
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource
        | TargetFilter::Named { .. } => super::filter::matches_target_filter(
            state,
            object_id,
            filter,
            &super::filter::FilterContext::from_source(state, source_id),
        ),
    }
}

/// CR 603.2c: Count subjects matching `valid_card` in the events that fired a
/// batched trigger. Building block for "Whenever one or more <FILTER>
/// <verb>, do <X> that many <thing>" patterns (The Ur-Dragon's attack-and-draw
/// trigger, etc.).
///
/// Returns `None` when the count is undefined — `valid_card` is absent or is
/// `SelfRef` (the trigger source is its own subject and the "that many" math
/// degenerates to 1). Callers fall back to the existing
/// `EventContextAmount` cascade in `quantity.rs`.
pub(crate) fn count_trigger_subjects_in_batch(
    state: &GameState,
    valid_card: Option<&TargetFilter>,
    source_id: ObjectId,
    events: &[GameEvent],
) -> Option<u32> {
    let filter = match valid_card {
        Some(f) if !matches!(f, TargetFilter::SelfRef) => f,
        _ => return None,
    };
    let count = events.iter().fold(0u32, |acc, event| {
        acc.saturating_add(count_matching_trigger_event_subjects(
            state, source_id, filter, event,
        ))
    });
    Some(count)
}

/// CR 603.2c: Count object subjects carried by a single `GameEvent` for
/// trigger filter matching. Grows by event family as new "one or more
/// <FILTER> <verb>" patterns land. Variants without an object subject count 0.
fn count_matching_trigger_event_subjects(
    state: &GameState,
    source_id: ObjectId,
    filter: &TargetFilter,
    event: &GameEvent,
) -> u32 {
    let matches = |id| target_filter_matches_object(state, id, filter, source_id);
    let count_slice =
        |ids: &[ObjectId]| usize_to_u32_saturating(ids.iter().filter(|id| matches(**id)).count());
    let count_one = |id| u32::from(matches(id));
    match event {
        GameEvent::AttackersDeclared { attacker_ids, .. } => count_slice(attacker_ids),
        GameEvent::ZoneChanged { object_id, .. }
        | GameEvent::Discarded { object_id, .. }
        | GameEvent::SpellCast { object_id, .. }
        | GameEvent::TokenCreated { object_id, .. }
        | GameEvent::CreatureDestroyed { object_id }
        | GameEvent::Evolved { object_id }
        | GameEvent::PermanentSacrificed { object_id, .. }
        | GameEvent::PermanentTapped { object_id, .. }
        | GameEvent::PermanentUntapped { object_id } => count_one(*object_id),
        // Object target events yield the affected object as subject. Player
        // target events carry no object subject; player scoping lives on
        // `valid_target`.
        GameEvent::DamageDealt { target, .. } | GameEvent::BecomesTarget { target, .. } => {
            match target {
                TargetRef::Object(id) => count_one(*id),
                TargetRef::Player(_) => 0,
            }
        }
        GameEvent::GameStarted
        | GameEvent::TurnStarted { .. }
        | GameEvent::PhaseChanged { .. }
        | GameEvent::PriorityPassed { .. }
        | GameEvent::SpellCopied { .. }
        | GameEvent::XValueChosen { .. }
        | GameEvent::AbilityActivated { .. }
        | GameEvent::LifeChanged { .. }
        | GameEvent::ManaAdded { .. }
        | GameEvent::TappedForMana { .. }
        | GameEvent::ManaPoolEmptied { .. }
        | GameEvent::ManaRecolored { .. }
        | GameEvent::PlayerLost { .. }
        | GameEvent::MulliganStarted
        | GameEvent::CardsDrawn { .. }
        | GameEvent::CardDrawn { .. }
        | GameEvent::PermanentPhasedOut { .. }
        | GameEvent::PermanentPhasedIn { .. }
        | GameEvent::PlayerPhasedOut { .. }
        | GameEvent::PlayerPhasedIn { .. }
        | GameEvent::LandPlayed { .. }
        | GameEvent::StackPushed { .. }
        | GameEvent::StackResolved { .. }
        | GameEvent::DamageCleared { .. }
        | GameEvent::GameOver { .. }
        | GameEvent::DamagePrevented { .. }
        | GameEvent::SpellCountered { .. }
        | GameEvent::CounterAdded { .. }
        | GameEvent::CounterRemoved { .. }
        | GameEvent::ObjectConjured { .. }
        | GameEvent::EffectResolved { .. }
        | GameEvent::Unattached { .. }
        | GameEvent::BlockersDeclared { .. }
        | GameEvent::CombatTaxPaid { .. }
        | GameEvent::CombatTaxDeclined { .. }
        | GameEvent::VehicleCrewed { .. }
        | GameEvent::Stationed { .. }
        | GameEvent::Saddled { .. }
        | GameEvent::ReplacementApplied { .. }
        | GameEvent::Transformed { .. }
        | GameEvent::DayNightChanged { .. }
        | GameEvent::TurnedFaceUp { .. }
        | GameEvent::CardsRevealed { .. }
        | GameEvent::CombatDamageDealtToPlayer { .. }
        | GameEvent::PlayerEliminated { .. }
        | GameEvent::CrimeCommitted { .. }
        | GameEvent::Cycled { .. }
        | GameEvent::PlayerPerformedAction { .. }
        | GameEvent::Regenerated { .. }
        | GameEvent::CreatureSuspected { .. }
        | GameEvent::BecamePrepared { .. }
        | GameEvent::BecameUnprepared { .. }
        | GameEvent::CaseSolved { .. }
        | GameEvent::ClassLevelGained { .. }
        | GameEvent::MonarchChanged { .. }
        | GameEvent::CityBlessingGained { .. }
        | GameEvent::DieRolled { .. }
        | GameEvent::CoinFlipped { .. }
        | GameEvent::RingTemptsYou { .. }
        | GameEvent::RoomEntered { .. }
        | GameEvent::RoomDoorUnlocked { .. }
        | GameEvent::BecomesPlotted { .. }
        | GameEvent::DungeonCompleted { .. }
        | GameEvent::InitiativeTaken { .. }
        | GameEvent::Firebend { .. }
        | GameEvent::Airbend { .. }
        | GameEvent::Earthbend { .. }
        | GameEvent::Waterbend { .. }
        | GameEvent::CompanionRevealed { .. }
        | GameEvent::CompanionMovedToHand { .. }
        | GameEvent::NinjutsuActivated { .. }
        | GameEvent::KeywordAbilityActivated { .. }
        | GameEvent::CreatureExploited { .. }
        | GameEvent::EnergyChanged { .. }
        | GameEvent::SpeedChanged { .. }
        | GameEvent::PlayerCounterChanged { .. }
        | GameEvent::ManaExpended { .. }
        | GameEvent::Clash { .. }
        | GameEvent::VoteCast { .. }
        | GameEvent::VoteResolved { .. }
        | GameEvent::PowerToughnessChanged { .. }
        | GameEvent::CascadeMissed { .. }
        | GameEvent::DebugActionUsed { .. }
        | GameEvent::DebugPermissionGranted { .. }
        | GameEvent::DebugPermissionRevoked { .. } => 0,
    }
}

fn usize_to_u32_saturating(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn destination_matches_constraint(zone: Zone, constraint: &DestinationConstraint) -> bool {
    match constraint {
        DestinationConstraint::Any => true,
        DestinationConstraint::Equals(expected) => zone == *expected,
        DestinationConstraint::NotEquals(excluded) => zone != *excluded,
        DestinationConstraint::OneOf(zones) => zones.contains(&zone),
    }
}

// ---------------------------------------------------------------------------
// Core Trigger Matchers (~20 with real logic)
// ---------------------------------------------------------------------------

/// CR 603.6 + CR 603.6c: Tests whether one zone-change event satisfies a single
/// origin/destination/valid_card clause. Shared by both the scalar
/// `match_changes_zone` path and the disjunctive `zone_change_clauses` path.
#[allow(clippy::too_many_arguments)]
fn zone_change_clause_matches(
    origin: &OriginConstraint,
    destination: Option<&Zone>,
    destination_constraint: &DestinationConstraint,
    valid_card: Option<&TargetFilter>,
    from: &Option<Zone>,
    to: &Zone,
    record: &crate::types::game_state::ZoneChangeRecord,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    // CR 603.6c + CR 111.1: A zone-change event's `from` is `None` when the
    // object was created directly in `to` (token creation / emblem). Any
    // constraint that names a specific source zone cannot match such an event;
    // `OriginConstraint::Any` matches regardless.
    let origin_ok = match origin {
        OriginConstraint::Any => true,
        OriginConstraint::Equals(z) => from == &Some(*z),
        OriginConstraint::NotEquals(z) => matches!(from, Some(f) if f != z),
        OriginConstraint::OneOf(zs) => matches!(from, Some(f) if zs.contains(f)),
    };
    if !origin_ok {
        return false;
    }
    if let Some(dest) = destination {
        if dest != to {
            return false;
        }
    }
    if !destination_matches_constraint(*to, destination_constraint) {
        return false;
    }
    if let Some(filter) = valid_card {
        let ctx = super::filter::FilterContext::from_source(state, source_id);
        let matches = if *to == Zone::Battlefield && state.objects.contains_key(&record.object_id) {
            super::filter::matches_target_filter(state, record.object_id, filter, &ctx)
        } else {
            super::filter::matches_target_filter_on_zone_change_record(state, record, filter, &ctx)
        };
        if !matches {
            return false;
        }
    }
    true
}

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
        // CR 603.2: A disjunctive zone-change trigger fires if the event matches
        // ANY of its clauses. When `zone_change_clauses` is non-empty it fully
        // supersedes the scalar `origin`/`origin_zones`/`destination`/`valid_card`
        // path (Syr Konrad's three-way "dies / put into graveyard / leaves
        // graveyard" disjunction).
        if !trigger.zone_change_clauses.is_empty() {
            return trigger.zone_change_clauses.iter().any(|clause| {
                zone_change_clause_matches(
                    &clause.origin,
                    clause.destination.as_ref(),
                    &clause.destination_constraint,
                    clause.valid_card.as_ref(),
                    from,
                    to,
                    record,
                    source_id,
                    state,
                )
            });
        }
        // Scalar single-clause path. CR 603.10a: `origin_zones` is a disjunctive
        // source-zone set that takes precedence over single-zone `origin` when
        // non-empty. CR 111.1: `from = None` (token creation) cannot satisfy a
        // trigger that names any specific origin zone.
        let origin = if !trigger.origin_zones.is_empty() {
            OriginConstraint::OneOf(trigger.origin_zones.clone())
        } else if let Some(origin) = trigger.origin {
            OriginConstraint::Equals(origin)
        } else {
            OriginConstraint::Any
        };
        zone_change_clause_matches(
            &origin,
            trigger.destination.as_ref(),
            &trigger.destination_constraint,
            trigger.valid_card.as_ref(),
            from,
            to,
            record,
            source_id,
            state,
        )
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
        amount,
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
        // CR 603.2 + CR 120.1: Optional per-event damage-amount threshold
        // ("…deals 5 or more damage to a player"). When set, only damage events
        // whose amount satisfies the comparator vs the threshold fire the
        // trigger. CR 120.1 events carry a single nonnegative amount, so the
        // u32→i32 widening here cannot truncate.
        if let Some((cmp, threshold)) = trigger.damage_amount {
            if !cmp.evaluate(*amount as i32, threshold as i32) {
                return false;
            }
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

/// CR 601.2a vs CR 707.10: whether an event placed a spell on the stack by
/// *casting* it or by *copying* it. These are distinct game events — a copy
/// isn't cast — so copy-sensitive and cast-only triggers must be told apart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SpellOnStackClass {
    Cast,
    Copy,
}

// CR 603.6a + CR 707.10: spell-on-stack trigger. `SpellCast` fires only on a
// cast, `SpellCopy` only on a copy, and `SpellCastOrCopy` (Magecraft) on both.
pub(super) fn match_spell_cast(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    // Extract the (controller, spell object) tuple and the event class. Both
    // `SpellCast` and `SpellCopied` carry full stack characteristics for the
    // spell object, so the shared filter checks below work unchanged.
    let (controller, object_id, class) = match event {
        GameEvent::SpellCast {
            controller,
            object_id,
            ..
        } => (controller, object_id, SpellOnStackClass::Cast),
        GameEvent::SpellCopied {
            controller,
            object_id,
            ..
        } => (controller, object_id, SpellOnStackClass::Copy),
        _ => return false,
    };

    // CR 707.10: gate the event class against the trigger's mode.
    let accepts = match (&trigger.mode, class) {
        (TriggerMode::SpellCast, SpellOnStackClass::Cast)
        | (TriggerMode::SpellCopy, SpellOnStackClass::Copy)
        | (TriggerMode::SpellCastOrCopy, _) => true,
        (TriggerMode::SpellCast, SpellOnStackClass::Copy)
        | (TriggerMode::SpellCopy, SpellOnStackClass::Cast) => false,
        // `match_spell_cast` is only registered for the three spell-on-stack
        // modes; any other mode reaching here is a registry wiring bug.
        _ => false,
    };
    if !accepts {
        return false;
    }

    // CR 601.2a + CR 603.2: enforce the cast-origin discriminator BEFORE the
    // card/player filters so the cheap one-lookup zone-equality check
    // short-circuits before the expensive ControllerRef-resolving filters.
    // `class` is bound at the destructuring above. SpellCopied events
    // (CR 707.10) are copies, not casts — they carry no cast origin and are
    // rejected by any non-Any constraint.
    match (&trigger.spell_cast_origin, class) {
        (OriginConstraint::Any, _) => {}
        (_, SpellOnStackClass::Copy) => return false,
        (constraint, SpellOnStackClass::Cast) => {
            let Some(origin) = super::casting::spell_cast_origin(state, *object_id) else {
                // CR 601.2a: every cast has an origin; absence here is a
                // matcher data-flow bug. Fail-closed rather than fire
                // spuriously.
                return false;
            };
            let ok = match constraint {
                OriginConstraint::Any => unreachable!(),
                OriginConstraint::Equals(z) => *z == origin,
                OriginConstraint::NotEquals(z) => *z != origin,
                OriginConstraint::OneOf(zs) => zs.contains(&origin),
            };
            if !ok {
                return false;
            }
        }
    }

    // Check valid_card filter on the spell object.
    if trigger.valid_card.is_some() && !valid_card_matches(trigger, state, *object_id, source_id) {
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
        if !attack_target_type_matches(target, filter) {
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

pub(super) fn attack_target_type_matches(
    target: crate::game::combat::AttackTarget,
    filter: &crate::types::triggers::AttackTargetFilter,
) -> bool {
    matches!(
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
    )
}

pub(super) fn attack_target_defending_player(
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
    !matching_block_events(event, trigger, source_id, state).is_empty()
}

pub(super) fn matching_block_events(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> Vec<GameEvent> {
    if let GameEvent::BlockersDeclared { assignments } = event {
        assignments
            .iter()
            .filter_map(|(blocker, attacker)| {
                let blocker_matches = if trigger.valid_card.is_some() {
                    valid_card_matches(trigger, state, *blocker, source_id)
                } else {
                    *blocker == source_id
                };
                blocker_matches.then_some(GameEvent::BlockersDeclared {
                    assignments: vec![(*blocker, *attacker)],
                })
            })
            .collect()
    } else {
        Vec::new()
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

pub(super) fn match_evolved(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::Evolved { object_id } = event {
        valid_card_matches(trigger, state, *object_id, source_id)
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
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::PhaseChanged { phase } = event {
        let phase_matches = if let Some(ref trigger_phase) = trigger.phase {
            phase == trigger_phase
        } else {
            true
        };
        phase_matches && valid_player_matches(trigger, state, state.active_player, source_id)
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
        target,
        source_id: targeting_spell_id,
    } = event
    else {
        return false;
    };

    // CR 115.1a + CR 115.1b: Trigger text like "of a spell" and "of an Aura spell"
    // constrains the targeting source to matching stack spell characteristics.
    if let Some(source_filter) = &trigger.valid_source {
        let Some(targeting_entry) = state
            .stack
            .iter()
            .find(|entry| entry.id == *targeting_spell_id)
        else {
            return false;
        };
        let trigger_controller = state
            .objects
            .get(&source_id)
            .map(|obj| obj.controller)
            .unwrap_or(state.active_player);
        if !super::targeting::stack_entry_matches_filter(
            state,
            targeting_entry,
            source_filter,
            trigger_controller,
            source_id,
        ) {
            return false;
        }
    }

    match target {
        TargetRef::Object(object_id) => {
            // Check if the targeted object matches the trigger's valid_card filter.
            if trigger.valid_card.is_some() {
                valid_card_matches(trigger, state, *object_id, source_id)
            } else {
                *object_id == source_id
            }
        }
        TargetRef::Player(player_id) => {
            trigger.valid_card.is_none()
                && trigger.valid_target.is_some()
                && valid_player_matches(trigger, state, *player_id, source_id)
        }
    }
}

/// CR 700.13: Match CommitCrime triggers — scoped by trigger.valid_target.
///
/// `valid_target` controls which player's crimes activate the trigger:
/// - `Controller` → only controller's crimes (e.g., "whenever you commit a crime")
/// - `Typed(Opponent)` → only an opponent's crimes (e.g., "whenever an opponent commits a crime")
/// - `Player` → any player's crimes (e.g., "whenever a player commits a crime")
/// - `None` → any player's crimes (no-filter fallback)
pub(super) fn match_commit_crime(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CrimeCommitted { player_id } = event {
        // CR 700.13: Scope the trigger to the acting player via valid_target.
        valid_player_matches(trigger, state, *player_id, source_id)
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
    if let GameEvent::LandPlayed {
        object_id,
        player_id,
        from_zone,
    } = event
    {
        // CR 305.1 + CR 603.2: Scope the trigger to the acting player.
        // "whenever you play a land" → valid_target = Controller;
        // "whenever an opponent plays a land" → valid_target = Opponent filter.
        if !valid_player_matches(trigger, state, *player_id, source_id) {
            return false;
        }
        match &trigger.valid_card {
            None => true,
            Some(filter) => state.objects.get(object_id).is_some_and(|obj| {
                let record =
                    obj.snapshot_for_zone_change(*object_id, Some(*from_zone), Zone::Battlefield);
                let ctx = super::filter::FilterContext::from_source(state, source_id);
                super::filter::matches_target_filter_on_zone_change_record(
                    state, &record, filter, &ctx,
                )
            }),
        }
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
    if let GameEvent::BlockersDeclared { .. } = event {
        state
            .combat
            .as_ref()
            .and_then(|combat| {
                combat
                    .attackers
                    .iter()
                    .find(|attacker| attacker.object_id == source_id)
            })
            .is_some_and(|attacker| !attacker.blocked)
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

/// CR 701.3a: Attached triggers compare the object that became attached
/// (`valid_card`) with the host it is attached to (`valid_target`).
pub(super) fn match_attached(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match event {
        GameEvent::EffectResolved {
            kind: EffectKind::Attach | EffectKind::AttachAll | EffectKind::Equip,
            source_id: event_source_id,
        } => {
            let attachment_id = if matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::AttachAll,
                    ..
                }
            ) {
                source_id
            } else {
                *event_source_id
            };

            if attachment_id != source_id
                && !matches!(trigger.valid_target, Some(TargetFilter::SelfRef))
            {
                return false;
            }

            valid_card_matches(trigger, state, attachment_id, source_id)
                && attached_host_matches(trigger, state, attachment_id, source_id)
        }
        _ => false,
    }
}

fn attached_host_matches(
    trigger: &TriggerDefinition,
    state: &GameState,
    attachment_id: ObjectId,
    trigger_source_id: ObjectId,
) -> bool {
    let Some(host) = state
        .objects
        .get(&attachment_id)
        .and_then(|obj| obj.attached_to)
    else {
        return false;
    };
    let Some(filter) = trigger.valid_target.as_ref() else {
        return true;
    };
    match host {
        crate::game::game_object::AttachTarget::Object(object_id) => {
            target_filter_matches_object(state, object_id, filter, trigger_source_id)
        }
        crate::game::game_object::AttachTarget::Player(player_id) => {
            player_matches_filter(filter, state, player_id, trigger_source_id)
        }
    }
}

fn target_ref_matches_filter(
    target: &TargetRef,
    filter: &TargetFilter,
    state: &GameState,
    source_id: ObjectId,
) -> bool {
    match target {
        TargetRef::Object(object_id) => {
            target_filter_matches_object(state, *object_id, filter, source_id)
        }
        TargetRef::Player(player_id) => player_matches_filter(filter, state, *player_id, source_id),
    }
}

fn unattach_target_matches(
    trigger: &TriggerDefinition,
    old_target: &TargetRef,
    state: &GameState,
    source_id: ObjectId,
) -> bool {
    trigger
        .valid_target
        .as_ref()
        .is_none_or(|filter| target_ref_matches_filter(old_target, filter, state, source_id))
}

/// Unattach: fires when an attachment ceases to be attached.
/// CR 701.3d covers explicit unattach effects, reattachment to a different
/// host, and the attached object or host leaving the battlefield.
pub(super) fn match_unattach(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match event {
        GameEvent::Unattached {
            attachment_id,
            old_target,
        } => {
            *attachment_id == source_id
                && valid_card_matches(trigger, state, *attachment_id, source_id)
                && unattach_target_matches(trigger, old_target, state, source_id)
        }
        GameEvent::ZoneChanged {
            object_id, from, ..
        } if *from == Some(Zone::Battlefield) => {
            let old_target = TargetRef::Object(*object_id);
            valid_card_matches(trigger, state, source_id, source_id)
                && unattach_target_matches(trigger, &old_target, state, source_id)
                && state
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

/// CR 701.24a: Shuffled — fires when a player shuffles their library.
/// Uses `PlayerPerformedAction { ShuffledLibrary }` to identify the acting
/// player, then gates on `trigger.valid_target` (e.g. Cosi's Trickster:
/// "Whenever an opponent shuffles their library").
pub(super) fn match_shuffled(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::PlayerPerformedAction {
        player_id,
        action: PlayerActionKind::ShuffledLibrary,
    } = event
    else {
        return false;
    };
    valid_player_matches(trigger, state, *player_id, source_id)
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

/// Card-identity predicate for `TapsForMana` triggers: does the permanent that
/// was tapped for mana (`mana_source`) match the trigger's `valid_card` filter
/// (or, absent a filter, equal the trigger source itself)?
///
/// Extracted as a standalone authority so the aura mana-refund probe
/// (`mana_sources::aura_taps_for_mana_sources_for_land`) can ask the same
/// question without synthesizing a `GameEvent`.
pub(super) fn taps_for_mana_card_matches(
    trigger: &TriggerDefinition,
    state: &GameState,
    mana_source: ObjectId,
    source_id: ObjectId,
) -> bool {
    if trigger.valid_card.is_some() {
        valid_card_matches(trigger, state, mana_source, source_id)
    } else {
        mana_source == source_id
    }
}

/// TapsForMana: fires when source taps and produces mana.
///
/// CR 106.12a: triggers once per resolution of a mana ability whose activation
/// cost includes `{T}` — keyed off `GameEvent::TappedForMana`, which the engine
/// emits exactly once per such resolution (not once per mana unit).
pub(super) fn match_taps_for_mana(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::TappedForMana {
        player_id,
        source_id: mana_source,
        ..
    } = event
    {
        if !taps_for_mana_card_matches(trigger, state, *mana_source, source_id) {
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

/// CR 701.44b: Explored — fires when a creature explores.
/// When `valid_card` is set (e.g. "whenever a creature you control explores"),
/// the filter is checked against the event's source_id (the exploring creature).
pub(super) fn match_explored(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::EffectResolved {
        kind: EffectKind::Explore,
        source_id: explorer_id,
    } = event
    {
        if trigger.valid_card.is_some() {
            valid_card_matches(trigger, state, *explorer_id, source_id)
        } else {
            true
        }
    } else {
        false
    }
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

/// CR 702.112b: "When [subject] becomes renowned" — fires when Renown
/// resolution gives a permanent the renowned designation.
pub(super) fn match_become_renowned(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::EffectResolved {
        kind: EffectKind::Renown,
        source_id: renowned_id,
    } = event
    else {
        return false;
    };

    if let Some(filter) = &trigger.valid_source {
        return target_filter_matches_object(state, *renowned_id, filter, source_id);
    }
    if let Some(filter) = &trigger.valid_card {
        return target_filter_matches_object(state, *renowned_id, filter, source_id);
    }
    *renowned_id == source_id
}

/// CR 701.37b: "When ~ becomes monstrous" — self-trigger only.
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
        object_id,
        from,
        to,
        ..
    } = event
    {
        if *from != Some(Zone::Battlefield) {
            return false;
        }
        if let Some(destination) = trigger.destination {
            if destination != *to {
                return false;
            }
        }
        if !destination_matches_constraint(*to, &trigger.destination_constraint) {
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

/// DamageReceived: fires when the trigger source or a filtered player is dealt damage.
/// Uses DamageDealt event but checks the *target* (not the damage source) against the trigger.
///
/// Two target patterns are supported:
/// - Object target: "Whenever ~ is dealt damage" — `target == source_id`.
/// - Player target: "Whenever you're dealt damage" — `valid_target` scopes the player.
///
/// `valid_source` optionally scopes the damage source for either target shape.
pub(super) fn match_damage_received(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::DamageDealt {
        target,
        is_combat,
        amount,
        source_id: damage_source_id,
        ..
    } = event
    {
        match trigger.damage_kind {
            DamageKindFilter::Any => {}
            DamageKindFilter::CombatOnly if !is_combat => return false,
            DamageKindFilter::NoncombatOnly if *is_combat => return false,
            DamageKindFilter::CombatOnly | DamageKindFilter::NoncombatOnly => {}
        }
        // CR 603.2 + CR 120.1: Per-event damage-amount threshold. Mirrors
        // `match_damage_done` so a "is dealt N or more damage" trigger sets
        // `damage_amount` once and the field's semantics is uniform across
        // every damage-event matcher.
        if let Some((cmp, threshold)) = trigger.damage_amount {
            if !cmp.evaluate(*amount as i32, threshold as i32) {
                return false;
            }
        }
        match target {
            TargetRef::Object(target_id) => {
                // Object target: trigger source is the damaged permanent.
                *target_id == source_id
                    && valid_source_matches(trigger, state, *damage_source_id, source_id)
            }
            TargetRef::Player(pid) => {
                // Player target: check the damaged player matches valid_target
                // (e.g., "you" → Controller) and optionally that the damage
                // source matches valid_source. CR 120.1 + CR 120.3.
                if !valid_player_matches(trigger, state, *pid, source_id) {
                    return false;
                }
                valid_source_matches(trigger, state, *damage_source_id, source_id)
            }
        }
    } else {
        false
    }
}

/// CR 120.10: ExcessDamage — fires when the trigger source deals excess damage to a permanent.
///
/// Intentionally ignores `trigger.damage_amount`: that field gates on the raw
/// dealt `amount`, while excess-damage triggers semantically gate on the
/// `excess` field (the portion beyond lethal/loyalty/defense). No printed card
/// composes these two thresholds, and the parser does not emit
/// `damage_amount` on `ExcessDamage` modes.
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
///
/// See `match_excess_damage` for why `trigger.damage_amount` is not consulted.
pub(super) fn match_excess_damage_all(
    event: &GameEvent,
    _trigger: &TriggerDefinition,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::DamageDealt { excess, .. } if *excess > 0)
}

/// YouAttack: fires once when a player declares attackers matching the trigger's
/// player-scope filter AND attacker-type filter.
///
/// CR 508.1m + CR 603.2c: If `trigger.valid_target` is set, the matcher resolves
/// the attacking player (the common controller of the attackers — CR 506.2 / CR
/// 508.1) and checks it against the filter (e.g. `ControllerRef::Opponent` for
/// "another player attacks"). With no filter, the legacy "you attack" semantics
/// apply: fire when any attacker is controlled by the trigger's source controller.
///
/// CR 508.1 + CR 506.2: If `trigger.valid_card` is set, the trigger is an
/// "attack with one or more <TYPE>" form — it fires iff at least one declared
/// attacker (CR 506.2: controlled by the active player) matches the type filter.
/// The batch fires the trigger once (CR 603.2c). With no `valid_card`, any
/// attacker satisfies the type gate (legacy behavior preserved). Both the
/// player-scope (`valid_target`) and type (`valid_card`) gates must hold.
pub(super) fn match_you_attack(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    !matching_you_attack_pairs(event, trigger, source_id, state).is_empty()
}

pub(super) fn matching_you_attack_pairs(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> Vec<(ObjectId, crate::game::combat::AttackTarget)> {
    let GameEvent::AttackersDeclared {
        attacker_ids,
        defending_player,
        attacks,
        ..
    } = event
    else {
        return Vec::new();
    };
    if attacker_ids.is_empty() {
        return Vec::new();
    }
    // CR 506.2: the active player is the attacking player; all attackers in
    // a single AttackersDeclared batch share one controller.
    let Some(attacking_player) = attacker_ids
        .iter()
        .find_map(|id| state.objects.get(id).map(|o| o.controller))
    else {
        return Vec::new();
    };
    // CR 603.2c: the player-scope gate (valid_target). No filter ⇒ legacy
    // "attackers controlled by the trigger's source controller" semantics.
    let player_ok = match trigger.valid_target.as_ref() {
        // Parser legacy for "one or more creatures attack a player": the
        // attacked-player type is represented as `TargetFilter::Player`, not
        // as attacking-player scope. The per-attack target filter below handles
        // it, so keep the attacking-player gate permissive here.
        Some(TargetFilter::Player) => true,
        Some(_) => valid_player_matches(trigger, state, attacking_player, source_id),
        None => {
            let source_controller = state.objects.get(&source_id).map(|o| o.controller);
            Some(attacking_player) == source_controller
        }
    };
    if !player_ok {
        return Vec::new();
    }

    attacker_ids
        .iter()
        .filter_map(|id| {
            if trigger
                .valid_card
                .as_ref()
                .is_some_and(|filter| !target_filter_matches_object(state, *id, filter, source_id))
            {
                return None;
            }
            let target = attacks
                .iter()
                .find_map(|(attacker_id, target)| (*attacker_id == *id).then_some(*target))
                .unwrap_or(crate::game::combat::AttackTarget::Player(*defending_player));
            if trigger
                .attack_target_filter
                .as_ref()
                .is_some_and(|filter| !attack_target_type_matches(target, filter))
            {
                return None;
            }
            if matches!(trigger.valid_target, Some(TargetFilter::Player))
                && !matches!(target, crate::game::combat::AttackTarget::Player(_))
            {
                return None;
            }
            Some((*id, target))
        })
        .collect()
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
    if let GameEvent::DieRolled {
        player_id, sides, ..
    } = event
    {
        if trigger.die_sides.is_some_and(|required| required != *sides) {
            return false;
        }
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
    if let GameEvent::CoinFlipped { player_id, won } = event {
        // CR 705.2: If the trigger specifies a result filter, check it.
        if let Some(required) = &trigger.coin_flip_result {
            let event_won = *won;
            let matches = match required {
                CoinFlipResult::Won => event_won,
                CoinFlipResult::Lost => !event_won,
            };
            if !matches {
                return false;
            }
        }
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

/// CR 701.30b-c: Match clash events.
/// Fires when a clash occurs and either clashing player matches `valid_target`.
/// "Whenever you clash" sets `valid_target = Controller`; a generic "whenever
/// a player clashes" leaves `valid_target` unset to match any clash.
pub(super) fn match_clash(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    match event {
        GameEvent::Clash {
            controller,
            opponent,
            ..
        } => {
            valid_player_matches(trigger, state, *controller, source_id)
                || valid_player_matches(trigger, state, *opponent, source_id)
        }
        _ => false,
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

/// CR 104.3a: "Whenever a player loses the game" — fires when any player's
/// loss event is recorded. The `valid_target` filter (if set) restricts
/// which player's loss triggers the ability. Cards: Withengar Unbound,
/// Ramses Assassin Lord, Blood Tyrant.
pub(super) fn match_loses_game(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::PlayerLost { player_id } = event {
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

/// CR 709.5h: Match a Room door becoming unlocked.
pub(super) fn match_unlock_door(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::RoomDoorUnlocked {
        player_id,
        object_id,
        ..
    } = event
    {
        *object_id == source_id && valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

/// CR 709.5i: Match a Room permanent becoming fully unlocked.
pub(super) fn match_fully_unlock(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::RoomDoorUnlocked {
        player_id,
        object_id,
        fully_unlocked: true,
        ..
    } = event
    {
        let card_matches = if trigger.valid_card.is_some() {
            valid_card_matches(trigger, state, *object_id, source_id)
        } else {
            *object_id == source_id
        };
        card_matches && valid_player_matches(trigger, state, *player_id, source_id)
    } else {
        false
    }
}

/// CR 702.170c-d: Match "when this card becomes plotted" while the source is in exile.
pub(super) fn match_becomes_plotted(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::BecomesPlotted {
        object_id,
        player_id,
    } = event
    {
        *object_id == source_id && valid_player_matches(trigger, state, *player_id, source_id)
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

/// CR 702.107a + CR 702.142b + CR 702.177a + CR 603.2: Matches when a player activates
/// a keyword ability whose `AbilityTag` matches the trigger's `KeywordAbilityActivated` tag.
/// `valid_card` scopes source-specific forms like "~'s outlast ability"; generic forms
/// like "an exhaust ability" intentionally match any matching activation by the controller.
pub(super) fn match_keyword_ability_activated(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let TriggerMode::KeywordAbilityActivated(ref tag) = trigger.mode else {
        return false;
    };
    if let GameEvent::KeywordAbilityActivated {
        ability_tag,
        player_id,
        source_id: activated_id,
        ..
    } = event
    {
        ability_tag == tag
            && valid_card_matches(trigger, state, *activated_id, source_id)
            && state
                .objects
                .get(&source_id)
                .map(|obj| obj.controller == *player_id)
                .unwrap_or(false)
    } else {
        false
    }
}

/// CR 602.1 + CR 603.2 + CR 605.1a: Matches when any player activates an
/// activated ability that uses the stack (which by CR 605.3b excludes mana
/// abilities). Player scope is filtered via `trigger.valid_target` (e.g.
/// "an opponent" → `ControllerRef::Opponent` filter against the activating
/// player); when no `valid_target` is set, the trigger fires for every player
/// (Burning-Tree Shaman). Source-object filtering rides on `valid_card`
/// (reserved for future patterns like "an ability of an artifact source").
pub(super) fn match_ability_activated(
    event: &GameEvent,
    trigger: &TriggerDefinition,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let GameEvent::AbilityActivated {
        player_id,
        source_id: activated_id,
    } = event
    else {
        return false;
    };
    valid_player_matches(trigger, state, *player_id, source_id)
        && valid_card_matches(trigger, state, *activated_id, source_id)
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
    use crate::game::game_object::{AttachTarget, RoomDoor};
    use crate::game::zones::create_object;
    use crate::parser::oracle_trigger::parse_trigger_line;
    use crate::types::ability::{
        Comparator, ControllerRef, FilterProp, QuantityExpr, ResolvedAbility, TargetFilter,
        TriggerDefinition, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::events::{GameEvent, ManaTapState, PlayerActionKind};
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
    fn rolled_die_matcher_filters_player_and_sides() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pixie Guide".to_string(),
            Zone::Battlefield,
        );
        let mut trigger =
            make_trigger(TriggerMode::RolledDieOnce).valid_target(TargetFilter::Controller);
        trigger.die_sides = Some(20);

        assert!(match_rolled_die(
            &GameEvent::DieRolled {
                player_id: PlayerId(0),
                sides: 20,
                result: 13,
            },
            &trigger,
            source,
            &state,
        ));
        assert!(!match_rolled_die(
            &GameEvent::DieRolled {
                player_id: PlayerId(0),
                sides: 6,
                result: 4,
            },
            &trigger,
            source,
            &state,
        ));
        assert!(!match_rolled_die(
            &GameEvent::DieRolled {
                player_id: PlayerId(1),
                sides: 20,
                result: 13,
            },
            &trigger,
            source,
            &state,
        ));
    }

    #[test]
    fn flipped_coin_matcher_filters_player_and_result() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Krark's Thumb".to_string(),
            Zone::Battlefield,
        );
        let mut trigger =
            make_trigger(TriggerMode::FlippedCoin).valid_target(TargetFilter::Controller);
        trigger.coin_flip_result = Some(CoinFlipResult::Won);

        assert!(match_flipped_coin(
            &GameEvent::CoinFlipped {
                player_id: PlayerId(0),
                won: true,
            },
            &trigger,
            source,
            &state,
        ));
        assert!(!match_flipped_coin(
            &GameEvent::CoinFlipped {
                player_id: PlayerId(0),
                won: false,
            },
            &trigger,
            source,
            &state,
        ));
        assert!(!match_flipped_coin(
            &GameEvent::CoinFlipped {
                player_id: PlayerId(1),
                won: true,
            },
            &trigger,
            source,
            &state,
        ));
    }

    #[test]
    fn attached_trigger_matches_equipped_source_and_host_filter() {
        let mut state = setup();
        let equipment = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Inchblade Companion".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
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
        state.objects.get_mut(&equipment).unwrap().attached_to = Some(creature.into());

        let mut trigger = make_trigger(TriggerMode::Attached);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.valid_target = Some(TargetFilter::Typed(TypedFilter::creature()));
        let event = GameEvent::EffectResolved {
            kind: EffectKind::Equip,
            source_id: equipment,
        };

        assert!(match_attached(&event, &trigger, equipment, &state));
    }

    #[test]
    fn attached_trigger_rejects_wrong_host_filter() {
        let mut state = setup();
        let equipment = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Assimilation Aegis".to_string(),
            Zone::Battlefield,
        );
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
        state.objects.get_mut(&equipment).unwrap().attached_to = Some(land.into());

        let mut trigger = make_trigger(TriggerMode::Attached);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.valid_target = Some(TargetFilter::Typed(TypedFilter::creature()));
        let event = GameEvent::EffectResolved {
            kind: EffectKind::Equip,
            source_id: equipment,
        };

        assert!(!match_attached(&event, &trigger, equipment, &state));
    }

    #[test]
    fn attached_trigger_rejects_unrelated_equip_resolution() {
        let mut state = setup();
        let equipment = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Enormous Energy Blade".to_string(),
            Zone::Battlefield,
        );
        let other_equipment = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other Equipment".to_string(),
            Zone::Battlefield,
        );
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
        state.objects.get_mut(&equipment).unwrap().attached_to = Some(creature.into());

        let trigger = make_trigger(TriggerMode::Attached);
        let event = GameEvent::EffectResolved {
            kind: EffectKind::Equip,
            source_id: other_equipment,
        };

        assert!(!match_attached(&event, &trigger, equipment, &state));
    }

    /// CR 701.3a Pattern 2: "Whenever an Aura becomes attached to ~" fires when
    /// an Aura (event_source_id) attaches to the trigger source (source_id).
    /// Cards: Bramble Elemental, Brood Keeper.
    #[test]
    fn attached_pattern2_fires_when_aura_attaches_to_host() {
        let mut state = setup();
        // host = Bramble Elemental (trigger source)
        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bramble Elemental".to_string(),
            Zone::Battlefield,
        );
        // aura = some Aura card
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Rancor".to_string(),
            Zone::Battlefield,
        );
        // Mark the aura as an Enchantment with Aura subtype
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.attached_to = Some(host.into());
        }

        // Trigger: valid_card = Aura attachment, valid_target = trigger source host.
        let mut trigger = make_trigger(TriggerMode::Attached);
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::default().subtype("Aura".to_string()),
        ));
        trigger.valid_target = Some(TargetFilter::SelfRef);

        // Event: Attach resolved with the Aura as source
        let event = GameEvent::EffectResolved {
            kind: EffectKind::Attach,
            source_id: aura,
        };
        assert!(
            match_attached(&event, &trigger, host, &state),
            "Pattern 2 must fire when an Aura attaches to the trigger source"
        );

        // Should NOT fire if the aura attaches to a different host
        let other_host = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&aura).unwrap().attached_to = Some(other_host.into());
        assert!(
            !match_attached(&event, &trigger, host, &state),
            "Pattern 2 must not fire when the Aura attaches to a different host"
        );

        trigger.valid_target = None;
        state.objects.get_mut(&aura).unwrap().attached_to = Some(host.into());
        assert!(
            !match_attached(&event, &trigger, host, &state),
            "external attachment events must declare the trigger source host"
        );
    }

    #[test]
    fn unattach_trigger_matches_explicit_unattached_event_and_host_filter() {
        let mut state = setup();
        let equipment = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bludgeon Brawl".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
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

        let mut trigger = make_trigger(TriggerMode::Unattach);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.valid_target = Some(TargetFilter::Typed(TypedFilter::creature()));
        let event = GameEvent::Unattached {
            attachment_id: equipment,
            old_target: TargetRef::Object(creature),
        };

        assert!(match_unattach(&event, &trigger, equipment, &state));
    }

    #[test]
    fn unattach_trigger_rejects_wrong_old_host_filter() {
        let mut state = setup();
        let equipment = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bludgeon Brawl".to_string(),
            Zone::Battlefield,
        );
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

        let mut trigger = make_trigger(TriggerMode::Unattach);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.valid_target = Some(TargetFilter::Typed(TypedFilter::creature()));
        let event = GameEvent::Unattached {
            attachment_id: equipment,
            old_target: TargetRef::Object(land),
        };

        assert!(!match_unattach(&event, &trigger, equipment, &state));
    }

    #[test]
    fn unattach_trigger_matches_host_leaving_battlefield() {
        let mut state = setup();
        let equipment = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bludgeon Brawl".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
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
        state.objects.get_mut(&equipment).unwrap().attached_to = Some(creature.into());

        let mut trigger = make_trigger(TriggerMode::Unattach);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.valid_target = Some(TargetFilter::Typed(TypedFilter::creature()));
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

        assert!(match_unattach(&event, &trigger, equipment, &state));
    }

    #[test]
    fn land_played_valid_card_matches_origin_zone() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rocco, Street Chef".to_string(),
            Zone::Battlefield,
        );
        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Exiled Land".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let mut trigger = make_trigger(TriggerMode::LandPlayed);
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::land().properties(vec![FilterProp::InZone { zone: Zone::Exile }]),
        ));

        assert!(match_land_played(
            &GameEvent::LandPlayed {
                object_id: land,
                player_id: PlayerId(1),
                from_zone: Zone::Exile,
            },
            &trigger,
            source,
            &state,
        ));
        assert!(!match_land_played(
            &GameEvent::LandPlayed {
                object_id: land,
                player_id: PlayerId(1),
                from_zone: Zone::Hand,
            },
            &trigger,
            source,
            &state,
        ));
    }

    #[test]
    fn become_monarch_trigger_filters_player_scope() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Custodi Lich".to_string(),
            Zone::Battlefield,
        );
        let controller_trigger = parse_trigger_line(
            "Whenever you become the monarch, target player sacrifices a creature of their choice.",
            "Custodi Lich",
        );
        let opponent_trigger = parse_trigger_line(
            "Whenever an opponent becomes the monarch, that player loses 2 life.",
            "Knights of the Black Rose",
        );
        let any_player_trigger = parse_trigger_line(
            "Whenever a player becomes the monarch, draw a card.",
            "Test Card",
        );
        let controller_event = GameEvent::MonarchChanged {
            player_id: PlayerId(0),
        };
        let opponent_event = GameEvent::MonarchChanged {
            player_id: PlayerId(1),
        };

        assert!(match_become_monarch(
            &controller_event,
            &controller_trigger,
            source,
            &state,
        ));
        assert!(!match_become_monarch(
            &opponent_event,
            &controller_trigger,
            source,
            &state,
        ));
        assert!(match_become_monarch(
            &opponent_event,
            &opponent_trigger,
            source,
            &state,
        ));
        assert!(!match_become_monarch(
            &controller_event,
            &opponent_trigger,
            source,
            &state,
        ));
        assert!(match_become_monarch(
            &controller_event,
            &any_player_trigger,
            source,
            &state,
        ));
        assert!(match_become_monarch(
            &opponent_event,
            &any_player_trigger,
            source,
            &state,
        ));
    }

    #[test]
    fn city_of_traitors_another_land_excludes_source_land() {
        let mut state = setup();
        let city = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "City of Traitors".to_string(),
            Zone::Battlefield,
        );
        let other_land = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Ancient Tomb".to_string(),
            Zone::Battlefield,
        );
        let opponent_land = create_object(
            &mut state,
            CardId(12),
            PlayerId(1),
            "Opponent Land".to_string(),
            Zone::Battlefield,
        );
        for land in [city, other_land, opponent_land] {
            state
                .objects
                .get_mut(&land)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }

        let trigger = parse_trigger_line(
            "When you play another land, sacrifice this land.",
            "City of Traitors",
        );

        assert!(!match_land_played(
            &GameEvent::LandPlayed {
                object_id: city,
                player_id: PlayerId(0),
                from_zone: Zone::Hand,
            },
            &trigger,
            city,
            &state,
        ));
        assert!(match_land_played(
            &GameEvent::LandPlayed {
                object_id: other_land,
                player_id: PlayerId(0),
                from_zone: Zone::Hand,
            },
            &trigger,
            city,
            &state,
        ));
        assert!(!match_land_played(
            &GameEvent::LandPlayed {
                object_id: opponent_land,
                player_id: PlayerId(1),
                from_zone: Zone::Hand,
            },
            &trigger,
            city,
            &state,
        ));
    }

    #[test]
    fn becomes_plotted_matches_only_source_card() {
        let mut state = setup();
        let plotted = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Aloe Alchemist".to_string(),
            Zone::Exile,
        );
        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other Card".to_string(),
            Zone::Exile,
        );
        let trigger = make_trigger(TriggerMode::BecomesPlotted);

        assert!(match_becomes_plotted(
            &GameEvent::BecomesPlotted {
                object_id: plotted,
                player_id: PlayerId(0),
            },
            &trigger,
            plotted,
            &state
        ));
        assert!(!match_becomes_plotted(
            &GameEvent::BecomesPlotted {
                object_id: other,
                player_id: PlayerId(0),
            },
            &trigger,
            plotted,
            &state
        ));
    }

    #[test]
    fn keyword_ability_activation_matches_generic_controller_trigger() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rangers' Aetherhive".to_string(),
            Zone::Battlefield,
        );
        let activated_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Another Exhaust Creature".to_string(),
            Zone::Battlefield,
        );
        let trigger = make_trigger(TriggerMode::KeywordAbilityActivated(AbilityTag::Exhaust));

        // Generic "you activate an exhaust ability" triggers may match a different source.
        assert!(match_keyword_ability_activated(
            &GameEvent::KeywordAbilityActivated {
                ability_tag: AbilityTag::Exhaust,
                player_id: PlayerId(0),
                source_id: activated_source,
                is_mana_ability: false,
            },
            &trigger,
            source,
            &state
        ));
        // Wrong controller must not match.
        assert!(!match_keyword_ability_activated(
            &GameEvent::KeywordAbilityActivated {
                ability_tag: AbilityTag::Exhaust,
                player_id: PlayerId(1),
                source_id: activated_source,
                is_mana_ability: false,
            },
            &trigger,
            source,
            &state
        ));
        // Wrong ability tag must not match.
        assert!(!match_keyword_ability_activated(
            &GameEvent::KeywordAbilityActivated {
                ability_tag: AbilityTag::Boast,
                player_id: PlayerId(0),
                source_id: source,
                is_mana_ability: false,
            },
            &trigger,
            source,
            &state
        ));
    }

    #[test]
    fn keyword_ability_activation_valid_card_scopes_self_reference() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Herald of Anafenza".to_string(),
            Zone::Battlefield,
        );
        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Abzan Falconer".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::KeywordAbilityActivated(AbilityTag::Outlast));
        trigger.valid_card = Some(TargetFilter::SelfRef);

        assert!(match_keyword_ability_activated(
            &GameEvent::KeywordAbilityActivated {
                ability_tag: AbilityTag::Outlast,
                player_id: PlayerId(0),
                source_id: source,
                is_mana_ability: false,
            },
            &trigger,
            source,
            &state
        ));
        assert!(!match_keyword_ability_activated(
            &GameEvent::KeywordAbilityActivated {
                ability_tag: AbilityTag::Outlast,
                player_id: PlayerId(0),
                source_id: other,
                is_mana_ability: false,
            },
            &trigger,
            source,
            &state
        ));
    }

    // --- CR 602.1 + CR 605.1a: generic non-mana ability activation matcher ---

    #[test]
    fn ability_activation_a_player_scope_matches_every_player() {
        // Burning-Tree Shaman: "Whenever a player activates an ability …" —
        // no valid_target filter, so every player's activation triggers.
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Burning-Tree Shaman".to_string(),
            Zone::Battlefield,
        );
        let activated = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        let trigger = make_trigger(TriggerMode::AbilityActivated);

        // Opponent's activation fires.
        assert!(match_ability_activated(
            &GameEvent::AbilityActivated {
                player_id: PlayerId(1),
                source_id: activated,
            },
            &trigger,
            source,
            &state
        ));
        // Own activation also fires.
        assert!(match_ability_activated(
            &GameEvent::AbilityActivated {
                player_id: PlayerId(0),
                source_id: activated,
            },
            &trigger,
            source,
            &state
        ));
    }

    #[test]
    fn ability_activation_an_opponent_scope_filters_by_controller() {
        // Flamescroll Celebrant: "Whenever an opponent activates an ability …"
        // — valid_target scopes the activator to opponents of the source's
        // controller.
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Flamescroll Celebrant".to_string(),
            Zone::Battlefield,
        );
        let activated = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::AbilityActivated);
        trigger.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));

        // Opponent activation fires.
        assert!(match_ability_activated(
            &GameEvent::AbilityActivated {
                player_id: PlayerId(1),
                source_id: activated,
            },
            &trigger,
            source,
            &state
        ));
        // Own activation must NOT fire.
        assert!(!match_ability_activated(
            &GameEvent::AbilityActivated {
                player_id: PlayerId(0),
                source_id: activated,
            },
            &trigger,
            source,
            &state
        ));
    }

    #[test]
    fn ability_activation_rejects_unrelated_event() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Burning-Tree Shaman".to_string(),
            Zone::Battlefield,
        );
        let trigger = make_trigger(TriggerMode::AbilityActivated);
        // SpellCast is a different family — must not match.
        assert!(!match_ability_activated(
            &GameEvent::SpellCast {
                card_id: CardId(2),
                controller: PlayerId(1),
                object_id: ObjectId(99),
            },
            &trigger,
            source,
            &state
        ));
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

    #[test]
    fn attacks_trigger_matches_player_host_for_attached_to_target() {
        let mut state = setup();
        let curse = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Curse".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&curse).unwrap().attached_to =
            Some(AttachTarget::Player(PlayerId(1)));

        let attacker = create_object(
            &mut state,
            CardId(12),
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

        let mut trigger = make_trigger(TriggerMode::Attacks);
        trigger.valid_card = Some(TargetFilter::Typed(TypedFilter::creature()));
        trigger.valid_target = Some(TargetFilter::AttachedTo);

        let enchanted_player_event = GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker],
            defending_player: PlayerId(1),
            attacks: vec![(
                attacker,
                crate::game::combat::AttackTarget::Player(PlayerId(1)),
            )],
        };
        assert!(match_attacks(
            &enchanted_player_event,
            &trigger,
            curse,
            &state
        ));

        let other_player_event = GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker],
            defending_player: PlayerId(0),
            attacks: vec![(
                attacker,
                crate::game::combat::AttackTarget::Player(PlayerId(0)),
            )],
        };
        assert!(!match_attacks(&other_player_event, &trigger, curse, &state));
    }

    #[test]
    fn room_door_unlock_events_match_existing_trigger_modes() {
        let mut state = setup();
        let room = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Test Room".to_string(),
            Zone::Battlefield,
        );

        let unlock_trigger = make_trigger(TriggerMode::UnlockDoor);
        let partial_unlock_event = GameEvent::RoomDoorUnlocked {
            player_id: PlayerId(0),
            object_id: room,
            door: RoomDoor::Left,
            fully_unlocked: false,
        };
        assert!(match_unlock_door(
            &partial_unlock_event,
            &unlock_trigger,
            room,
            &state
        ));

        let fully_unlock_trigger = make_trigger(TriggerMode::FullyUnlock);
        assert!(!match_fully_unlock(
            &partial_unlock_event,
            &fully_unlock_trigger,
            room,
            &state
        ));

        let fully_unlock_event = GameEvent::RoomDoorUnlocked {
            player_id: PlayerId(0),
            object_id: room,
            door: RoomDoor::Right,
            fully_unlocked: true,
        };
        assert!(match_fully_unlock(
            &fully_unlock_event,
            &fully_unlock_trigger,
            room,
            &state
        ));
    }

    #[test]
    fn fully_unlock_room_trigger_matches_observer_with_room_filter() {
        let mut state = setup();
        let room = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Test Room".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&room).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Room".to_string());
        }
        let observer = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Entity Tracker".to_string(),
            Zone::Battlefield,
        );

        let mut trigger = make_trigger(TriggerMode::FullyUnlock);
        trigger.valid_target = Some(TargetFilter::Controller);
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::default().subtype("Room".to_string()),
        ));
        let fully_unlock_event = GameEvent::RoomDoorUnlocked {
            player_id: PlayerId(0),
            object_id: room,
            door: RoomDoor::Right,
            fully_unlocked: true,
        };
        assert!(match_fully_unlock(
            &fully_unlock_event,
            &trigger,
            observer,
            &state
        ));

        let opponent_unlock_event = GameEvent::RoomDoorUnlocked {
            player_id: PlayerId(1),
            object_id: room,
            door: RoomDoor::Right,
            fully_unlocked: true,
        };
        assert!(!match_fully_unlock(
            &opponent_unlock_event,
            &trigger,
            observer,
            &state
        ));
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
    fn match_changes_zone_disjunctive() {
        use crate::types::ability::{OriginConstraint, ZoneChangeClause};

        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        // CR 603.6: Syr Konrad's three-clause disjunction.
        trigger.zone_change_clauses = vec![
            // Clause 1: another creature dies (battlefield -> graveyard).
            ZoneChangeClause {
                origin: OriginConstraint::Equals(Zone::Battlefield),
                destination: Some(Zone::Graveyard),
                destination_constraint: DestinationConstraint::Any,
                valid_card: None,
            },
            // Clause 2: a creature card put into a graveyard from anywhere
            // other than the battlefield.
            ZoneChangeClause {
                origin: OriginConstraint::NotEquals(Zone::Battlefield),
                destination: Some(Zone::Graveyard),
                destination_constraint: DestinationConstraint::Any,
                valid_card: None,
            },
            // Clause 3: a creature card leaves the graveyard (any destination).
            ZoneChangeClause {
                origin: OriginConstraint::Equals(Zone::Graveyard),
                destination: None,
                destination_constraint: DestinationConstraint::Any,
                valid_card: None,
            },
        ];

        // Clause 1: dies in combat.
        let dies = zone_changed_event(
            ObjectId(5),
            Zone::Battlefield,
            Zone::Graveyard,
            vec![CoreType::Creature],
            Vec::new(),
        );
        assert!(match_changes_zone(&dies, &trigger, ObjectId(1), &state));

        // Clause 2: milled from library into graveyard.
        let milled = zone_changed_event(
            ObjectId(6),
            Zone::Library,
            Zone::Graveyard,
            vec![CoreType::Creature],
            Vec::new(),
        );
        assert!(match_changes_zone(&milled, &trigger, ObjectId(1), &state));

        // Clause 3: creature card leaves the graveyard for the hand.
        let leaves_graveyard = zone_changed_event(
            ObjectId(7),
            Zone::Graveyard,
            Zone::Hand,
            vec![CoreType::Creature],
            Vec::new(),
        );
        assert!(match_changes_zone(
            &leaves_graveyard,
            &trigger,
            ObjectId(1),
            &state
        ));

        // Matches no clause: a creature enters the battlefield from hand.
        let etb = zone_changed_event(
            ObjectId(8),
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        );
        assert!(!match_changes_zone(&etb, &trigger, ObjectId(1), &state));

        // Implicit `from = None` guard: a token created directly in the
        // graveyard must NOT satisfy clause 2's `NotEquals(Battlefield)`.
        let created_in_graveyard = GameEvent::ZoneChanged {
            object_id: ObjectId(9),
            from: None,
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                ..ZoneChangeRecord::test_minimal(ObjectId(9), None, Zone::Graveyard)
            }),
        };
        assert!(!match_changes_zone(
            &created_in_graveyard,
            &trigger,
            ObjectId(1),
            &state
        ));
    }

    #[test]
    fn match_changes_zone_clause_origin_one_of_excluding_graveyard_and_exile() {
        use crate::types::ability::{OriginConstraint, ZoneChangeClause};

        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        // CR 603.6a + CR 603.2: "Name Sticker" Goblin's "enters from anywhere
        // other than a graveyard or exile" is modeled with the existing
        // positive source-zone set over every concrete zone except Graveyard
        // and Exile. `from = None` (token creation, CR 111.1) still rejects.
        trigger.zone_change_clauses = vec![ZoneChangeClause {
            origin: OriginConstraint::OneOf(vec![
                Zone::Library,
                Zone::Hand,
                Zone::Battlefield,
                Zone::Stack,
                Zone::Command,
            ]),
            destination: Some(Zone::Battlefield),
            destination_constraint: DestinationConstraint::Any,
            valid_card: None,
        }];

        // Hand → Battlefield: Hand is in the allowed set, must match.
        let from_hand = zone_changed_event(
            ObjectId(5),
            Zone::Hand,
            Zone::Battlefield,
            Vec::new(),
            Vec::new(),
        );
        assert!(match_changes_zone(
            &from_hand,
            &trigger,
            ObjectId(1),
            &state
        ));

        // Library → Battlefield: Library is in the allowed set, must match.
        let from_library = zone_changed_event(
            ObjectId(6),
            Zone::Library,
            Zone::Battlefield,
            Vec::new(),
            Vec::new(),
        );
        assert!(match_changes_zone(
            &from_library,
            &trigger,
            ObjectId(1),
            &state
        ));

        // Graveyard → Battlefield: Graveyard is not in the allowed set, must NOT match.
        let from_graveyard = zone_changed_event(
            ObjectId(7),
            Zone::Graveyard,
            Zone::Battlefield,
            Vec::new(),
            Vec::new(),
        );
        assert!(!match_changes_zone(
            &from_graveyard,
            &trigger,
            ObjectId(1),
            &state
        ));

        // Exile → Battlefield: Exile is not in the allowed set, must NOT match.
        let from_exile = zone_changed_event(
            ObjectId(8),
            Zone::Exile,
            Zone::Battlefield,
            Vec::new(),
            Vec::new(),
        );
        assert!(!match_changes_zone(
            &from_exile,
            &trigger,
            ObjectId(1),
            &state
        ));

        // None → Battlefield: token created directly on the battlefield
        // (CR 111.1). `OriginConstraint::OneOf` only matches concrete
        // `Some(zone)` origins, so it rejects `None`.
        let from_none = GameEvent::ZoneChanged {
            object_id: ObjectId(9),
            from: None,
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                ObjectId(9),
                None,
                Zone::Battlefield,
            )),
        };
        assert!(!match_changes_zone(
            &from_none,
            &trigger,
            ObjectId(1),
            &state
        ));
    }

    #[test]
    fn nontoken_artifact_etb_trigger_rejects_created_artifact_tokens() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Weapons Manufacturing".to_string(),
            Zone::Battlefield,
        );
        let trigger = parse_trigger_line(
            "Whenever one or more nontoken artifacts you control enter, create a Munitions token.",
            "Weapons Manufacturing",
        );

        let valid_card = trigger.valid_card.as_ref().expect("valid_card");
        let TargetFilter::Typed(tf) = valid_card else {
            panic!("expected typed valid_card, got {valid_card:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        assert!(tf.properties.contains(&FilterProp::NonToken));

        let nontoken_artifact = ObjectId(31);
        let nontoken_event = GameEvent::ZoneChanged {
            object_id: nontoken_artifact,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                core_types: vec![CoreType::Artifact],
                controller: PlayerId(0),
                owner: PlayerId(0),
                is_token: false,
                ..ZoneChangeRecord::test_minimal(
                    nontoken_artifact,
                    Some(Zone::Hand),
                    Zone::Battlefield,
                )
            }),
        };
        assert!(match_changes_zone(
            &nontoken_event,
            &trigger,
            source_id,
            &state
        ));

        let munitions = ObjectId(32);
        let token_event = GameEvent::ZoneChanged {
            object_id: munitions,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                name: "Munitions".to_string(),
                core_types: vec![CoreType::Artifact],
                controller: PlayerId(0),
                owner: PlayerId(0),
                is_token: true,
                ..ZoneChangeRecord::test_minimal(munitions, None, Zone::Battlefield)
            }),
        };
        assert!(!match_changes_zone(
            &token_event,
            &trigger,
            source_id,
            &state
        ));
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
    fn player_performed_action_matches_proliferate() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(15),
            PlayerId(0),
            "Scheming Aspirant".to_string(),
            Zone::Battlefield,
        );
        let trigger = parse_trigger_line(
            "Whenever you proliferate, each opponent loses 2 life and you gain 2 life.",
            "Scheming Aspirant",
        );
        let event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(0),
            action: PlayerActionKind::Proliferate,
        };
        assert!(match_player_action(&event, &trigger, source_id, &state));
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
    fn leaves_battlefield_without_dying_rejects_graveyard_destination() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::LeavesBattlefield);
        trigger.destination_constraint = DestinationConstraint::NotEquals(Zone::Graveyard);

        let to_exile = zone_changed_event(
            ObjectId(5),
            Zone::Battlefield,
            Zone::Exile,
            Vec::new(),
            Vec::new(),
        );
        assert!(match_leaves_battlefield(
            &to_exile,
            &trigger,
            ObjectId(1),
            &state
        ));

        let to_graveyard = zone_changed_event(
            ObjectId(5),
            Zone::Battlefield,
            Zone::Graveyard,
            Vec::new(),
            Vec::new(),
        );
        assert!(!match_leaves_battlefield(
            &to_graveyard,
            &trigger,
            ObjectId(1),
            &state
        ));
    }

    #[test]
    fn changes_zone_origin_graveyard_rejects_command_zone_event() {
        // CR 603.6 + CR 603.6a — issue #396: Flayer of the Hatebound's
        // "whenever this creature or another creature enters from your
        // graveyard" trigger must NOT fire when a creature enters from any
        // other zone. Drives the same runtime entry-point the engine uses
        // (`match_changes_zone`) with a Command-zone → Battlefield event to
        // prove the parsed `origin = Some(Graveyard)` is actually honored at
        // match time (and a parser-only fix is not a no-op).
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Graveyard);
        trigger.destination = Some(Zone::Battlefield);

        // Positive case: enters from graveyard — must match.
        let graveyard_event = zone_changed_event(
            ObjectId(5),
            Zone::Graveyard,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        );
        assert!(match_changes_zone(
            &graveyard_event,
            &trigger,
            ObjectId(1),
            &state,
        ));

        // Negative case (the user-reported bug): commander cast from the
        // command zone — must NOT match.
        let command_zone_event = zone_changed_event(
            ObjectId(5),
            Zone::Command,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        );
        assert!(!match_changes_zone(
            &command_zone_event,
            &trigger,
            ObjectId(1),
            &state,
        ));

        // Negative case: creature cast normally from hand — must NOT match.
        let hand_event = zone_changed_event(
            ObjectId(5),
            Zone::Hand,
            Zone::Battlefield,
            vec![CoreType::Creature],
            Vec::new(),
        );
        assert!(!match_changes_zone(
            &hand_event,
            &trigger,
            ObjectId(1),
            &state,
        ));
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
    fn changes_zone_parsed_teval_trigger_scopes_to_own_graveyard() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(893),
            PlayerId(0),
            "Teval, the Balanced Scale".to_string(),
            Zone::Battlefield,
        );
        let trigger = parse_trigger_line(
            "Whenever one or more cards leave your graveyard, create a 2/2 black Zombie Druid creature token.",
            "Teval, the Balanced Scale",
        );

        let own_card = ObjectId(100);
        let own_card_leaves_graveyard = GameEvent::ZoneChanged {
            object_id: own_card,
            from: Some(Zone::Graveyard),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                controller: PlayerId(0),
                owner: PlayerId(0),
                ..ZoneChangeRecord::test_minimal(own_card, Some(Zone::Graveyard), Zone::Battlefield)
            }),
        };
        assert!(match_changes_zone(
            &own_card_leaves_graveyard,
            &trigger,
            source,
            &state
        ));

        let opponent_card = ObjectId(101);
        let opponent_card_leaves_graveyard = GameEvent::ZoneChanged {
            object_id: opponent_card,
            from: Some(Zone::Graveyard),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                controller: PlayerId(1),
                owner: PlayerId(1),
                ..ZoneChangeRecord::test_minimal(
                    opponent_card,
                    Some(Zone::Graveyard),
                    Zone::Battlefield,
                )
            }),
        };
        assert!(
            !match_changes_zone(&opponent_card_leaves_graveyard, &trigger, source, &state),
            "Teval must not trigger for a card leaving an opponent's graveyard"
        );

        let opponent_creature = ObjectId(102);
        let opponent_creature_dies = GameEvent::ZoneChanged {
            object_id: opponent_creature,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                controller: PlayerId(1),
                owner: PlayerId(1),
                ..ZoneChangeRecord::test_minimal(
                    opponent_creature,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            }),
        };
        assert!(
            !match_changes_zone(&opponent_creature_dies, &trigger, source, &state),
            "Teval must not trigger for an opponent's creature dying"
        );
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
            vec![crate::types::ability::FilterProp::PtComparison {
                stat: crate::types::ability::PtStat::Power,
                scope: crate::types::ability::PtValueScope::Current,
                comparator: crate::types::ability::Comparator::GE,
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

    /// Push a spell stack entry whose `ResolvedAbility.context.cast_from_zone`
    /// is set to `origin` — mirrors the production path in
    /// `casting_costs.rs:2540` where the cast-origin zone is stamped on the
    /// ability context before `GameEvent::SpellCast` is emitted.
    fn push_spell_with_cast_origin(
        state: &mut GameState,
        object_id: ObjectId,
        controller: PlayerId,
        origin: Zone,
    ) {
        let mut ability = ResolvedAbility::new(
            crate::types::ability::Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            object_id,
            controller,
        );
        ability.context.cast_from_zone = Some(origin);
        state.stack.push_back(StackEntry {
            id: object_id,
            source_id: object_id,
            controller,
            kind: StackEntryKind::Spell {
                card_id: CardId(100),
                ability: Some(ability),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
    }

    /// CR 601.2a + #538 — Ghostly Pilferer shape. Trigger has
    /// `spell_cast_origin = NotEquals(Hand)`; an opponent casting an instant
    /// from hand must NOT fire it. Discriminating: the pre-fix matcher (no
    /// cast-origin gate) returned `true` for this event.
    #[test]
    fn spell_cast_not_equals_hand_rejects_hand_cast() {
        let mut state = setup();
        let trigger_controller = PlayerId(0);
        let opponent = PlayerId(1);
        // Trigger source must be a real object so `valid_target` resolution
        // can read its controller (CR 109.5).
        let source = create_object(
            &mut state,
            CardId(1),
            trigger_controller,
            "Ghostly Pilferer".to_string(),
            Zone::Battlefield,
        );
        let spell_id = ObjectId(70);
        push_spell_with_cast_origin(&mut state, spell_id, opponent, Zone::Hand);

        let mut trigger = make_trigger(TriggerMode::SpellCast);
        trigger.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));
        trigger.spell_cast_origin = OriginConstraint::NotEquals(Zone::Hand);

        let event = GameEvent::SpellCast {
            card_id: CardId(100),
            controller: opponent,
            object_id: spell_id,
        };
        assert!(!match_spell_cast(&event, &trigger, source, &state));
    }

    /// CR 601.2a + #538 — same trigger shape, but the opponent casts from
    /// exile (flashback). Trigger MUST fire. Companion discriminator to the
    /// negative test above — together they prove the gate distinguishes
    /// origins rather than uniformly accepting or rejecting.
    #[test]
    fn spell_cast_not_equals_hand_accepts_exile_cast() {
        let mut state = setup();
        let trigger_controller = PlayerId(0);
        let opponent = PlayerId(1);
        let source = create_object(
            &mut state,
            CardId(1),
            trigger_controller,
            "Ghostly Pilferer".to_string(),
            Zone::Battlefield,
        );
        let spell_id = ObjectId(71);
        push_spell_with_cast_origin(&mut state, spell_id, opponent, Zone::Exile);

        let mut trigger = make_trigger(TriggerMode::SpellCast);
        trigger.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));
        trigger.spell_cast_origin = OriginConstraint::NotEquals(Zone::Hand);

        let event = GameEvent::SpellCast {
            card_id: CardId(100),
            controller: opponent,
            object_id: spell_id,
        };
        assert!(match_spell_cast(&event, &trigger, source, &state));
    }

    /// CR 601.2a — positive-direction shape (Snapcaster-class "whenever you
    /// cast a spell from your graveyard"). `Equals(Graveyard)` fires on
    /// graveyard cast, rejects hand cast.
    #[test]
    fn spell_cast_equals_graveyard_discriminates() {
        let mut state = setup();
        let caster = PlayerId(0);
        let source = create_object(
            &mut state,
            CardId(1),
            caster,
            "Source".to_string(),
            Zone::Battlefield,
        );

        // Graveyard cast → fires.
        let gy_id = ObjectId(80);
        push_spell_with_cast_origin(&mut state, gy_id, caster, Zone::Graveyard);
        let mut trigger = make_trigger(TriggerMode::SpellCast);
        trigger.valid_target = Some(TargetFilter::Controller);
        trigger.spell_cast_origin = OriginConstraint::Equals(Zone::Graveyard);
        let event = GameEvent::SpellCast {
            card_id: CardId(100),
            controller: caster,
            object_id: gy_id,
        };
        assert!(match_spell_cast(&event, &trigger, source, &state));

        // Hand cast → does not fire.
        let hand_id = ObjectId(81);
        push_spell_with_cast_origin(&mut state, hand_id, caster, Zone::Hand);
        let event = GameEvent::SpellCast {
            card_id: CardId(100),
            controller: caster,
            object_id: hand_id,
        };
        assert!(!match_spell_cast(&event, &trigger, source, &state));
    }

    /// CR 707.10 — a copy is not cast and has no cast origin. A SpellCopy /
    /// SpellCastOrCopy trigger with a non-Any cast-origin constraint must
    /// reject the SpellCopied event.
    #[test]
    fn spell_copy_rejected_when_origin_constraint_is_set() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::SpellCastOrCopy);
        trigger.spell_cast_origin = OriginConstraint::Equals(Zone::Graveyard);

        let event = GameEvent::SpellCopied {
            card_id: CardId(10),
            controller: PlayerId(0),
            object_id: ObjectId(10),
            original_id: ObjectId(10),
        };
        assert!(!match_spell_cast(&event, &trigger, ObjectId(1), &state));
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
    fn blocks_trigger_events_split_per_blocked_attacker() {
        let mut state = setup();
        let blocker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Loyal Sentry".to_string(),
            Zone::Battlefield,
        );
        let first_attacker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "First Attacker".to_string(),
            Zone::Battlefield,
        );
        let second_attacker = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Second Attacker".to_string(),
            Zone::Battlefield,
        );
        let trigger = make_trigger(TriggerMode::Blocks).valid_card(TargetFilter::SelfRef);
        let event = GameEvent::BlockersDeclared {
            assignments: vec![(blocker, first_attacker), (blocker, second_attacker)],
        };

        let matched = matching_block_events(&event, &trigger, blocker, &state);

        assert_eq!(matched.len(), 2);
        assert_eq!(
            matched,
            vec![
                GameEvent::BlockersDeclared {
                    assignments: vec![(blocker, first_attacker)]
                },
                GameEvent::BlockersDeclared {
                    assignments: vec![(blocker, second_attacker)]
                },
            ]
        );
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
    fn attacker_unblocked_uses_sticky_combat_blocked_state() {
        let mut state = setup();
        let attacker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        let mut attacker_info =
            crate::game::combat::AttackerInfo::attacking_player(attacker, PlayerId(1));
        attacker_info.blocked = true;
        state.combat = Some(crate::game::combat::CombatState {
            attackers: vec![attacker_info],
            ..Default::default()
        });

        let event = GameEvent::BlockersDeclared {
            assignments: vec![],
        };
        let trigger = make_trigger(TriggerMode::AttackerUnblocked);
        assert!(!match_attacker_unblocked(
            &event, &trigger, attacker, &state
        ));
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
    fn taps_for_mana_matches_tapped_for_mana() {
        let state = setup();
        let source = ObjectId(5);
        let event = GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: source,
            produced: vec![crate::types::mana::ManaType::Green],
            tap_state: ManaTapState::FromTap,
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

        let event = GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: enchanted_land,
            produced: vec![crate::types::mana::ManaType::Green],
            tap_state: ManaTapState::FromTap,
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

        let event = GameEvent::TappedForMana {
            player_id: PlayerId(1),
            source_id: tapped_land,
            produced: vec![crate::types::mana::ManaType::Green],
            tap_state: ManaTapState::FromTap,
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
        // Mana produced by a triggered ability effect, not a mana ability
        // activation, emits `ManaAdded` (per-unit pool accounting) but never
        // `TappedForMana` — so the matcher must not fire on it.
        let event = GameEvent::ManaAdded {
            player_id: PlayerId(0),
            mana_type: crate::types::mana::ManaType::Green,
            source_id: source,
            tap_state: ManaTapState::NotFromTap,
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
            nth_in_turn: 1,
            nth_in_step: 1,
        };
        assert!(match_drawn(&opponent_event, &trigger, source, &state));

        let controller_event = GameEvent::CardDrawn {
            player_id: PlayerId(0),
            object_id: ObjectId(21),
            nth_in_turn: 1,
            nth_in_step: 1,
        };
        assert!(!match_drawn(&controller_event, &trigger, source, &state));
    }

    #[test]
    fn shuffled_matches_player_performed_action_event() {
        let state = setup();
        let event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(0),
            action: PlayerActionKind::ShuffledLibrary,
        };
        let trigger = make_trigger(TriggerMode::Shuffled);
        assert!(match_shuffled(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn shuffled_rejects_opponent_when_valid_target_is_controller() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Cosis Trickster".to_string(),
            Zone::Battlefield,
        );
        // "Whenever an opponent shuffles" — valid_target filters for opponent
        let mut trigger = make_trigger(TriggerMode::Shuffled);
        trigger.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(crate::types::ability::ControllerRef::Opponent),
        ));

        // Opponent shuffles — should fire
        let opp_event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(1),
            action: PlayerActionKind::ShuffledLibrary,
        };
        assert!(match_shuffled(&opp_event, &trigger, source, &state));

        // Controller shuffles — should NOT fire
        let self_event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(0),
            action: PlayerActionKind::ShuffledLibrary,
        };
        assert!(!match_shuffled(&self_event, &trigger, source, &state));
    }

    #[test]
    fn shuffled_rejects_effect_resolved_event() {
        let state = setup();
        // The old EffectResolved event should no longer trigger match_shuffled
        let event = GameEvent::EffectResolved {
            kind: EffectKind::Shuffle,
            source_id: ObjectId(1),
        };
        let trigger = make_trigger(TriggerMode::Shuffled);
        assert!(!match_shuffled(&event, &trigger, ObjectId(1), &state));
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
    fn pay_echo_is_promoted_to_real_matcher() {
        let registry = build_trigger_registry();
        assert!(trigger_matcher(TriggerMode::PayEcho).is_some());
        assert!(registry.contains_key(&TriggerMode::PayEcho));
    }

    #[test]
    fn pay_cumulative_upkeep_matcher_registered() {
        let registry = build_trigger_registry();
        assert!(trigger_matcher(TriggerMode::PayCumulativeUpkeep).is_some());
        assert!(registry.contains_key(&TriggerMode::PayCumulativeUpkeep));
    }

    #[test]
    fn phase_trigger_valid_target_scopes_active_player() {
        let mut state = setup();
        let aura = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Paradox Haze".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Player(PlayerId(1)));
        let mut trigger = make_trigger(TriggerMode::Phase);
        trigger.phase = Some(crate::types::phase::Phase::Upkeep);
        trigger.valid_target = Some(TargetFilter::AttachedTo);
        let event = GameEvent::PhaseChanged {
            phase: crate::types::phase::Phase::Upkeep,
        };

        state.active_player = PlayerId(0);
        assert!(!match_phase(&event, &trigger, aura, &state));

        state.active_player = PlayerId(1);
        assert!(match_phase(&event, &trigger, aura, &state));
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
        // "whenever you commit a crime" → valid_target = Controller
        let trigger = make_trigger(TriggerMode::CommitCrime).valid_target(TargetFilter::Controller);

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

        // Opponent committed the crime; controller-scoped trigger must not fire.
        let event = GameEvent::CrimeCommitted {
            player_id: PlayerId(1),
        };
        // "whenever you commit a crime" → valid_target = Controller
        let trigger = make_trigger(TriggerMode::CommitCrime).valid_target(TargetFilter::Controller);

        assert!(!match_commit_crime(&event, &trigger, obj_id, &state));
    }

    #[test]
    fn commit_crime_matcher_opponent_scope_fires_for_opponent() {
        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Patrolling Peacemaker".to_string(),
            Zone::Battlefield,
        );

        // Opponent (PlayerId(1)) commits the crime — should fire.
        let event = GameEvent::CrimeCommitted {
            player_id: PlayerId(1),
        };
        // "whenever an opponent commits a crime" → valid_target = Typed(Opponent)
        let trigger = make_trigger(TriggerMode::CommitCrime).valid_target(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));

        assert!(match_commit_crime(&event, &trigger, obj_id, &state));
    }

    #[test]
    fn commit_crime_matcher_opponent_scope_ignores_controller_crime() {
        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Patrolling Peacemaker".to_string(),
            Zone::Battlefield,
        );

        // Controller (PlayerId(0)) commits — opponent-scoped trigger must NOT fire.
        let event = GameEvent::CrimeCommitted {
            player_id: PlayerId(0),
        };
        let trigger = make_trigger(TriggerMode::CommitCrime).valid_target(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));

        assert!(!match_commit_crime(&event, &trigger, obj_id, &state));
    }

    #[test]
    fn commit_crime_matcher_any_player_scope_fires_for_either() {
        let mut state = setup();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tarnation".to_string(),
            Zone::Battlefield,
        );

        // "whenever a player commits a crime" → valid_target = Player (any player)
        let trigger = make_trigger(TriggerMode::CommitCrime).valid_target(TargetFilter::Player);

        let own_crime = GameEvent::CrimeCommitted {
            player_id: PlayerId(0),
        };
        let opp_crime = GameEvent::CrimeCommitted {
            player_id: PlayerId(1),
        };

        assert!(match_commit_crime(&own_crime, &trigger, obj_id, &state));
        assert!(match_commit_crime(&opp_crime, &trigger, obj_id, &state));
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

    fn setup_with_spell_on_stack(is_aura_spell: bool) -> (GameState, ObjectId) {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            if is_aura_spell {
                "Pacifism".to_string()
            } else {
                "Lightning Bolt".to_string()
            },
            Zone::Stack,
        );
        if is_aura_spell {
            if let Some(spell_obj) = state.objects.get_mut(&spell_id) {
                spell_obj.card_types.core_types.push(CoreType::Enchantment);
                spell_obj.card_types.subtypes.push("Aura".to_string());
            }
        }
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

    fn aura_stack_spell_filter() -> TargetFilter {
        TargetFilter::And {
            filters: vec![
                TargetFilter::StackSpell,
                TargetFilter::Typed(TypedFilter::default().subtype("Aura".to_string())),
            ],
        }
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
        let (state, spell_id) = setup_with_spell_on_stack(false);
        // trigger_owner is the permanent with the trigger (e.g. Bonecrusher Giant)
        let trigger_owner = ObjectId(5);
        let mut trigger = make_trigger(TriggerMode::BecomesTarget);
        trigger.valid_source = Some(TargetFilter::StackSpell);

        // Event: trigger_owner becomes the target of spell_id
        let event = GameEvent::BecomesTarget {
            target: TargetRef::Object(trigger_owner),
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
            target: TargetRef::Object(trigger_owner),
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
            target: TargetRef::Object(trigger_owner),
            source_id: ability_id,
        };
        assert!(match_becomes_target(
            &event,
            &trigger,
            trigger_owner,
            &state
        ));
    }

    #[test]
    fn becomes_target_player_matches_valid_target_controller() {
        let (mut state, spell_id) = setup_with_spell_on_stack(false);
        let trigger_owner = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Player Target Observer".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::BecomesTarget);
        trigger.valid_target = Some(TargetFilter::Controller);
        trigger.valid_source = Some(TargetFilter::StackSpell);

        let event = GameEvent::BecomesTarget {
            target: TargetRef::Player(PlayerId(0)),
            source_id: spell_id,
        };

        assert!(match_becomes_target(
            &event,
            &trigger,
            trigger_owner,
            &state
        ));
    }

    #[test]
    fn becomes_target_player_rejects_wrong_player() {
        let (mut state, spell_id) = setup_with_spell_on_stack(false);
        let trigger_owner = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Player Target Observer".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::BecomesTarget);
        trigger.valid_target = Some(TargetFilter::Controller);
        trigger.valid_source = Some(TargetFilter::StackSpell);

        let event = GameEvent::BecomesTarget {
            target: TargetRef::Player(PlayerId(1)),
            source_id: spell_id,
        };

        assert!(!match_becomes_target(
            &event,
            &trigger,
            trigger_owner,
            &state
        ));
    }

    #[test]
    fn becomes_target_player_rejects_object_subject_shape() {
        let (mut state, spell_id) = setup_with_spell_on_stack(false);
        let trigger_owner = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Player Target Observer".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::BecomesTarget);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.valid_source = Some(TargetFilter::StackSpell);

        let event = GameEvent::BecomesTarget {
            target: TargetRef::Player(PlayerId(0)),
            source_id: spell_id,
        };

        assert!(!match_becomes_target(
            &event,
            &trigger,
            trigger_owner,
            &state
        ));
    }

    #[test]
    fn becomes_target_aura_spell_filter_matches_aura_spell() {
        let (state, spell_id) = setup_with_spell_on_stack(true);
        let trigger_owner = ObjectId(5);
        let mut trigger = make_trigger(TriggerMode::BecomesTarget);
        trigger.valid_source = Some(aura_stack_spell_filter());

        let event = GameEvent::BecomesTarget {
            target: TargetRef::Object(trigger_owner),
            source_id: spell_id,
        };
        assert!(match_becomes_target(
            &event,
            &trigger,
            trigger_owner,
            &state
        ));
    }

    #[test]
    fn becomes_target_aura_spell_filter_rejects_non_aura_spell() {
        let (state, spell_id) = setup_with_spell_on_stack(false);
        let trigger_owner = ObjectId(5);
        let mut trigger = make_trigger(TriggerMode::BecomesTarget);
        trigger.valid_source = Some(aura_stack_spell_filter());

        let event = GameEvent::BecomesTarget {
            target: TargetRef::Object(trigger_owner),
            source_id: spell_id,
        };
        assert!(!match_becomes_target(
            &event,
            &trigger,
            trigger_owner,
            &state
        ));
    }

    #[test]
    fn becomes_target_aura_spell_filter_rejects_ability_source() {
        let (state, ability_id) = setup_with_ability_on_stack();
        let trigger_owner = ObjectId(5);
        let mut trigger = make_trigger(TriggerMode::BecomesTarget);
        trigger.valid_source = Some(aura_stack_spell_filter());

        let event = GameEvent::BecomesTarget {
            target: TargetRef::Object(trigger_owner),
            source_id: ability_id,
        };
        assert!(!match_becomes_target(
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
    fn damage_received_noncombat_only_rejects_combat() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Damage Receiver".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::DamageReceived);
        trigger.damage_kind = DamageKindFilter::NoncombatOnly;
        trigger.valid_target = Some(TargetFilter::Controller);

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(99),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: true,
            excess: 0,
        };

        assert!(!match_damage_received(&event, &trigger, source_id, &state));
    }

    #[test]
    fn damage_received_noncombat_only_accepts_noncombat() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Damage Receiver".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::DamageReceived);
        trigger.damage_kind = DamageKindFilter::NoncombatOnly;
        trigger.valid_target = Some(TargetFilter::Controller);

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(99),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            excess: 0,
        };

        assert!(match_damage_received(&event, &trigger, source_id, &state));
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

    // ── damage_amount threshold (CR 603.2 + CR 120.1) ─────────────
    //
    // Building-block tests: the matcher must apply the optional
    // `(Comparator, threshold)` filter to the `DamageDealt` event's `amount`
    // independently of the source/target/damage-kind axes. Exercises the
    // common `GE` comparator (covers Dragonborn Champion's "5 or more" form)
    // plus the orthogonal `EQ` comparator to prove the field is a true
    // comparator slot, not a hard-coded GE check.
    #[test]
    fn damage_amount_ge_threshold_rejects_below() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::DamageDone);
        trigger.damage_amount = Some((Comparator::GE, 5));

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 4,
            is_combat: false,
            excess: 0,
        };
        assert!(!match_damage_done(&event, &trigger, ObjectId(1), &state));
    }

    #[test]
    fn damage_amount_ge_threshold_accepts_at_or_above() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::DamageDone);
        trigger.damage_amount = Some((Comparator::GE, 5));

        for amount in [5, 7, 100] {
            let event = GameEvent::DamageDealt {
                source_id: ObjectId(1),
                target: TargetRef::Player(PlayerId(0)),
                amount,
                is_combat: false,
                excess: 0,
            };
            assert!(
                match_damage_done(&event, &trigger, ObjectId(1), &state),
                "expected amount={amount} to satisfy GE 5"
            );
        }
    }

    #[test]
    fn damage_amount_none_passes_any_amount() {
        let state = setup();
        let trigger = make_trigger(TriggerMode::DamageDone);
        assert_eq!(trigger.damage_amount, None);

        for amount in [0, 1, 99] {
            let event = GameEvent::DamageDealt {
                source_id: ObjectId(1),
                target: TargetRef::Player(PlayerId(0)),
                amount,
                is_combat: false,
                excess: 0,
            };
            assert!(match_damage_done(&event, &trigger, ObjectId(1), &state));
        }
    }

    // CR 603.2 + CR 120.1: `match_damage_received` must apply the same
    // `damage_amount` threshold as `match_damage_done` so the field's
    // semantics is uniform across damage-event matchers. Without this gate, a
    // future "Whenever ~ is dealt N or more damage" trigger would silently
    // drop its threshold.
    #[test]
    fn damage_received_amount_ge_threshold_rejects_below_and_accepts_at_or_above() {
        let mut state = setup();
        // The DamageReceived matcher checks the *target* against `source_id`,
        // so the trigger's source object must equal the damage target.
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            String::new(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::DamageReceived);
        trigger.damage_amount = Some((Comparator::GE, 3));

        for (amount, expect) in [(2u32, false), (3, true), (10, true)] {
            let event = GameEvent::DamageDealt {
                source_id: ObjectId(99),
                target: TargetRef::Object(source_id),
                amount,
                is_combat: false,
                excess: 0,
            };
            assert_eq!(
                match_damage_received(&event, &trigger, source_id, &state),
                expect,
                "amount={amount} GE 3"
            );
        }
    }

    #[test]
    fn damage_received_object_target_rejects_damage_to_other_objects() {
        let mut state = setup();
        let obliterator = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Phyrexian Obliterator".to_string(),
            Zone::Battlefield,
        );
        let other_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other Creature".to_string(),
            Zone::Battlefield,
        );
        let damage_source = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Damage Source".to_string(),
            Zone::Battlefield,
        );
        let trigger = make_trigger(TriggerMode::DamageReceived);

        let unrelated_damage = GameEvent::DamageDealt {
            source_id: damage_source,
            target: TargetRef::Object(other_creature),
            amount: 3,
            is_combat: false,
            excess: 0,
        };
        assert!(
            !match_damage_received(&unrelated_damage, &trigger, obliterator, &state),
            "Obliterator-style DamageReceived triggers must ignore damage to other objects"
        );

        let self_damage = GameEvent::DamageDealt {
            source_id: damage_source,
            target: TargetRef::Object(obliterator),
            amount: 3,
            is_combat: false,
            excess: 0,
        };
        assert!(match_damage_received(
            &self_damage,
            &trigger,
            obliterator,
            &state
        ));
    }

    /// CR 120.1: match_damage_received fires for player targets when valid_target=Controller
    /// and the opponent's source matches valid_source.
    #[test]
    fn damage_received_player_target_with_opponent_source_filter() {
        let mut state = setup();
        // Trigger source = Farsight Mask (controller = P0)
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Farsight Mask".to_string(),
            Zone::Battlefield,
        );
        // Opponent's source (P1 controls this creature)
        let opp_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );

        let mut trigger = make_trigger(TriggerMode::DamageReceived);
        trigger.valid_target = Some(TargetFilter::Controller); // "deals damage to you"
        trigger.valid_source = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        )); // "a source an opponent controls"

        // Opponent's source (P1) deals damage to you (P0) — fires.
        let event = GameEvent::DamageDealt {
            source_id: opp_source,
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: true,
            excess: 0,
        };
        assert!(
            match_damage_received(&event, &trigger, source, &state),
            "must fire when opponent source deals damage to controller"
        );

        // Your own source (P0) deals damage to you (P0) — must NOT fire (source is not opponent).
        let own_source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "OwnCreature".to_string(),
            Zone::Battlefield,
        );
        let event2 = GameEvent::DamageDealt {
            source_id: own_source,
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: true,
            excess: 0,
        };
        assert!(
            !match_damage_received(&event2, &trigger, source, &state),
            "must not fire when own source deals damage"
        );

        // Opponent source deals damage to opponent (P1) — must NOT fire (wrong player).
        let event3 = GameEvent::DamageDealt {
            source_id: opp_source,
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            excess: 0,
        };
        assert!(
            !match_damage_received(&event3, &trigger, source, &state),
            "must not fire when opponent is damaged, not controller"
        );
    }

    /// CR 120.1: source filters also apply when the damaged target is the
    /// trigger source object, matching the player-target branch.
    #[test]
    fn damage_received_object_target_respects_source_filter() {
        let mut state = setup();
        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Phyrexian Obliterator".to_string(),
            Zone::Battlefield,
        );
        let opp_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        let own_source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Own Creature".to_string(),
            Zone::Battlefield,
        );

        let mut trigger = make_trigger(TriggerMode::DamageReceived);
        trigger.valid_source = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));

        let event = GameEvent::DamageDealt {
            source_id: opp_source,
            target: TargetRef::Object(target),
            amount: 3,
            is_combat: false,
            excess: 0,
        };
        assert!(match_damage_received(&event, &trigger, target, &state));

        let own_event = GameEvent::DamageDealt {
            source_id: own_source,
            target: TargetRef::Object(target),
            amount: 3,
            is_combat: false,
            excess: 0,
        };
        assert!(!match_damage_received(&own_event, &trigger, target, &state));
    }

    #[test]
    fn damage_amount_eq_threshold_only_matches_exact() {
        let state = setup();
        let mut trigger = make_trigger(TriggerMode::DamageDone);
        trigger.damage_amount = Some((Comparator::EQ, 3));

        for (amount, expect) in [(2, false), (3, true), (4, false)] {
            let event = GameEvent::DamageDealt {
                source_id: ObjectId(1),
                target: TargetRef::Player(PlayerId(0)),
                amount,
                is_combat: false,
                excess: 0,
            };
            assert_eq!(
                match_damage_done(&event, &trigger, ObjectId(1), &state),
                expect,
                "amount={amount} EQ 3"
            );
        }
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
    fn explored_trigger_filters_exploring_creature_controller() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(340),
            PlayerId(0),
            "Wildgrowth Walker".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, source_id);
        let controlled_explorer = create_object(
            &mut state,
            CardId(341),
            PlayerId(0),
            "Merfolk Branchwalker".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, controlled_explorer);
        let opponent_explorer = create_object(
            &mut state,
            CardId(342),
            PlayerId(1),
            "Opponent Scout".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, opponent_explorer);
        let trigger = parse_trigger_line(
            "Whenever a creature you control explores, put a +1/+1 counter on this creature and you gain 3 life.",
            "Wildgrowth Walker",
        );

        let controlled_event = GameEvent::EffectResolved {
            kind: EffectKind::Explore,
            source_id: controlled_explorer,
        };
        assert!(match_explored(
            &controlled_event,
            &trigger,
            source_id,
            &state
        ));

        let opponent_event = GameEvent::EffectResolved {
            kind: EffectKind::Explore,
            source_id: opponent_explorer,
        };
        assert!(!match_explored(
            &opponent_event,
            &trigger,
            source_id,
            &state
        ));
    }

    #[test]
    fn become_renowned_trigger_matches_filtered_controlled_creature() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(343),
            PlayerId(0),
            "Valeron Wardens".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, source_id);
        let controlled = create_object(
            &mut state,
            CardId(344),
            PlayerId(0),
            "Renown Ally".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, controlled);
        let opponent = create_object(
            &mut state,
            CardId(345),
            PlayerId(1),
            "Opponent Renown Creature".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, opponent);
        let trigger = parse_trigger_line(
            "Whenever a creature you control becomes renowned, draw a card.",
            "Valeron Wardens",
        );

        let controlled_event = GameEvent::EffectResolved {
            kind: EffectKind::Renown,
            source_id: controlled,
        };
        assert!(match_become_renowned(
            &controlled_event,
            &trigger,
            source_id,
            &state
        ));

        let opponent_event = GameEvent::EffectResolved {
            kind: EffectKind::Renown,
            source_id: opponent,
        };
        assert!(!match_become_renowned(
            &opponent_event,
            &trigger,
            source_id,
            &state
        ));
    }

    #[test]
    fn become_renowned_trigger_defaults_to_self_when_unfiltered() {
        let mut state = setup();
        let source_id = create_object(
            &mut state,
            CardId(346),
            PlayerId(0),
            "Self Renown Watcher".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, source_id);
        let other = create_object(
            &mut state,
            CardId(347),
            PlayerId(0),
            "Other Renown Creature".to_string(),
            Zone::Battlefield,
        );
        make_creature(&mut state, other);
        let trigger = make_trigger(TriggerMode::BecomeRenowned);

        assert!(match_become_renowned(
            &GameEvent::EffectResolved {
                kind: EffectKind::Renown,
                source_id,
            },
            &trigger,
            source_id,
            &state
        ));
        assert!(!match_become_renowned(
            &GameEvent::EffectResolved {
                kind: EffectKind::Renown,
                source_id: other,
            },
            &trigger,
            source_id,
            &state
        ));
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

    /// Issue #311 — Undead Alchemist class. The matcher must consult
    /// `valid_card.controller` together with `origin` so the trigger fires
    /// only when an opponent's creature card moves from library to graveyard
    /// (CR 109.5 + CR 603.6c). The user-reported softlock was the source's
    /// own death (Battlefield → Graveyard, controller=You) erroneously
    /// firing this trigger.
    #[test]
    fn changes_zone_undead_alchemist_excludes_self_battlefield_death() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(311),
            PlayerId(0),
            "Undead Alchemist".to_string(),
            Zone::Battlefield,
        );

        let mut trigger = make_trigger(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Library);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::Opponent),
        ));

        // (a) Source's OWN death (Battlefield → Graveyard, controller=You)
        //     MUST NOT fire. This is the symptom the user reported.
        let self_dying = GameEvent::ZoneChanged {
            object_id: source,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                controller: PlayerId(0),
                owner: PlayerId(0),
                ..ZoneChangeRecord::test_minimal(source, Some(Zone::Battlefield), Zone::Graveyard)
            }),
        };
        assert!(
            !match_changes_zone(&self_dying, &trigger, source, &state),
            "trigger must not fire on the source's own battlefield death"
        );

        // (b) The controller's OWN creature being milled (Library → Graveyard,
        //     controller=You) MUST NOT fire (valid_card.controller=Opponent).
        let own_milled = ObjectId(100);
        let own_milled_event = GameEvent::ZoneChanged {
            object_id: own_milled,
            from: Some(Zone::Library),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                controller: PlayerId(0),
                owner: PlayerId(0),
                ..ZoneChangeRecord::test_minimal(own_milled, Some(Zone::Library), Zone::Graveyard)
            }),
        };
        assert!(
            !match_changes_zone(&own_milled_event, &trigger, source, &state),
            "trigger must not fire on the controller's own milled creature"
        );

        // (c) An opponent's creature dying (Battlefield → Graveyard,
        //     controller=Opponent) MUST NOT fire because the origin is
        //     restricted to Library.
        let opp_dying = ObjectId(101);
        let opp_dying_event = GameEvent::ZoneChanged {
            object_id: opp_dying,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                controller: PlayerId(1),
                owner: PlayerId(1),
                ..ZoneChangeRecord::test_minimal(
                    opp_dying,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            }),
        };
        assert!(
            !match_changes_zone(&opp_dying_event, &trigger, source, &state),
            "trigger must not fire when origin is Battlefield, not Library"
        );

        // (d) An opponent's creature card being milled (Library → Graveyard,
        //     controller=Opponent) — the intended firing condition.
        let opp_milled = ObjectId(102);
        let opp_milled_event = GameEvent::ZoneChanged {
            object_id: opp_milled,
            from: Some(Zone::Library),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                controller: PlayerId(1),
                owner: PlayerId(1),
                ..ZoneChangeRecord::test_minimal(opp_milled, Some(Zone::Library), Zone::Graveyard)
            }),
        };
        assert!(
            match_changes_zone(&opp_milled_event, &trigger, source, &state),
            "trigger must fire when an opponent's creature card is milled"
        );
    }

    /// CR 701.30b-c: match_clash fires when the controller of the trigger
    /// source is either player participating in the clash.
    #[test]
    fn clash_trigger_fires_for_controller() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(701),
            PlayerId(0),
            "Entangling Trap".to_string(),
            Zone::Battlefield,
        );
        let mut trigger = make_trigger(TriggerMode::Clashed);
        trigger.valid_target = Some(TargetFilter::Controller);

        // Controller (P0) initiates the clash — fires.
        let event = GameEvent::Clash {
            controller: PlayerId(0),
            opponent: PlayerId(1),
            controller_mana_value: None,
            opponent_mana_value: None,
            result: crate::types::events::ClashResult::Won,
        };
        assert!(
            match_clash(&event, &trigger, source, &state),
            "clash trigger must fire for controller"
        );

        // Controller (P0) is the chosen opponent and still clashes — fires.
        let event2 = GameEvent::Clash {
            controller: PlayerId(1),
            opponent: PlayerId(0),
            controller_mana_value: None,
            opponent_mana_value: None,
            result: crate::types::events::ClashResult::Won,
        };
        assert!(
            match_clash(&event2, &trigger, source, &state),
            "clash trigger must fire when controller is the opponent participant"
        );
    }

    /// Issue #311 end-to-end: parse the Undead Alchemist trigger line and
    /// confirm the parsed `TriggerDefinition` rejects the source's own
    /// battlefield death. Tightens the regression net by exercising the
    /// parse → match pipeline together rather than the matcher in isolation.
    #[test]
    fn undead_alchemist_parsed_trigger_rejects_self_death_end_to_end() {
        let trigger = parse_trigger_line(
            "Whenever a creature card is put into an opponent's graveyard from their library, exile that card.",
            "Undead Alchemist",
        );

        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(311),
            PlayerId(0),
            "Undead Alchemist".to_string(),
            Zone::Battlefield,
        );

        // Self-death: source going from Battlefield → Graveyard, controller=You.
        let self_dying = GameEvent::ZoneChanged {
            object_id: source,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                controller: PlayerId(0),
                owner: PlayerId(0),
                ..ZoneChangeRecord::test_minimal(source, Some(Zone::Battlefield), Zone::Graveyard)
            }),
        };
        assert!(
            !match_changes_zone(&self_dying, &trigger, source, &state),
            "parsed Undead Alchemist trigger must not fire on its own death"
        );

        // Opponent's creature milled (Library → Graveyard, controller=Opponent) — fires.
        let opp_milled = ObjectId(102);
        let opp_milled_event = GameEvent::ZoneChanged {
            object_id: opp_milled,
            from: Some(Zone::Library),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                controller: PlayerId(1),
                owner: PlayerId(1),
                ..ZoneChangeRecord::test_minimal(opp_milled, Some(Zone::Library), Zone::Graveyard)
            }),
        };
        assert!(
            match_changes_zone(&opp_milled_event, &trigger, source, &state),
            "parsed Undead Alchemist trigger must fire when an opponent's creature is milled"
        );
    }

    /// CR 104.3a: match_loses_game fires when a PlayerLost event is received
    /// and the losing player passes valid_player_matches (or no filter is set).
    #[test]
    fn loses_game_trigger_fires_on_player_lost_event() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Withengar Unbound".to_string(),
            Zone::Battlefield,
        );

        // Unscoped trigger (any player loses) — should fire for any player.
        let mut trigger = make_trigger(TriggerMode::LosesGame);

        let opp_lost = GameEvent::PlayerLost {
            player_id: PlayerId(1),
        };
        assert!(
            match_loses_game(&opp_lost, &trigger, source, &state),
            "unscoped trigger must fire when any player loses"
        );

        let my_lost = GameEvent::PlayerLost {
            player_id: PlayerId(0),
        };
        assert!(
            match_loses_game(&my_lost, &trigger, source, &state),
            "unscoped trigger must fire when controller loses"
        );

        // Non-PlayerLost event must not fire.
        let non_lost = GameEvent::EffectResolved {
            kind: EffectKind::Draw,
            source_id: source,
        };
        assert!(
            !match_loses_game(&non_lost, &trigger, source, &state),
            "trigger must not fire for non-PlayerLost events"
        );

        // Controller-scoped trigger — only fires when controller loses.
        trigger.valid_target = Some(TargetFilter::Controller);
        assert!(
            !match_loses_game(&opp_lost, &trigger, source, &state),
            "controller-scoped trigger must not fire when opponent loses"
        );
        assert!(
            match_loses_game(&my_lost, &trigger, source, &state),
            "controller-scoped trigger must fire when controller loses"
        );
    }

    // -----------------------------------------------------------------------
    // count_trigger_subjects_in_batch — building block for "one or more
    // <FILTER> <verb>" batched-trigger subject counting (issue #707).
    // -----------------------------------------------------------------------

    fn make_dragon(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Dragon".to_string());
        obj.base_card_types = obj.card_types.clone();
        id
    }

    fn make_non_dragon(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Soldier".to_string());
        obj.base_card_types = obj.card_types.clone();
        id
    }

    /// CR 603.2c: `count_trigger_subjects_in_batch` filters
    /// `AttackersDeclared.attacker_ids` against the trigger's `valid_card`
    /// and returns the count — three Dragons among four attackers ⇒ 3.
    #[test]
    fn count_trigger_subjects_filters_attack_batch_by_subtype() {
        let mut state = setup();
        let source = make_dragon(&mut state, PlayerId(0), "Ur-Dragon");
        let d2 = make_dragon(&mut state, PlayerId(0), "Helper A");
        let d3 = make_dragon(&mut state, PlayerId(0), "Helper B");
        let non = make_non_dragon(&mut state, PlayerId(0), "Lowly Soldier");
        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![source, d2, d3, non],
            defending_player: PlayerId(1),
            attacks: vec![],
        };
        let filter = TargetFilter::Typed(
            TypedFilter::card()
                .controller(ControllerRef::You)
                .subtype("Dragon".to_string()),
        );
        let count = count_trigger_subjects_in_batch(
            &state,
            Some(&filter),
            source,
            std::slice::from_ref(&event),
        );
        assert_eq!(count, Some(3));
    }

    /// CR 603.2c: no `valid_card` ⇒ "that many" is undefined; callers fall
    /// back to the existing `EventContextAmount` cascade.
    #[test]
    fn count_trigger_subjects_returns_none_without_filter() {
        let state = setup();
        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![ObjectId(1), ObjectId(2)],
            defending_player: PlayerId(1),
            attacks: vec![],
        };
        let count = count_trigger_subjects_in_batch(
            &state,
            None,
            ObjectId(99),
            std::slice::from_ref(&event),
        );
        assert_eq!(count, None);
    }

    /// CR 603.2c: `SelfRef` is the "this permanent" reference — the trigger
    /// source is its own subject and "that many" degenerates. The caller's
    /// fallback chain (event-amount, then last_effect_count) is the right
    /// path for self-referential batched triggers.
    #[test]
    fn count_trigger_subjects_returns_none_for_self_ref_filter() {
        let state = setup();
        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![ObjectId(1)],
            defending_player: PlayerId(1),
            attacks: vec![],
        };
        let count = count_trigger_subjects_in_batch(
            &state,
            Some(&TargetFilter::SelfRef),
            ObjectId(99),
            std::slice::from_ref(&event),
        );
        assert_eq!(count, None);
    }
}
