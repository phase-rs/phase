use rand::Rng;
use std::collections::{HashSet, VecDeque};
use thiserror::Error;

use crate::types::ability::{EffectKind, KeywordAction, TargetRef};
#[cfg(test)]
use crate::types::ability::{EffectScope, TapStateChange};
use crate::types::actions::{
    GameAction, MayTriggerAutoChoiceOp, PriorityYieldOp, TriggerOrderTemplateOp,
};
use crate::types::events::{BendingType, ContestRound, GameEvent, ManaTapState, PlayerActionKind};
use crate::types::game_state::{
    ActionResult, AssistState, AutoMayChoice, AutoPassMode, AutoPassRequest, CastOfferKind,
    ConvokeMode, CostResume, GameState, LandPlayRecord, LoopDetectionMode, ManaAbilityResume,
    MayTriggerAutoChoiceKey, PayCostKind, PendingCostMoveResume, RetargetScope, StackEntry,
    StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{CardId, DelayedTriggerOrigin, ObjectId};
use crate::types::match_config::MatchType;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
#[cfg(debug_assertions)]
use crate::types::resolution::debug_assert_runtime_resolution_invariants;
use crate::types::resolved_commands::{
    ResolvedInformationAudience, ResolvedInformationEdit, ResolvedInformationLifetime,
    ResolvedOncePerTurnPermission, ResolvedRulesCommand,
};
use crate::types::statics::{CastFrequency, StaticMode};
use crate::types::zones::Zone;

use super::ability_utils::{
    begin_target_selection_for_ability, build_target_slots, cap_distribution_target_slots,
    compute_unavailable_modes, has_legal_target_assignment_for_ability, modal_choice_for_player,
};
use super::casting;
use super::casting_costs;
use super::companion;
use super::crew_payment;
use super::effects;
use super::end_continuous_effect;
use super::engine_casting;
use super::engine_combat;
use super::engine_modes;
use super::engine_payment_choices;
use super::engine_priority;
use super::engine_replacement;
use super::engine_resolution_choices;
use super::engine_stack;
use super::interaction;
use super::keywords;
use super::mana_abilities;
use super::mana_payment;
use super::mana_sources;
use super::match_flow;
use super::morph;
use super::mulligan;
use super::planechase;
use super::planeswalker;
use super::priority;
use super::public_state::{
    bump_state_revision, finalize_display_state, finalize_public_state, finalize_rules_state,
    mark_public_state_all_dirty, mark_public_state_from_events, sync_waiting_for,
};
use super::room;
use super::sba;
use super::splice;
use super::transform;
use super::triggers;
use super::turn_control;
use super::turns;
use super::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};
#[cfg(test)]
use super::zones;

pub use super::engine_resolve_batch::{
    resolve_all_fast_forward, ResolveAllCallbackDecision, ResolveAllFastForwardResult,
};

#[derive(Debug, Clone, Error)]
pub enum EngineError {
    #[error("Invalid action: {0}")]
    InvalidAction(String),
    #[error("Wrong player")]
    WrongPlayer,
    #[error("Not your priority")]
    NotYourPriority,
    #[error("Action not allowed: {0}")]
    ActionNotAllowed(String),
}

/// The three non-interchangeable authorities carried by a live Priority
/// window. This is deliberately restricted to `game`: only engine-owned
/// preflight providers may name it, and only this module can construct it.
pub(in crate::game) struct PriorityPrincipal {
    semantic_holder: PlayerId,
    authenticated_actor: PlayerId,
    land_resource_owner: PlayerId,
}

/// Why an actionless Priority preflight cannot safely proceed. These failures
/// are intentionally distinct from ordinary reducer errors: no mandatory
/// transition may turn an uncertain Priority window into a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::game) enum PriorityPreflightIndeterminate {
    NotPriority,
    PriorityAuthorityMismatch,
    SharedTeamHolderOutsideActiveTeam,
}

impl PriorityPrincipal {
    fn from_priority_window(state: &GameState) -> Result<Self, PriorityPreflightIndeterminate> {
        let semantic_holder = match &state.waiting_for {
            WaitingFor::Priority { player } => *player,
            _ => return Err(PriorityPreflightIndeterminate::NotPriority),
        };
        let authenticated_actor =
            turn_control::authorized_submitter_for_player(state, semantic_holder);
        if state.priority_player != authenticated_actor {
            return Err(PriorityPreflightIndeterminate::PriorityAuthorityMismatch);
        }
        let land_resource_owner = if state.format_config.topology().has_shared_team_turns() {
            if !super::topology::team_members(state, state.active_player).contains(&semantic_holder)
            {
                return Err(PriorityPreflightIndeterminate::SharedTeamHolderOutsideActiveTeam);
            }
            semantic_holder
        } else {
            turn_control::turn_resource_owner(state)
        };
        Ok(Self {
            semantic_holder,
            authenticated_actor,
            land_resource_owner,
        })
    }

    pub(in crate::game) fn semantic_holder(&self) -> PlayerId {
        self.semantic_holder
    }

    pub(in crate::game) fn authenticated_actor(&self) -> PlayerId {
        self.authenticated_actor
    }

    pub(in crate::game) fn land_resource_owner(&self) -> PlayerId {
        self.land_resource_owner
    }
}

/// Build the authoritative Priority principal for the private mandatory
/// transition preflight. Callers receive no fallback identity when a Priority
/// window is stale or absent.
pub(in crate::game) fn priority_principal_for_preflight(
    state: &GameState,
) -> Result<PriorityPrincipal, PriorityPreflightIndeterminate> {
    PriorityPrincipal::from_priority_window(state)
}

/// The closed set of ordinary Priority reducer families that can make a
/// mandatory transition unsafe. `PassPriority` and `SetAutoPass` deliberately
/// have no member: neither proves the holder has a meaningful action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::game) enum PriorityReducerFamily {
    PlayLand,
    TapLandForMana,
    ActivateManaSource,
    UntapLandForMana,
    CastSpell,
    Foretell,
    ActivateAbility,
    UnlockRoomDoor,
    RollPlanarDie,
    Equip,
    CrewVehicle,
    ActivateStation,
    SaddleMount,
    Transform,
    ActivateNinjutsu,
    CastSpellAsSneak,
    CastSpellAsWebSlinging,
    CastSpellForFree,
    PlayFaceDown,
    TurnFaceUp,
    CompanionToHand,
    EndContinuousEffect,
    CastPreparedCopy,
}

impl PriorityReducerFamily {
    const ALL: [Self; 23] = [
        Self::PlayLand,
        Self::TapLandForMana,
        Self::ActivateManaSource,
        Self::UntapLandForMana,
        Self::CastSpell,
        Self::Foretell,
        Self::ActivateAbility,
        Self::UnlockRoomDoor,
        Self::RollPlanarDie,
        Self::Equip,
        Self::CrewVehicle,
        Self::ActivateStation,
        Self::SaddleMount,
        Self::Transform,
        Self::ActivateNinjutsu,
        Self::CastSpellAsSneak,
        Self::CastSpellAsWebSlinging,
        Self::CastSpellForFree,
        Self::PlayFaceDown,
        Self::TurnFaceUp,
        Self::CompanionToHand,
        Self::EndContinuousEffect,
        Self::CastPreparedCopy,
    ];
}

/// The reason a Priority preflight cannot establish that passing is safe.
/// This stays private because mandatory progression only distinguishes
/// `Actionless` from every other outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::game) enum PriorityPreflightBlock {
    Principal(PriorityPreflightIndeterminate),
    RequiresChosenX,
    ReducerRejected,
}

/// The only outcomes a mandatory transition may observe at Priority. Only
/// `Actionless` permits a transition; every other result is an uncharged block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::game) enum PriorityPreflight {
    Actionless,
    Actionable {
        family: PriorityReducerFamily,
    },
    NotPriorityWindow,
    Indeterminate {
        family: Option<PriorityReducerFamily>,
        block: PriorityPreflightBlock,
    },
}

/// Engine-only read capability for the eventual provider-owned announcement
/// facades. It is non-unit and has no cloning or default construction so sibling
/// providers can name it in a borrowed accessor without manufacturing one.
pub(in crate::game) struct PriorityAnnouncementFacadeAccess {
    engine_only: std::marker::PhantomData<fn() -> ()>,
}

impl PriorityAnnouncementFacadeAccess {
    fn new() -> Self {
        Self {
            engine_only: std::marker::PhantomData,
        }
    }
}

/// One typed, engine-authored primer for a normal Priority reducer arm. The
/// enum is intentionally private: it is neither an action protocol nor an AI
/// candidate API, and conversion is confined to the facade below.
enum PriorityAnnouncement {
    PlayLand(casting::PriorityPlayLandAnnouncement),
    TapLandForMana(mana_sources::PriorityLandManaAnnouncement),
    ActivateManaSource(mana_sources::PriorityNonlandManaAnnouncement),
    UntapLandForMana(mana_sources::PriorityUntapLandAnnouncement),
    CastSpell(casting::PriorityCastSpellAnnouncement),
    Foretell(casting::PriorityForetellAnnouncement),
    ActivateAbility(casting::PriorityActivateAbilityAnnouncement),
    UnlockRoomDoor(room::PriorityUnlockRoomDoorAnnouncement),
    RollPlanarDie(planechase::PriorityPlanarDieAnnouncement),
    Equip(effects::attach::PriorityEquipAnnouncement),
    CrewVehicle(crew_payment::PriorityCrewAnnouncement),
    ActivateStation(crew_payment::PriorityStationAnnouncement),
    SaddleMount(crew_payment::PrioritySaddleAnnouncement),
    Transform(transform::PriorityTransformAnnouncement),
    ActivateNinjutsu(keywords::PriorityNinjutsuAnnouncement),
    CastSpellAsSneak(casting::PrioritySneakAnnouncement),
    CastSpellAsWebSlinging(casting::PriorityWebSlingingAnnouncement),
    CastSpellForFree(casting::PriorityCastFreeAnnouncement),
    PlayFaceDown(morph::PriorityPlayFaceDownAnnouncement),
    TurnFaceUp(morph::PriorityTurnFaceUpAnnouncement),
    CompanionToHand(companion::PriorityCompanionAnnouncement),
    EndContinuousEffect(end_continuous_effect::PriorityEndContinuousEffectAnnouncement),
    CastPreparedCopy(effects::prepare::PriorityPreparedCopyAnnouncement),
}

impl PriorityAnnouncement {
    fn family(&self) -> PriorityReducerFamily {
        match self {
            Self::PlayLand(_) => PriorityReducerFamily::PlayLand,
            Self::TapLandForMana(_) => PriorityReducerFamily::TapLandForMana,
            Self::ActivateManaSource(_) => PriorityReducerFamily::ActivateManaSource,
            Self::UntapLandForMana(_) => PriorityReducerFamily::UntapLandForMana,
            Self::CastSpell(_) => PriorityReducerFamily::CastSpell,
            Self::Foretell(_) => PriorityReducerFamily::Foretell,
            Self::ActivateAbility(_) => PriorityReducerFamily::ActivateAbility,
            Self::UnlockRoomDoor(_) => PriorityReducerFamily::UnlockRoomDoor,
            Self::RollPlanarDie(_) => PriorityReducerFamily::RollPlanarDie,
            Self::Equip(_) => PriorityReducerFamily::Equip,
            Self::CrewVehicle(_) => PriorityReducerFamily::CrewVehicle,
            Self::ActivateStation(_) => PriorityReducerFamily::ActivateStation,
            Self::SaddleMount(_) => PriorityReducerFamily::SaddleMount,
            Self::Transform(_) => PriorityReducerFamily::Transform,
            Self::ActivateNinjutsu(_) => PriorityReducerFamily::ActivateNinjutsu,
            Self::CastSpellAsSneak(_) => PriorityReducerFamily::CastSpellAsSneak,
            Self::CastSpellAsWebSlinging(_) => PriorityReducerFamily::CastSpellAsWebSlinging,
            Self::CastSpellForFree(_) => PriorityReducerFamily::CastSpellForFree,
            Self::PlayFaceDown(_) => PriorityReducerFamily::PlayFaceDown,
            Self::TurnFaceUp(_) => PriorityReducerFamily::TurnFaceUp,
            Self::CompanionToHand(_) => PriorityReducerFamily::CompanionToHand,
            Self::EndContinuousEffect(_) => PriorityReducerFamily::EndContinuousEffect,
            Self::CastPreparedCopy(_) => PriorityReducerFamily::CastPreparedCopy,
        }
    }
}

/// The sole Priority-announcement conversion path. It creates a local access
/// capability before exhaustively rebuilding the ordinary reducer action.
fn priority_announcement_to_action(announcement: PriorityAnnouncement) -> GameAction {
    let _access = PriorityAnnouncementFacadeAccess::new();
    match announcement {
        PriorityAnnouncement::PlayLand(announcement) => GameAction::PlayLand {
            object_id: announcement.object_id(&_access),
            card_id: announcement.card_id(&_access),
        },
        PriorityAnnouncement::TapLandForMana(announcement) => GameAction::TapLandForMana {
            selection: announcement.selection(&_access).clone(),
        },
        PriorityAnnouncement::ActivateManaSource(announcement) => GameAction::ActivateManaSource {
            selection: announcement.selection(&_access).clone(),
        },
        PriorityAnnouncement::UntapLandForMana(announcement) => GameAction::UntapLandForMana {
            object_id: announcement.object_id(&_access),
        },
        PriorityAnnouncement::CastSpell(announcement) => GameAction::CastSpell {
            object_id: announcement.object_id(&_access),
            card_id: announcement.card_id(&_access),
            targets: Vec::new(),
            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
        PriorityAnnouncement::Foretell(announcement) => GameAction::Foretell {
            object_id: announcement.object_id(&_access),
            card_id: announcement.card_id(&_access),
        },
        PriorityAnnouncement::ActivateAbility(announcement) => GameAction::ActivateAbility {
            source_id: announcement.source_id(&_access),
            ability_index: announcement.ability_index(&_access),
        },
        PriorityAnnouncement::UnlockRoomDoor(announcement) => GameAction::UnlockRoomDoor {
            object_id: announcement.object_id(&_access),
            door: announcement.door(&_access),
        },
        PriorityAnnouncement::RollPlanarDie(_) => GameAction::RollPlanarDie,
        PriorityAnnouncement::Equip(announcement) => {
            let equipment_id = announcement.equipment_id(&_access);
            GameAction::Equip {
                equipment_id,
                // The Priority reducer ignores this field and enters its normal
                // target-selection flow when the choice is not forced.
                target_id: equipment_id,
            }
        }
        PriorityAnnouncement::CrewVehicle(announcement) => GameAction::CrewVehicle {
            vehicle_id: announcement.vehicle_id(&_access),
            creature_ids: Vec::new(),
        },
        PriorityAnnouncement::ActivateStation(announcement) => GameAction::ActivateStation {
            spacecraft_id: announcement.spacecraft_id(&_access),
            creature_id: None,
        },
        PriorityAnnouncement::SaddleMount(announcement) => GameAction::SaddleMount {
            mount_id: announcement.mount_id(&_access),
            creature_ids: Vec::new(),
        },
        PriorityAnnouncement::Transform(announcement) => GameAction::Transform {
            object_id: announcement.object_id(&_access),
        },
        PriorityAnnouncement::ActivateNinjutsu(announcement) => GameAction::ActivateNinjutsu {
            ninjutsu_object_id: announcement.ninjutsu_object_id(&_access),
            creature_to_return: announcement.creature_to_return(&_access),
        },
        PriorityAnnouncement::CastSpellAsSneak(announcement) => GameAction::CastSpellAsSneak {
            hand_object: announcement.hand_object(&_access),
            card_id: announcement.card_id(&_access),
            creature_to_return: announcement.creature_to_return(&_access),
            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
        PriorityAnnouncement::CastSpellAsWebSlinging(announcement) => {
            GameAction::CastSpellAsWebSlinging {
                hand_object: announcement.hand_object(&_access),
                card_id: announcement.card_id(&_access),
                creature_to_return: announcement.creature_to_return(&_access),
                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            }
        }
        PriorityAnnouncement::CastSpellForFree(announcement) => GameAction::CastSpellForFree {
            object_id: announcement.object_id(&_access),
            card_id: announcement.card_id(&_access),
            source_id: announcement.source_id(&_access),
            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
        PriorityAnnouncement::PlayFaceDown(announcement) => GameAction::PlayFaceDown {
            object_id: announcement.object_id(&_access),
            card_id: announcement.card_id(&_access),
        },
        PriorityAnnouncement::TurnFaceUp(announcement) => GameAction::TurnFaceUp {
            object_id: announcement.object_id(&_access),
            x: 0,
        },
        PriorityAnnouncement::CompanionToHand(_) => GameAction::CompanionToHand,
        PriorityAnnouncement::EndContinuousEffect(announcement) => {
            GameAction::EndContinuousEffect {
                group: announcement.group(&_access),
                source_name: announcement.source_name(&_access).to_string(),
                cost: announcement.cost(&_access).clone(),
            }
        }
        PriorityAnnouncement::CastPreparedCopy(announcement) => GameAction::CastPreparedCopy {
            source: announcement.source_id(&_access),
        },
    }
}

fn apply_priority_announcement(
    state: &GameState,
    principal: &PriorityPrincipal,
    announcement: PriorityAnnouncement,
) -> Result<ActionResult, EngineError> {
    let mut projected = state.clone();
    let action = priority_announcement_to_action(announcement);
    apply_interaction_for_simulation(
        &mut projected,
        principal.authenticated_actor(),
        principal.semantic_holder(),
        action,
    )
}

enum PriorityPreflightCandidate {
    Announcement(PriorityAnnouncement),
    Indeterminate {
        family: PriorityReducerFamily,
        block: PriorityPreflightBlock,
    },
}

impl PriorityPreflightCandidate {
    fn family(&self) -> PriorityReducerFamily {
        match self {
            Self::Announcement(announcement) => announcement.family(),
            Self::Indeterminate { family, .. } => *family,
        }
    }
}

/// Enumerate only reducer-shaped, finite Priority primers from the engine's
/// existing legality authorities. This intentionally never reaches into
/// `ai_support`: tactical candidates are neither complete nor an authority for
/// mandatory progress.
fn priority_preflight_candidates(
    state: &GameState,
    principal: &PriorityPrincipal,
) -> Vec<PriorityPreflightCandidate> {
    let semantic_holder = principal.semantic_holder();
    let mut candidates = Vec::new();
    let split_second_active = super::keywords::stack_has_split_second(state);
    let is_active = state.active_player == semantic_holder;
    candidates.extend(
        casting::priority_play_land_announcements(state, principal)
            .into_iter()
            .map(PriorityAnnouncement::PlayLand)
            .map(PriorityPreflightCandidate::Announcement),
    );

    let (land_mana_announcements, nonland_mana_announcements) =
        mana_sources::priority_mana_announcements(state, principal).into_partitioned();
    candidates.extend(
        land_mana_announcements
            .into_iter()
            .map(PriorityAnnouncement::TapLandForMana)
            .map(PriorityPreflightCandidate::Announcement),
    );
    candidates.extend(
        nonland_mana_announcements
            .into_iter()
            .map(PriorityAnnouncement::ActivateManaSource)
            .map(PriorityPreflightCandidate::Announcement),
    );
    candidates.extend(
        mana_sources::priority_untap_land_announcements(state, principal)
            .into_iter()
            .map(PriorityAnnouncement::UntapLandForMana)
            .map(PriorityPreflightCandidate::Announcement),
    );

    if !split_second_active {
        candidates.extend(
            casting::priority_cast_spell_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::CastSpell)
                .map(PriorityPreflightCandidate::Announcement),
        );
        candidates.extend(
            casting::priority_cast_free_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::CastSpellForFree)
                .map(PriorityPreflightCandidate::Announcement),
        );

        candidates.extend(
            casting::priority_activate_ability_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::ActivateAbility)
                .map(PriorityPreflightCandidate::Announcement),
        );

        candidates.extend(
            effects::attach::priority_equip_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::Equip)
                .map(PriorityPreflightCandidate::Announcement),
        );

        candidates.extend(
            transform::priority_transform_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::Transform)
                .map(PriorityPreflightCandidate::Announcement),
        );

        candidates.extend(
            effects::prepare::priority_prepared_copy_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::CastPreparedCopy)
                .map(PriorityPreflightCandidate::Announcement),
        );

        let (crew_announcements, station_announcements, saddle_announcements) =
            crew_payment::priority_tap_payment_announcements(state, principal).into_partitioned();
        candidates.extend(
            crew_announcements
                .into_iter()
                .map(PriorityAnnouncement::CrewVehicle)
                .map(PriorityPreflightCandidate::Announcement),
        );
        candidates.extend(
            station_announcements
                .into_iter()
                .map(PriorityAnnouncement::ActivateStation)
                .map(PriorityPreflightCandidate::Announcement),
        );
        candidates.extend(
            saddle_announcements
                .into_iter()
                .map(PriorityAnnouncement::SaddleMount)
                .map(PriorityPreflightCandidate::Announcement),
        );

        candidates.extend(
            room::priority_unlock_room_door_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::UnlockRoomDoor)
                .map(PriorityPreflightCandidate::Announcement),
        );

        candidates.extend(
            keywords::priority_ninjutsu_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::ActivateNinjutsu)
                .map(PriorityPreflightCandidate::Announcement),
        );

        candidates.extend(
            casting::priority_sneak_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::CastSpellAsSneak)
                .map(PriorityPreflightCandidate::Announcement),
        );

        candidates.extend(
            casting::priority_web_slinging_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::CastSpellAsWebSlinging)
                .map(PriorityPreflightCandidate::Announcement),
        );
    }

    if is_active {
        candidates.extend(
            casting::priority_foretell_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::Foretell)
                .map(PriorityPreflightCandidate::Announcement),
        );
        candidates.extend(
            morph::priority_play_face_down_announcements(state, principal)
                .into_iter()
                .map(PriorityAnnouncement::PlayFaceDown)
                .map(PriorityPreflightCandidate::Announcement),
        );
    }

    candidates.extend(
        morph::priority_turn_face_up_candidates(state, principal)
            .into_iter()
            .map(|candidate| match candidate {
                morph::PriorityTurnFaceUpCandidate::Ready(announcement) => {
                    PriorityPreflightCandidate::Announcement(PriorityAnnouncement::TurnFaceUp(
                        announcement,
                    ))
                }
                morph::PriorityTurnFaceUpCandidate::RequiresChosenX => {
                    PriorityPreflightCandidate::Indeterminate {
                        family: PriorityReducerFamily::TurnFaceUp,
                        block: PriorityPreflightBlock::RequiresChosenX,
                    }
                }
            }),
    );

    if let Some(announcement) = companion::priority_companion_announcement(state, principal) {
        candidates.push(PriorityPreflightCandidate::Announcement(
            PriorityAnnouncement::CompanionToHand(announcement),
        ));
    }
    candidates.extend(
        end_continuous_effect::priority_end_continuous_effect_announcements(state, principal)
            .into_iter()
            .map(PriorityAnnouncement::EndContinuousEffect)
            .map(PriorityPreflightCandidate::Announcement),
    );
    if let Some(announcement) = planechase::priority_planar_die_announcement(state, principal) {
        candidates.push(PriorityPreflightCandidate::Announcement(
            PriorityAnnouncement::RollPlanarDie(announcement),
        ));
    }
    candidates
}

/// Probe every finite Priority primer through exactly one normal interaction
/// boundary on an isolated clone. A reducer rejection is not interpreted as a
/// pass: it remains an indeterminate mandatory-transition block.
pub(in crate::game) fn preflight_priority_window(state: &GameState) -> PriorityPreflight {
    debug_assert_eq!(PriorityReducerFamily::ALL.len(), 23);
    let principal = match priority_principal_for_preflight(state) {
        Ok(principal) => principal,
        Err(PriorityPreflightIndeterminate::NotPriority) => {
            return PriorityPreflight::NotPriorityWindow;
        }
        Err(block) => {
            return PriorityPreflight::Indeterminate {
                family: None,
                block: PriorityPreflightBlock::Principal(block),
            };
        }
    };
    let mut candidates = priority_preflight_candidates(state, &principal);
    for family in PriorityReducerFamily::ALL {
        let Some(index) = candidates
            .iter()
            .position(|candidate| candidate.family() == family)
        else {
            continue;
        };
        match candidates.remove(index) {
            PriorityPreflightCandidate::Indeterminate { block, .. } => {
                return PriorityPreflight::Indeterminate {
                    family: Some(family),
                    block,
                };
            }
            PriorityPreflightCandidate::Announcement(announcement) => {
                match apply_priority_announcement(state, &principal, announcement) {
                    Ok(_) => return PriorityPreflight::Actionable { family },
                    Err(_) => {
                        return PriorityPreflight::Indeterminate {
                            family: Some(family),
                            block: PriorityPreflightBlock::ReducerRejected,
                        };
                    }
                }
            }
        }
    }
    PriorityPreflight::Actionless
}

/// The narrow admission gate for engine-owned prospective progress. It keeps
/// the detailed preflight result private to `game` while ensuring a caller can
/// advance only an already-proven actionless Priority window.
pub(crate) fn priority_window_is_actionless_for_mandatory_progress(state: &GameState) -> bool {
    matches!(
        preflight_priority_window(state),
        PriorityPreflight::Actionless
    )
}

/// Submit the sole ordinary pass for an already-proven actionless Priority
/// window during an engine-owned prospective simulation. The caller receives
/// no actor, semantic holder, or action-construction authority.
pub(crate) fn apply_actionless_priority_pass_for_prospective(
    state: &mut GameState,
) -> Result<ProspectiveSimulationOutcome, EngineError> {
    let principal = priority_principal_for_preflight(state).map_err(|_| {
        EngineError::ActionNotAllowed("Priority is not safe for mandatory progress".to_string())
    })?;
    if !priority_window_is_actionless_for_mandatory_progress(state) {
        return Err(EngineError::ActionNotAllowed(
            "Priority is not actionless".to_string(),
        ));
    }
    apply_interaction_for_prospective_simulation(
        state,
        principal.authenticated_actor(),
        principal.semantic_holder(),
        GameAction::PassPriority,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicFinalizeMode {
    Immediate,
    DeferredDisplay,
}

/// CR 601.2h + CR 702.132a: Assist remains cancellable while it is only a
/// selected contribution. Once helper payment has started or completed, its
/// resources may have changed and cancellation cannot roll that prefix back.
fn ensure_assist_cancellation_is_allowed(state: &GameState) -> Result<(), EngineError> {
    let pending = state
        .pending_cast
        .as_deref()
        .or_else(|| state.waiting_for.pending_cast_ref());
    if pending.is_some_and(|pending| pending.activation_cost_committed) {
        return Err(EngineError::ActionNotAllowed(
            "Cannot cancel an activation after a cost is paid".to_string(),
        ));
    }
    if matches!(
        pending.map(|pending| pending.assist_state),
        Some(AssistState::PaymentStarted { .. } | AssistState::Paid { .. })
    ) {
        return Err(EngineError::ActionNotAllowed(
            "Cannot cancel a cast after an Assist contribution is committed".to_string(),
        ));
    }
    Ok(())
}

fn handle_unlock_room_door(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    door: crate::game::game_object::RoomDoor,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if state.active_player != player
        || !matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
        || !state.stack.is_empty()
    {
        return Err(EngineError::ActionNotAllowed(
            "Room doors can be unlocked only as a main-phase special action with an empty stack"
                .to_string(),
        ));
    }

    let cost = {
        let obj = state
            .objects
            .get(&object_id)
            .ok_or_else(|| EngineError::InvalidAction("Room not found".to_string()))?;
        if obj.controller != player || obj.zone != Zone::Battlefield {
            return Err(EngineError::ActionNotAllowed(
                "Only the controller of a battlefield Room can unlock it".to_string(),
            ));
        }
        if !obj
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Room")
        {
            return Err(EngineError::ActionNotAllowed(
                "Object is not a Room".to_string(),
            ));
        }
        if obj.room_unlocks.unwrap_or_default().is_unlocked(door) {
            return Err(EngineError::ActionNotAllowed(
                "That door is already unlocked".to_string(),
            ));
        }
        match door {
            crate::game::game_object::RoomDoor::Left => obj.mana_cost.clone(),
            crate::game::game_object::RoomDoor::Right => obj
                .back_face
                .as_ref()
                .map(|face| face.mana_cost.clone())
                .ok_or_else(|| {
                    EngineError::ActionNotAllowed("Room has no right door face".to_string())
                })?,
        }
    };

    // CR 116.2m + CR 118.7a: Reduce the door's generic unlock cost by the
    // player's active `ReduceActionCost { action: UnlockDoor }` statics
    // (Inquisitive Glimmer — "Unlock costs you pay cost {1} less") before
    // payment. Single authority shared with the plot path.
    let cost = casting::apply_special_action_cost_reduction(
        state,
        player,
        crate::types::mana::SpecialAction::UnlockDoor,
        cost,
    );

    // CR 116.2m + CR 709.5e + CR 106.6: The unlock cost is a special action's
    // mana cost. Route payment through `PaymentContext::SpecialAction(UnlockDoor)`
    // so spend-restricted mana ("only to … unlock doors", Smoky Lounge) is
    // eligible here and spell/activation-restricted mana is correctly rejected.
    casting::pay_special_action_mana_cost(
        state,
        player,
        Some(object_id),
        &cost,
        crate::types::mana::SpecialAction::UnlockDoor,
        events,
    )?;

    super::room::unlock_door_designation(state, object_id, player, door, events);
    Ok(WaitingFor::Priority { player })
}

/// Public engine entrypoint. Every caller must supply the `actor` — the
/// `PlayerId` whose authenticated identity is making this action. The engine
/// rejects any action whose `actor` does not match `authorized_submitter(state)`
/// (with a narrow Concede exception — see `check_actor_authorization`).
///
/// # Safety contract (non-negotiable)
///
/// `actor` must come from a **trusted transport boundary**, never from
/// client-supplied payload data. Adapters that forward actions from a remote
/// peer (WebSocket server, P2P host) must tag the action with the PlayerId
/// associated with the *connection*, not a value copied out of the wire frame.
/// Otherwise a malicious peer can trivially spoof another player's identity.
///
/// Engine-internal simulation (AI search, legal-action probing) may use
/// [`apply_as_current`] which derives `actor` from the game state itself.
pub fn apply(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    apply_action_boundary(state, actor, action, PublicFinalizeMode::Immediate)
}

/// Explicit-actor simulation apply: [`apply`] for throwaway forward-projection
/// clones the caller never renders (the AI velocity-policy `project_to`
/// look-ahead). Identical rules resolution to [`apply`], but in
/// `DeferredDisplay` mode it skips `finalize_display_state` — the board-global
/// mana-availability sweep whose frontend-only output no rules or
/// AI-evaluation path consults. See [`apply_as_current_for_simulation`] for the
/// actor-derived counterpart used by the search's `apply_candidate`; both keep
/// the projected/simulated game-logic state rules-correct while removing the
/// per-step O(battlefield) display sweep (#4798).
pub fn apply_for_simulation(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    apply_action_boundary(state, actor, action, PublicFinalizeMode::DeferredDisplay)
}

/// Interaction-contract action boundary. `authenticated_actor` is the trusted
/// submitting connection; `semantic_owner` is the player whose decision slot
/// the opaque interaction capability names. They differ when another player
/// controls that player's decisions.
pub fn apply_interaction(
    state: &mut GameState,
    authenticated_actor: PlayerId,
    semantic_owner: PlayerId,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    apply_action_boundary_for_semantic_owner(
        state,
        authenticated_actor,
        semantic_owner,
        action,
        PublicFinalizeMode::Immediate,
    )
}

pub(crate) fn apply_interaction_for_simulation(
    state: &mut GameState,
    authenticated_actor: PlayerId,
    semantic_owner: PlayerId,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    apply_action_boundary_for_semantic_owner(
        state,
        authenticated_actor,
        semantic_owner,
        action,
        PublicFinalizeMode::DeferredDisplay,
    )
}

/// Apply one reducer-backed action for a prospective route and retain only the
/// observation sidecar emitted by its successful outer action boundary.
///
/// Rules state, events, and ordinary simulation behavior remain identical to
/// `apply_interaction_for_simulation`; the additional data is deliberately
/// opaque and cannot be forged into a live action.
pub(crate) fn apply_interaction_for_prospective_simulation(
    state: &mut GameState,
    authenticated_actor: PlayerId,
    semantic_owner: PlayerId,
    action: GameAction,
) -> Result<ProspectiveSimulationOutcome, EngineError> {
    let raw = apply_action_boundary_core(state, authenticated_actor, semantic_owner, action, None)?;
    let (action, lifecycle_facts) = finish_action_boundary_with_lifecycle(
        state,
        raw,
        PublicFinalizeMode::DeferredDisplay,
        true,
    )?;
    Ok(ProspectiveSimulationOutcome {
        action,
        lifecycle_facts,
    })
}

/// Apply exactly the reducer portion of an interaction action for the
/// clone-local life-safety preview. The normal public/simulation entry points
/// continue through the complete reconciliation and presentation boundary.
pub(crate) fn apply_interaction_pre_reconciliation_for_life_safety(
    state: &mut GameState,
    authenticated_actor: PlayerId,
    semantic_owner: PlayerId,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    let raw = apply_action_boundary_core(state, authenticated_actor, semantic_owner, action, None)?;
    let RawActionApplication {
        result, lifecycle, ..
    } = raw;
    lifecycle.discard();
    Ok(result)
}

pub(super) fn apply_action_boundary(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
    mode: PublicFinalizeMode,
) -> Result<ActionResult, EngineError> {
    apply_action_boundary_with_stack_limit(state, actor, actor, action, mode, None)
}

fn apply_action_boundary_for_semantic_owner(
    state: &mut GameState,
    authenticated_actor: PlayerId,
    semantic_owner: PlayerId,
    action: GameAction,
    mode: PublicFinalizeMode,
) -> Result<ActionResult, EngineError> {
    apply_action_boundary_with_stack_limit(
        state,
        authenticated_actor,
        semantic_owner,
        action,
        mode,
        None,
    )
}

pub(super) fn apply_action_boundary_with_stack_limit(
    state: &mut GameState,
    authenticated_actor: PlayerId,
    semantic_owner: PlayerId,
    action: GameAction,
    mode: PublicFinalizeMode,
    stack_resolution_limit: Option<u32>,
) -> Result<ActionResult, EngineError> {
    let raw = apply_action_boundary_core(
        state,
        authenticated_actor,
        semantic_owner,
        action,
        stack_resolution_limit,
    )?;
    finish_action_boundary(state, raw, mode)
}

struct RawActionApplication {
    result: ActionResult,
    journal_start: usize,
    is_actor_scoped_preference: bool,
    boundary_snapshot: GameState,
    previous_interaction_waiting: WaitingFor,
    previous_interaction_slots: Vec<crate::types::interaction::ActiveInteractionSlot>,
    submitted_interaction_owner: Option<PlayerId>,
    preserve_interaction: bool,
    lifecycle: super::lifecycle::ActionLifecycleGuard,
}

/// Receipt-relevant observations from one prospective action. The lifecycle
/// frame itself remains private to `game`; consumers only receive exact,
/// immutable delayed-trigger facts after a successful outer boundary.
pub(crate) struct ProspectiveSimulationOutcome {
    pub(crate) action: ActionResult,
    lifecycle_facts: Option<super::lifecycle::ProspectiveLifecycleFacts>,
}

impl ProspectiveSimulationOutcome {
    pub(crate) fn has_outer_lifecycle_facts(&self) -> bool {
        self.lifecycle_facts.is_some()
    }

    pub(crate) fn delayed_installations(
        &self,
    ) -> impl Iterator<Item = (DelayedTriggerOrigin, ObjectId, PlayerId)> + '_ {
        self.lifecycle_facts
            .iter()
            .flat_map(|facts| facts.delayed_installations())
    }

    pub(crate) fn receipt_finished_normally(&self, origin: DelayedTriggerOrigin) -> bool {
        self.lifecycle_facts
            .as_ref()
            .is_some_and(|facts| facts.receipt_finished_normally(origin))
    }

    pub(crate) fn receipt_terminalized(&self, origin: DelayedTriggerOrigin) -> bool {
        self.lifecycle_facts
            .as_ref()
            .is_some_and(|facts| facts.receipt_terminalized(origin))
    }
}

fn apply_action_boundary_core(
    state: &mut GameState,
    authenticated_actor: PlayerId,
    semantic_owner: PlayerId,
    action: GameAction,
    stack_resolution_limit: Option<u32>,
) -> Result<RawActionApplication, EngineError> {
    let lifecycle = super::lifecycle::enter_action_frame();
    if let Err(error) = mana_sources::preflight_tap_land_action(state, semantic_owner, &action) {
        lifecycle.discard();
        return Err(error);
    }
    let boundary_snapshot = state.clone();
    let journal_start = state.resolved_rules_journal.entries().len();
    let is_actor_scoped_preference = action.is_actor_scoped_preference();
    interaction::ensure_interaction_authority(state);
    let previous_interaction_waiting = state.waiting_for.clone();
    let previous_interaction_slots = state.active_interaction_slots.clone();
    let submitted_interaction_owner = if authenticated_actor == semantic_owner {
        interaction::semantic_owner_for_actor(state, authenticated_actor)
    } else {
        Some(semantic_owner)
    };
    let preserve_interaction = interaction::action_preserves_interaction(&action);
    // Clear transient inter-effect state at the start of each player action.
    // last_effect_count is set by interactive handlers (e.g., DiscardChoice) and
    // consumed by sub_ability continuations via EventContextAmount fallback.
    state.last_effect_count = None;
    state.last_effect_counts_by_player.clear();
    state.exiled_from_hand_this_resolution = 0;
    state.die_result_this_resolution = None;
    state.consumed_before_priority_trigger_events.clear();
    if let Err(err) = check_actor_authorization(state, authenticated_actor, &action) {
        lifecycle.discard();
        *state = boundary_snapshot;
        return Err(err);
    }
    let mut result = match apply_action(state, semantic_owner, action, stack_resolution_limit) {
        Ok(result) => result,
        Err(err) => {
            lifecycle.discard();
            *state = boundary_snapshot;
            return Err(err);
        }
    };
    // CR 400.7 + CR 403.3 + CR 614.12a: an as-enters choice (and any continuation it raises) can
    // span an arbitrary number of client round-trips of ANY `WaitingFor` shape, so realization of a
    // parked token battlefield entry is keyed on the action having SETTLED, not on prompt shape.
    // `apply_action` realizes it itself on every route that reaches `run_post_action_pipeline`.
    //
    // CR 603.6a: the entry pair this realization emits IS the event that puts a permanent onto the
    // battlefield, so every permanent must be checked for matching enters-the-battlefield triggers
    // (CR 603.2 + CR 603.3b place them on the stack before the next player receives priority).
    // Reaching here with something to realize means the action settled WITHOUT running
    // `run_post_action_pipeline` — one of the reducer arms that builds an `ActionResult` straight
    // out of the match (`handle_tribute_choice` is the reachable one). Converge those onto the same
    // pipeline the rest of the reducer uses, scanning ONLY the slice this realization appended, so
    // a handler that already settled its own events (`handle_opponent_may_choice`, which collects
    // into `deferred_triggers` without recording them in `consumed_before_priority_trigger_events`)
    // cannot have them collected a second time. Inert on every other route: the flush returns
    // `false` when nothing was parked or an earlier convergence point already consumed it
    // (`Option::take_if`).
    let scan_from = result.events.len();
    if effects::token::realize_settled_token_battlefield_entry(state, &mut result.events) {
        let wf = match engine_priority::run_post_action_pipeline_from(
            state,
            &mut result.events,
            scan_from,
            &result.waiting_for,
            false,
            false,
        ) {
            Ok(wf) => wf,
            Err(err) => {
                *state = boundary_snapshot;
                return Err(err);
            }
        };
        // The pipeline's terminal return hands back `flush_pending_priority_intercepts(..)` WITHOUT
        // writing `state.waiting_for`, and the drain can raise `OrderTriggers` (CR 603.3b; measured
        // on the Fanatic route). BOTH writes are load-bearing: `finish_action_boundary` copies
        // `result.waiting_for` INTO the state at `sync_waiting_for`, and
        // `apply_interaction_pre_reconciliation_for_life_safety` returns `raw.result` without ever
        // calling `finish_action_boundary`.
        state.waiting_for = wf.clone();
        result.waiting_for = wf;
    }
    Ok(RawActionApplication {
        result,
        journal_start,
        is_actor_scoped_preference,
        boundary_snapshot,
        previous_interaction_waiting,
        previous_interaction_slots,
        submitted_interaction_owner,
        preserve_interaction,
        lifecycle,
    })
}

fn finish_action_boundary(
    state: &mut GameState,
    raw: RawActionApplication,
    mode: PublicFinalizeMode,
) -> Result<ActionResult, EngineError> {
    finish_action_boundary_with_lifecycle(state, raw, mode, false).map(|(result, _)| result)
}

fn finish_action_boundary_with_lifecycle(
    state: &mut GameState,
    raw: RawActionApplication,
    mode: PublicFinalizeMode,
    return_outer_lifecycle: bool,
) -> Result<
    (
        ActionResult,
        Option<super::lifecycle::ProspectiveLifecycleFacts>,
    ),
    EngineError,
> {
    state.consumed_before_priority_trigger_events.clear();
    let RawActionApplication {
        mut result,
        journal_start,
        is_actor_scoped_preference,
        boundary_snapshot,
        previous_interaction_waiting,
        previous_interaction_slots,
        submitted_interaction_owner,
        preserve_interaction,
        lifecycle,
    } = raw;
    reconcile_terminal_result(state, &mut result);
    bump_state_revision(state);
    sync_waiting_for(state, &result.waiting_for);
    let auto_pass_advanced = if is_actor_scoped_preference {
        false
    } else {
        run_auto_pass_loop(state, &mut result)
    };
    reconcile_terminal_result(state, &mut result);
    // Debug "infinite mana" (CR 500.5 suppressed for flagged players): restore any
    // pool that a spend during this action depleted, before public state is
    // finalized and the next affordability probe runs. No-op when none flagged.
    super::mana_payment::refill_infinite_mana(state);
    remember_public_reveals(state, &result.events, journal_start);
    // Targeted public-state dirty marking over the full accumulated event set
    // (the auto-pass loop appends events). `finalize_public_state` is the only
    // consumer of `public_state_dirty`, so marking once here over the complete
    // event stream is correct and cheapest.
    mark_public_state_from_events(state, &result.events);
    finalize_rules_state(state);
    result.waiting_for = state.waiting_for.clone();
    if matches!(mode, PublicFinalizeMode::Immediate) {
        finalize_display_state(state);
    }
    result.log_entries = super::log::resolve_log_entries(&result.events, state);
    if preserve_interaction && !auto_pass_advanced {
        interaction::preserve_interaction_slots(state, previous_interaction_slots);
    } else {
        if interaction::rebind_interaction_slots_after_action(
            state,
            &previous_interaction_waiting,
            previous_interaction_slots,
            submitted_interaction_owner,
        )
        .is_err()
        {
            lifecycle.discard();
            *state = boundary_snapshot;
            return Err(EngineError::InvalidAction(
                "Unable to allocate interaction authority for the resulting decision".to_string(),
            ));
        }
    }
    #[cfg(debug_assertions)]
    debug_assert_runtime_resolution_invariants(state);
    let lifecycle_facts = if return_outer_lifecycle {
        lifecycle.take_outer_facts()
    } else {
        lifecycle.commit_into_parent();
        None
    };
    Ok((result, lifecycle_facts))
}

thread_local! {
    /// PR-3 (Option C): set while inside a legality/search simulation probe
    /// (`ai_support::SimulationFilter`'s clone-and-apply). Loop-shortcut detection
    /// (`reconcile_terminal_result` §3) and ring accumulation
    /// (`pass_priority_once_with_pipeline` §2) are TOP-LEVEL-ONLY — a hypothetical
    /// single-action probe is NOT a real CR 732.2a play sequence, so it must neither
    /// shortcut nor accumulate. Engine game logic is single-threaded (no rayon /
    /// par_iter / std::thread::spawn in the apply or legal_actions path), `apply()` is
    /// fully synchronous (no `.await` between set and restore), and the tokio server
    /// runs each apply synchronously within one task on one thread, so the RAII
    /// set/restore is balanced on a single thread within one call. Mirrors the in-engine
    /// thread-local idiom (`perf_counters.rs`, `layers.rs`, `quantity.rs`).
    static IN_SIMULATION_PROBE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// True while inside a `SimulationFilter` legality probe. Read by §2 and §3.
pub(crate) fn in_simulation_probe() -> bool {
    IN_SIMULATION_PROBE.with(|f| f.get())
}

/// RAII guard: sets the probe flag, restores the PREVIOUS value on drop (panic-safe,
/// nesting-correct — a probe that itself enumerates legal actions keeps the flag set).
#[must_use]
pub(crate) struct SimulationProbeGuard(bool);
impl SimulationProbeGuard {
    pub(crate) fn enter() -> Self {
        SimulationProbeGuard(IN_SIMULATION_PROBE.with(|f| f.replace(true)))
    }
}
impl Drop for SimulationProbeGuard {
    fn drop(&mut self) {
        IN_SIMULATION_PROBE.with(|f| f.set(self.0));
    }
}

fn reconcile_terminal_result(state: &mut GameState, result: &mut ActionResult) {
    // Safety net (fixes #962): If a player-loss SBA would eliminate a player,
    // run SBAs now. CR 704.3 normally checks SBAs when a player would receive
    // priority, but skipping them here can leave the engine waiting on a dead
    // player for a non-priority choice.
    //
    // The predicate lives in `sba` so it shares the same CR 101.2 "can't lose"
    // exception as the real player-loss SBA checks, and stays narrower than the
    // full SBA loop to avoid unrelated mid-resolution SBA prompts.
    if sba::has_pending_player_loss_sba(state) {
        sba::check_state_based_actions(state, &mut result.events);
        // SBA may have advanced waiting_for (e.g., GameOver, or Priority for
        // the next living player). Sync the result.
        result.waiting_for = state.waiting_for.clone();
    }

    super::elimination::ensure_game_over_if_terminal(state, &mut result.events);
    if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        match_flow::handle_game_over_transition(state);
        result.waiting_for = state.waiting_for.clone();
    }

    // CR 732.2a + CR 704.5a: shortcut a NET-PROGRESS mandatory cascade to its
    // determinate single-opponent loss. Runs AFTER the CR 704 state-based actions
    // above (CR 704.3 ordering), so a player ALREADY at 0 life loses via the real
    // 704.5a SBA first and this never preempts or double-fires a legitimate win — it
    // only fires when the game would otherwise grind on (high victim life, or mid-drain
    // before 0). The `!GameOver` guard makes it idempotent across the two
    // `reconcile_terminal_result` calls in `apply` (`:326` and `:330`).
    if !matches!(state.waiting_for, WaitingFor::GameOver { .. })
        && matches!(state.waiting_for, WaitingFor::Priority { .. }) // a player would get priority (CR 704.3)
        // CR 732.2a: the mandatory-loop game-ending shortcut is gated behind the
        // user-controllable combo-detector opt-in. With `loop_detection == Off` (the
        // default) the engine NEVER resolves a mandatory loop to its determinate
        // outcome — the game simply continues as it did before the combo-detector
        // existed (the natural CR 704.5a SBA death still ends a real life drain, just
        // not as a shortcut). This is an intentional opt-in departure: new
        // game-changing functionality ships OFF so it can be developed safely
        // (issue #4603). When OFF the ring is also never populated (the sampler is
        // gated identically), so this conjunct is defense-in-depth, not the sole gate.
        // PR-7 Phase 3: `samples()` (not `is_on()`) so `Interactive` also enters. For
        // `Off` (false) and `On` (true) `samples() == is_on()`, so both are unchanged;
        // only `Interactive` newly enters, dispatched by the mode `match` in the body.
        && state.loop_detection.samples()
        && !state.stack.is_empty()
        && !state.loop_detect_ring.is_empty()
        // PR-3 Defect-2: loop-shortcut detection is TOP-LEVEL-ONLY. Inside a
        // `SimulationFilter` legality probe the flag is set, so §3 is skipped. This
        // enforces the invariant that a hypothetical single-action probe never runs
        // game-ending shortcut logic, and guards the
        // reconcile→§3→§9→legal_actions→SimulationFilter→reconcile path against
        // unbounded re-entry. (In the current architecture the §9 gate's pass-state
        // reset already makes those nested probes handoffs that do not re-resolve, so
        // the path is bounded even without this conjunct — see the impl report's
        // Defect-2 measurement — but the guard keeps the top-level-only invariant
        // explicit and robust to future §9/§2 changes.)
        && !in_simulation_probe()
    {
        // PR-7 Phase 3: dispatch the confirmed-loop body by mode. The `On` arm is the
        // pre-change block VERBATIM — byte-identical event stream, proven by the T-ON
        // golden captured from HEAD before this wrap. `Interactive` routes to the general
        // classification bridge (offer + APNAP window + CR 732.4 draw). `Off` is
        // unreachable: the `samples()` guard above excludes it.
        match state.loop_detection {
            LoopDetectionMode::On => {
                // Clone the Arc handles (cheap refcount bumps) to release the borrow on the
                // ring before the GameOver mutation below.
                let priors: Vec<std::sync::Arc<crate::types::LoopDetectSample>> =
                    state.loop_detect_ring.iter().cloned().collect();
                let cur = crate::analysis::resource::ResourceVector::snapshot(state);
                // Carry the matching cycle's `delta` out of the scan alongside the winner so
                // the ∞ producer below can name the loop's unbounded axes without recomputing.
                // INDEXED scan (not `find_map`) so the matched prior's ring index `k` is known:
                // the m9 controller-non-dip and R5-B2 faller-simultaneity checks consume the
                // SAME `frames[k..] ++ live` per-resolution window. On a candidate winner that
                // fails either seam gate, continue scanning older priors (fail-safe).
                if let Some((winner, delta)) = priors.iter().enumerate().find_map(|(k, prior)| {
                    let delta = crate::analysis::resource::ResourceVector::delta(
                        &crate::analysis::resource::ResourceVector::snapshot(&prior.normalized),
                        &cur,
                    );
                    let winner = crate::analysis::loop_check::live_mandatory_loop_winner(
                        &prior.normalized,
                        state,
                        &delta,
                    )?;
                    // The matched window: the prior frame at `k`, every subsequent ring frame,
                    // then the live state — all per-resolution, no gaps (a non-sampling beat
                    // clears the ring, so a confirmed window is gap-free).
                    let mut frames: Vec<&GameState> =
                        priors[k..].iter().map(|p| &p.normalized).collect();
                    frames.push(state);
                    // CR 704.5a + CR 104.4a (m9): the winner (sole non-faller) must never dip
                    // across the window — a transient intra-cycle dip a net-delta check cannot
                    // see would kill it before the extrapolated win.
                    if !crate::analysis::loop_check::winner_life_never_dips(&frames, winner) {
                        return None;
                    }
                    // CR 704.3 + CR 800.4a + CR 104.2a (R5-B2): with ≥2 fallers, require
                    // pairwise-equal faller life at every frame so all cross lethal in ONE SBA
                    // batch (the first elimination is terminal — nothing past it is modeled).
                    let fallers: Vec<crate::types::player::PlayerId> = state
                        .players
                        .iter()
                        .filter(|p| !p.is_eliminated)
                        .map(|p| p.id)
                        .filter(|p| delta.life.get(p).copied().unwrap_or(0) < 0)
                        .collect();
                    if fallers.len() >= 2
                        && !crate::analysis::loop_check::fallers_lives_pairwise_equal(
                            &frames, &fallers,
                        )
                    {
                        return None;
                    }
                    Some((winner, delta))
                }) {
                    // CR 732.5: shortcut ONLY a loop NO living player can break. The gate runs
                    // ONCE after find_map (not per prior). At the per-beat drive this is the
                    // entire soundness firewall.
                    if no_living_player_has_meaningful_priority_action(state) {
                        // CR 732.2a: persist the confirmed loop's unbounded axes so
                        // `derive_views` projects the `∞` HUD rows. `winner` is the loop's
                        // controller (the non-faller); `unbounded_axes_for(winner)` returns the
                        // same axes `detect_loop` records in `LoopCertificate.unbounded`. This is
                        // the live producer of `unbounded_resources` for a detected loop (the
                        // debug `SetInfiniteMana` toggle is the only other producer). It runs
                        // only inside this OFF-gated block, so a default-OFF game never marks ∞.
                        state.mark_unbounded_loop(winner, &delta.unbounded_axes_for(winner));
                        result.events.push(GameEvent::GameOver {
                            winner: Some(winner),
                        });
                        state.waiting_for = WaitingFor::GameOver {
                            winner: Some(winner),
                        };
                        result.waiting_for = state.waiting_for.clone();
                        match_flow::handle_game_over_transition(state);
                    }
                }
            }
            LoopDetectionMode::Interactive => interactive_loop_bridge(state, result),
            LoopDetectionMode::Off => {
                unreachable!("reconcile shortcut body: samples() guard excludes Off")
            }
        }
    }

    // PR-7 Phase 4d-ii (CR 732.2a): the EMPTY-STACK dual of the ring-gated bridge above.
    // A self-returning (buyback) recast that creates an inert token settles with an EMPTY
    // stack, so the sampler clears the ring at that beat and the `!stack.is_empty()` bridge
    // is structurally unreachable for it. Detect it here by driving the captured loop-action
    // sequence on a clone. Gated identically (opt-in + top-level-only) plus a cheap
    // `last_loop_action_sequence` precondition (non-empty only on a buyback-paid token-creating
    // cast or a multi-activation engine's accumulated beats — so the clone-drive runs ~never for
    // the recast class; a mana engine arms per mana activation but its drive aborts fast when
    // unsustainable). INV-2: this OFFERS the interactive shortcut (never auto-resolves — CR 732.2a).
    if !matches!(state.waiting_for, WaitingFor::GameOver { .. })
        && matches!(state.waiting_for, WaitingFor::Priority { .. })
        && state.stack.is_empty()
        && state.loop_detection.samples()
        && !in_simulation_probe()
        && !state.last_loop_action_sequence.is_empty()
    {
        if let Some((certificate, schema)) = try_offer_object_growth_shortcut(state) {
            let WaitingFor::Priority { player: proposer } = state.waiting_for else {
                unreachable!("guarded by matches!(Priority) above")
            };
            state.waiting_for = WaitingFor::LoopShortcut {
                proposer,
                predicted_winner: None,
                certificate,
                schema,
            };
            result.waiting_for = state.waiting_for.clone();
        }
    }
}

/// PR-7 Phase 3 (CR 732.2a/b/c + CR 732.4 + CR 704.5a): the `Interactive`-mode branch of
/// the reconcile shortcut block. Routes the SAME confirmed live loop signal the `On` arm
/// consumes through the GENERAL classification instead of only the lethal auto-win:
///
/// - **Path A — determinate lethal single-winner** (constant-depth OR ω growing cascade,
///   via the reused, UN-widened [`crate::analysis::loop_check::live_mandatory_loop_winner`]):
///   if the loop is mandatory (CR 732.5: no living player can break it) this AUTO-WINS
///   exactly as `On` does (mandatory winning drain). If it is OPTIONAL (some player could
///   respond) it OFFERS the interactive shortcut (CR 732.2a) via `WaitingFor::LoopShortcut`.
/// - **Path B — CR 732.4 all-mandatory, net-progress, no-loss draw**: a confirmed cycle
///   with no determinate winner that drives NO player toward a loss and that no living
///   player can break is a draw (CR 104.4b / 104.4f).
///
/// Everything else (staggered-pod losses, optional pure-advantage loops) falls through
/// with no action — the pre-feature halt/continue behavior. Runs inside the same
/// top-level-only `!in_simulation_probe()` guard as the `On` arm.
///
/// Multiplayer subset-lethality is safe by construction: [`find_live_loop_winner`] delegates
/// to [`crate::analysis::loop_check::live_mandatory_loop_winner`], which partitions the living
/// players into life-fallers vs non-fallers and requires EXACTLY one non-faller
/// (`nonfallers.len() == 1`; CR 104.2a — a winner is determinate only when every other living
/// player falls). A loop lethal to only SOME opponents leaves a surviving bystander as a
/// second non-faller ⇒ `None` ⇒ neither Path A (no winner) nor Path B (a life-loss axis is
/// present, so it is not a CR 732.4 no-loss draw) fires, and it falls through without crowning.
fn interactive_loop_bridge(state: &mut GameState, result: &mut ActionResult) {
    // CR 732.5 / CR 732.2b: is the loop mandatory (no living player has a meaningful
    // priority action that could break it)? The single mandatory-vs-optional signal the
    // engine already computes — not a new stored flag.
    let mandatory = no_living_player_has_meaningful_priority_action(state);

    // Path A: determinate lethal single-winner drain.
    if let Some((winner, delta, prior)) = find_live_loop_winner(state) {
        if mandatory {
            // FIRM #1 — mandatory winning drain: identical to the `On` auto-win.
            // CR 732.2a: mark the loop's unbounded axes; CR 704.5a: terminal GameOver.
            state.mark_unbounded_loop(winner, &delta.unbounded_axes_for(winner));
            result.events.push(GameEvent::GameOver {
                winner: Some(winner),
            });
            state.waiting_for = WaitingFor::GameOver {
                winner: Some(winner),
            };
            result.waiting_for = state.waiting_for.clone();
            match_flow::handle_game_over_transition(state);
        } else {
            // CR 732.2a: OPTIONAL winning drain — only the player with priority may propose
            // the shortcut. Keep that proposer distinct from the already-measured winner; a
            // loop can be detected during a different player's priority window.
            // `build_cert`'s only use of the frame is `board_delta(prior, state)`, a
            // comparand read ⇒ the CR 104.4b `.normalized` half.
            let certificate = build_cert(&prior.normalized, state, &delta, winner);
            // CR 732.2a: a non-targeted drain reifies no per-iteration player choice ⇒ carry an
            // empty pin list; only the `iteration_count` (from `win_kind`) is populated.
            let WaitingFor::Priority { player: proposer } = state.waiting_for else {
                unreachable!("interactive bridge only runs during priority")
            };
            // CR 732.2a: a non-targeted drain publishes no decision points, and this path
            // states no narrowed CR 704 count bound — `UntilLethal` is terminated by the
            // real SBA, not by a caller-supplied count, so the ceiling stays the global
            // safety limit.
            let schema = build_shortcut_schema(
                Vec::new(),
                shortcut_iteration_count(certificate.win_kind),
                MAX_SHORTCUT_CYCLES,
            );
            state.waiting_for = WaitingFor::LoopShortcut {
                proposer,
                predicted_winner: Some(winner),
                certificate,
                schema,
            };
            result.waiting_for = state.waiting_for.clone();
        }
        return;
    }

    // Path D: CR 732.2a BOUNDED cycle fast-forward. Only reached when Path A found no
    // determinate winner — a drain lethal to SOME opponents leaves a second non-faller, so
    // CR 104.2a's determinacy requirement (`loop_check`'s crown gate) refuses to crown and
    // Path A returns `None`. This seam routes AROUND that gate rather than weakening it: it
    // never calls `live_mandatory_loop_winner` and writes `predicted_winner: None`.
    // Placed before Path B because Path B's CR 732.4 verdict is TERMINAL (it writes
    // `GameOver` and returns), so a seam ordered after it could never be reached on a state
    // Path B accepts. The two are disjoint anyway and the ordering does not paper over an
    // overlap: Path B requires `has_no_loss_axis(&delta)`, while this seam only offers when
    // `elimination_bounds` NARROWED below `MAX_SHORTCUT_CYCLES`, which happens only when the
    // cycle drives some living seat toward a CR 704.5a / CR 704.5c / CR 104.3c threshold —
    // i.e. exactly a loss axis.
    if let Ok(offer) = try_offer_bounded_cycle_shortcut(state, mandatory) {
        state.waiting_for = offer;
        result.waiting_for = state.waiting_for.clone();
        return;
    }

    // Path B: CR 732.4 all-mandatory, net-progress, no-loss draw. Only reached when Path A
    // found no determinate winner. `mandatory` gates it (CR 732.5); a loss axis or an
    // optional loop falls through to the pre-feature halt.
    if mandatory {
        let priors: Vec<std::sync::Arc<crate::types::LoopDetectSample>> =
            state.loop_detect_ring.iter().cloned().collect();
        let cur = crate::analysis::resource::ResourceVector::snapshot(state);
        for prior in &priors {
            let prior = &prior.normalized;
            let delta = crate::analysis::resource::ResourceVector::delta(
                &crate::analysis::resource::ResourceVector::snapshot(prior),
                &cur,
            );
            // CR 732.2a board-recurrence (constant-depth OR ω growing cascade) + net
            // progress + NO loss axis for anyone ⇒ the loop grinds forever with nobody
            // able to win or lose ⇒ CR 732.4 / 104.4b draw.
            if (crate::analysis::resource::loop_states_equal_modulo_resources(prior, state)
                || crate::analysis::resource::loop_states_cover_modulo_growth(prior, state))
                && delta.is_net_progress()
                && has_no_loss_axis(&delta)
            {
                result.events.push(GameEvent::GameOver { winner: None });
                state.waiting_for = WaitingFor::GameOver { winner: None };
                result.waiting_for = state.waiting_for.clone();
                match_flow::handle_game_over_transition(state);
                return;
            }
        }
    }
    // PR-7 Phase 4c (B5): OPTIONAL beneficial (non-winning) loop ⇒ revocable-∞ capability.
    // CR 104.4b: "Loops that contain an optional action don't result in a draw" — so an
    // optional net-progress no-loss loop is neither crowned (Path A: no faller) nor drawn
    // (Path B: !mandatory). It grinds under player control; record the unbounded capability
    // (mark_unbounded_loop) + its enablers so an enabler's departure REVOKES it (defuse hook
    // in zones.rs `apply_zone_exit_cleanup`). Reached only when Path A named no winner AND
    // the loop is OPTIONAL (a player can break it) — the pre-feature halt already applied
    // when Path B's `mandatory` gate excludes this branch, so this is a NEW arm, not a
    // narrowing of one.
    //
    // CR-FIDELITY NOTE: CR 104.4b grants the controller "no draw + player control", NOT a
    // persistent resource. The realization here reuses `unbounded_resources` /
    // `refill_infinite_mana`, which is a DOCUMENTED DEBUG-ONLY DEPARTURE FROM THE RULES
    // (mana_payment.rs top-up); reusing it for a real detected loop is team-lead's stated
    // design intent (in-scope). The mark means "this player can grind this axis unboundedly
    // under their own control", the closest live realization of CR 104.4b's grant.
    if !mandatory {
        let controller = state.active_player; // sampler gate is Priority{active_player}: the driver
        let priors: Vec<std::sync::Arc<crate::types::LoopDetectSample>> =
            state.loop_detect_ring.iter().cloned().collect();
        let cur = crate::analysis::resource::ResourceVector::snapshot(state);
        for prior in &priors {
            let prior = &prior.normalized;
            let delta = crate::analysis::resource::ResourceVector::delta(
                &crate::analysis::resource::ResourceVector::snapshot(prior),
                &cur,
            );
            // Same recurrence + net-progress predicate as Path B (byte-reused), minus the
            // `mandatory` gate. The object-growth disjunct is the SHARED-BUT-DORMANT arm
            // (empty residual today; lights up under 4a-live with no further edit).
            //
            // REDUNDANCY PROOF (R6, team-lead-verified): `has_no_loss_axis` (conjunct 3
            // below) is UNCONDITIONALLY REDUNDANT at this Path-C call site — every
            // self-loss axis it checks is already rejected by an EARLIER conjunct, so
            // removing it changes no Path-C outcome and a discriminating runtime test for
            // it HERE is unsatisfiable (waived; kept as documented defense-in-depth):
            //   - library↓ (self-mill): a card leaving the Library zone changes its
            //     `objects_content_eq` zone, so successive frames compare UNEQUAL and
            //     recurrence (conjunct 1) fails first — the loop never recurs, so this
            //     arm is never even reached.
            //   - life↓ (self-burn): life is a Consumed axis (`ResourceVector::components`),
            //     so `is_net_progress` (conjunct 2) returns false on any net-negative life
            //     (resource.rs ~:409, over all players) before conjunct 3 runs.
            //   - poison↑ (self-poison): `classify_win_kind` (conjunct 4) maps poison>0 to
            //     `WinKind::PoisonLoss`, not `Advantage`, so the `== Advantage` conjunct
            //     rejects it.
            // CONTRAST — the Path-B DRAW gate (:512-516 = recurrence + is_net_progress +
            // has_no_loss_axis, with NO `== Advantage` backstop) is DIFFERENT: there
            // `has_no_loss_axis` is the SOLE loss-axis veto and is LOAD-BEARING BY
            // CONSTRUCTION — it MUST NOT be removed. A poison loop reaching Path B satisfies
            // recurrence (poison is projected out at resource.rs:1995) AND is_net_progress
            // (poison is a Gained axis, which cannot make is_net_progress false), so without
            // this conjunct such a loop would be WRONGLY certified a CR 732.4 draw. (Path C's
            // poison redundancy comes ENTIRELY from its extra `== Advantage` conjunct, which
            // Path B lacks.) The Path-B veto is currently NOT runtime-discriminable: a
            // single-compound-trigger poison loop DOES reach the Path-B bridge, but the
            // "you gain N life and [each opponent gets a poison counter]" parser drop removes
            // the poison conjunct (card-build keeps only `GainLife`), so poison is 0 in the loop
            // delta at the gate → it draws as a benign lifegain loop and never exercises
            // has_no_loss_axis's poison veto. No constructible fixture carries poison>0 to the
            // Path-B gate (the 2-trigger form clears `loop_detect_ring` on its OrderTriggers
            // beats at engine.rs:1307; the single-compound-trigger form drops the poison at
            // parse). The runtime discriminator is therefore WAIVED as measured-unsatisfiable;
            // this in-code load-bearing-by-construction proof is the substitute. See the
            // `interactive_recurring_poison_is_not_drawn` Path-B behavioral test.
            if (crate::analysis::resource::loop_states_equal_modulo_resources(prior, state)
                || crate::analysis::resource::loop_states_cover_modulo_growth(prior, state)
                // CR 122.1 + CR 104.4b: OR a pure preserved-`Generic` counter-growth
                // cover (proliferate/charge Pentad Prism, burden The One Ring). Live
                // revocable-∞ mark ONLY — this Path-C arm routes to `mark_unbounded_loop`
                // + enabler registration below, which NEVER produces a GameOver; an
                // over-claim is a revocable capability, not a wrongful game-end.
                || crate::analysis::resource::loop_states_cover_modulo_counter_growth(
                    prior, state,
                ))
                && delta.is_net_progress()
                && has_no_loss_axis(&delta)
                && crate::analysis::loop_check::classify_win_kind(controller, &delta)
                    == crate::analysis::loop_check::WinKind::Advantage
            {
                let axes = delta.unbounded_axes_for(controller);
                if axes.is_empty() {
                    continue; // no unbounded axis for the driver ⇒ not this player's loop
                }
                // CR 104.4b: mark the revocable unbounded capability (idempotent set-union).
                state.mark_unbounded_loop(controller, &axes);
                // CR 110.1 + every-enabler: the stable recurring board is the enabler set.
                // battlefield_ids(prior) ∩ battlefield_ids(state) — complete for battlefield-
                // permanent enablers of a constant-depth loop, excludes intra-loop churn.
                let enablers: std::collections::BTreeSet<ObjectId> = prior
                    .battlefield
                    .iter()
                    .copied()
                    .filter(|id| state.battlefield.contains(id))
                    .collect();
                state.register_unbounded_loop_enablers(controller, enablers);
                return;
            }
        }
    }
    // else: staggered-pod loss / non-beneficial optional loop ⇒ no auto-resolve; fall
    // through to the pre-feature behavior (halt / continue).
}

/// PR-7 Phase 3: scan the live loop-detect ring for a determinate lethal single-winner,
/// applying the SAME per-frame window gates the `On` reconcile arm uses
/// ([`crate::analysis::loop_check::winner_life_never_dips`] +
/// [`crate::analysis::loop_check::fallers_lives_pairwise_equal`]). This is a deliberate,
/// isolated copy of the `On` arm's `find_map` scan — the `On` arm stays VERBATIM (byte-
/// identity gate), so it is not refactored to call this. Returns `(winner, per-cycle
/// delta, cycle-start frame)`; the frame feeds `board_delta` for the offer certificate.
fn find_live_loop_winner(
    state: &GameState,
) -> Option<(
    PlayerId,
    crate::analysis::resource::ResourceVector,
    std::sync::Arc<crate::types::LoopDetectSample>,
)> {
    let priors: Vec<std::sync::Arc<crate::types::LoopDetectSample>> =
        state.loop_detect_ring.iter().cloned().collect();
    let cur = crate::analysis::resource::ResourceVector::snapshot(state);
    priors.iter().enumerate().find_map(|(k, prior)| {
        let delta = crate::analysis::resource::ResourceVector::delta(
            &crate::analysis::resource::ResourceVector::snapshot(&prior.normalized),
            &cur,
        );
        let winner = crate::analysis::loop_check::live_mandatory_loop_winner(
            &prior.normalized,
            state,
            &delta,
        )?;
        let mut frames: Vec<&GameState> = priors[k..].iter().map(|p| &p.normalized).collect();
        frames.push(state);
        if !crate::analysis::loop_check::winner_life_never_dips(&frames, winner) {
            return None;
        }
        let fallers: Vec<PlayerId> = state
            .players
            .iter()
            .filter(|p| !p.is_eliminated)
            .map(|p| p.id)
            .filter(|p| delta.life.get(p).copied().unwrap_or(0) < 0)
            .collect();
        if fallers.len() >= 2
            && !crate::analysis::loop_check::fallers_lives_pairwise_equal(&frames, &fallers)
        {
            return None;
        }
        Some((winner, delta, prior.clone()))
    })
}

/// PR-7 Phase 3: build the offer certificate for an OPTIONAL winning drain. Fills the
/// residual via the SINGLE `board_delta` population seam (`loop_check.rs` invariant — NOT
/// `BoardDelta::default()`); empty for a constant-depth drain, non-empty for the ω growing
/// cascade where the Phase-4 materialization consumer reads it.
fn build_cert(
    prior: &GameState,
    state: &GameState,
    delta: &crate::analysis::resource::ResourceVector,
    winner: PlayerId,
) -> crate::analysis::loop_check::LoopCertificate {
    crate::analysis::loop_check::LoopCertificate {
        unbounded: delta.unbounded_axes_for(winner),
        win_kind: crate::analysis::loop_check::classify_win_kind(winner, delta),
        // The offer is only reached for an OPTIONAL loop.
        mandatory: false,
        residual_board_delta: crate::analysis::resource::board_delta(prior, state),
        // CR 732.2a: only a producer that NARROWED the repetition bound states a per-period
        // signature. The bounded-cycle offer overrides this field with functional-update
        // syntax at its own call site; every other producer publishes none.
        per_cycle: None,
    }
}

/// CR 732.2a: which conjunct of [`try_offer_bounded_cycle_shortcut`] refused to offer.
///
/// Exhaustive and typed, in the order the conjuncts run. Production ignores the value (a
/// refusal is a refusal), but a negative test row must be able to say WHICH conjunct it is
/// about: an assertion that merely observes "no offer" silently stops testing its own
/// conjunct the moment an earlier one starts refusing first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundedOfferRefusal {
    /// (1) Not a `WaitingFor::Priority` beat, so nobody may suggest a shortcut.
    NotAtPriority,
    /// (1b) A non-empty `last_loop_action_sequence` routes an accepted proposal to the
    /// object-growth materializer, which commits zero bounded cycles.
    DrivingSequenceNotEmpty,
    /// (2) The priority holder is not the active player the ring sampler gates on.
    ProposerIsNotActivePlayer,
    /// (4) Neither certification basis matched.
    NoCertification,
    /// (5) `WinKind::Advantage` — no CR 704 threshold, so this is Path C's class.
    AdvantageOnlyCycle,
    /// (6) A per-iteration choice the cycle opens is not specified by a published slot.
    UnspecifiedChoiceWindow,
    /// (7) `elimination_bounds` produced no count in `1..MAX_SHORTCUT_CYCLES`.
    NoNarrowedLegalCount,
}

/// CR 732.2a: the THIRD entry predicate into the loop-shortcut pipeline — a BOUNDED cycle
/// fast-forward for a loop that is lethal to SOME opponents but crowns nobody.
///
/// Path A ([`find_live_loop_winner`]) needs a determinate single winner, which CR 104.2a
/// makes impossible while two non-fallers live; Path B needs an all-mandatory no-loss draw.
/// A 4-player drain that kills two seats and leaves two is neither, so both fall through
/// and the loop grinds by hand. CR 732.2a still licenses a shortcut for it, PROVIDED the
/// proposal names a repetition count whose results are *predictable* — which is exactly what
/// this predicate establishes and refuses to offer without.
///
/// Everything downstream of the `WaitingFor::LoopShortcut` this returns is shipped and
/// unchanged: the same offer shape Path A writes, the same declare handler, the same APNAP
/// window, the same materializer. Two field values keep the classes apart BY CONSTRUCTION
/// rather than by review vigilance:
/// * `predicted_winner: None` — this seam never calls `live_mandatory_loop_winner`, so it
///   neither consults nor weakens the CR 104.2a crown gate (`loop_check.rs`'s
///   `nonfallers.len() != 1`); it routes around it.
/// * an EMPTY `last_loop_action_sequence` (step 1b) — the object-growth producer's class is
///   the complement, and `materialize_fixed_shortcut` dispatches on that same discriminant.
///
/// Returns the offer to write, or the FIRST conjunct that refused. Pure: it reads `state` and
/// writes nothing. The refusal is typed rather than a bare `None` because nine fail-closed
/// conjuncts that all collapse to "no offer" are neither diagnosable nor testable: a negative
/// row asserting only the absence of an offer passes for the wrong reason as soon as an
/// upstream conjunct starts refusing first (domination), and `BoundedOfferRefusal` is what
/// lets such a row name the conjunct it is actually about.
/// CR 732.2a: the capability token that gates the cap-parameterised
/// [`crate::analysis::resource::PeriodVerdicts`] constructor.
///
/// The unit field is PRIVATE, so the tuple constructor is nameable only inside
/// `game::engine` — the metered seam's own module. Any other site that tried to
/// build a fresh arbitrary-cap verdict container, whose spend the mint's meter
/// would never see, is E0603. Derive list pinned to `#[derive(Debug)]`: a derived
/// constructor would re-open arbitrary caps crate-wide, which is the same
/// strength as the defect this token closes.
#[derive(Debug)]
pub(crate) struct CapAuthority(());

/// CR 732.2a: the CLOSED cap domain the metered seam accepts.
///
/// The seam is `pub` because rows outside this crate ride it, and it mints its
/// own [`CapAuthority`] for whoever calls it — so no token mechanism can gate
/// this route. It is gated at the VALUE instead: an arbitrary raise is
/// unrepresentable, and every image is fail-closed. Resolution is the SEAM's,
/// never the caller's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeCap {
    /// The shipped budget.
    Shipped,
    /// `min(n, PROBE_BUDGET)` — expresses every starvation arm and can never raise.
    Lowered(u32),
    /// Twice the board's own link count, derived from the `state` the seam
    /// probes rather than chosen by the caller. At the seam no window has been
    /// selected yet, so nothing is exempt and the whole current stack IS the
    /// non-exempt population; the work is therefore proportional to the input
    /// the caller itself supplied.
    RaisedTwiceLinks,
}

/// CR 732.2a: the mint's meter SNAPSHOT, taken at seam exit.
///
/// `spent`/`denied` are the probe budget's; the three `conjunct*` counters are
/// the verdict door's own, so an iteration claim has a surface to be asserted on
/// rather than being inferred from charges (which are measurably not a proxy for
/// iterations).
#[derive(Debug, Clone, Copy)]
pub struct MintMeter {
    pub spent: u32,
    pub denied: bool,
    pub conjunct6_asks: u32,
    pub conjunct6_frozen_skips: u32,
    pub conjunct4_scans: u32,
    /// CR 732.2a: WHICH certificate step 4/4b selected — `None` when the mint refused
    /// before certification (steps 1/1b/2/2b, or no basis matched at all).
    ///
    /// It exists because the axis has NO other surface. Both bases now MEASURE
    /// `frames_per_period`, so the published [`crate::analysis::loop_check::LoopCertificate`]
    /// discriminates in neither direction (see `certified_bounded_cycle_offer`'s
    /// attribution note), and the disjunct within basis A is invisible there entirely.
    /// A row that must prove a real beat certified through a particular disjunct — the
    /// frozen exemption is keyed to exactly one — would otherwise have no assert site.
    pub certification: Option<crate::analysis::resource::PeriodCertification>,
}

/// The production entry point: delegates at the shipped cap and drops the meter,
/// so the refusal contract and every existing caller are untouched.
pub fn try_offer_bounded_cycle_shortcut(
    state: &GameState,
    mandatory: bool,
) -> Result<WaitingFor, BoundedOfferRefusal> {
    try_offer_bounded_cycle_shortcut_metered(state, mandatory, ProbeCap::Shipped).0
}

/// CR 732.2a: the OBSERVATION-AND-CAP seam — the same mint, with the per-mint
/// probe cap supplied from the closed [`ProbeCap`] domain and the meter returned
/// instead of dropped.
///
/// This is the only channel by which a cap other than the shipped one enters, and
/// the only surface on which `spent` / `denied` / the conjunct counters are
/// readable at all: the verdict container never escapes this function.
pub fn try_offer_bounded_cycle_shortcut_metered(
    state: &GameState,
    mandatory: bool,
    cap: ProbeCap,
) -> (Result<WaitingFor, BoundedOfferRefusal>, MintMeter) {
    let mut meter = MintMeter {
        spent: 0,
        denied: false,
        conjunct6_asks: 0,
        conjunct6_frozen_skips: 0,
        conjunct4_scans: 0,
        certification: None,
    };
    let outcome = bounded_cycle_offer(state, mandatory, cap, &mut meter);
    (outcome, meter)
}

fn bounded_cycle_offer(
    state: &GameState,
    mandatory: bool,
    cap: ProbeCap,
    meter: &mut MintMeter,
) -> Result<WaitingFor, BoundedOfferRefusal> {
    use crate::analysis::resource::{PeriodVerdicts, PROBE_BUDGET};

    // (1) CR 732.2a: "the player with priority may suggest a shortcut."
    let WaitingFor::Priority { player: proposer } = state.waiting_for else {
        return Err(BoundedOfferRefusal::NotAtPriority);
    };
    // (1b) The bounded drain mints nothing, so it is reachable in `materialize_fixed_shortcut`
    // ONLY below that function's object-growth dispatch — and that dispatch is an EARLY
    // RETURN gated on `!state.last_loop_action_sequence.is_empty()`. An offer minted with a
    // non-empty sequence would be accepted and routed to the object-growth materializer,
    // committing ZERO bounded cycles and making this whole path silently dead. The two
    // conjuncts are not disjoint — a mana activation arms a period and a same-controller
    // on-stack activation both appends to it and leaves the stack non-empty, which is the
    // bridge's own entry condition — so this guard is load-bearing, not a restatement of an
    // invariant. It converts a silent misroute into an observable refusal.
    if !state.last_loop_action_sequence.is_empty() {
        return Err(BoundedOfferRefusal::DrivingSequenceNotEmpty);
    }
    // (2) The ring sampler gates on `Priority{active_player}`, so requiring the proposer to
    // BE the active player is what establishes they held priority at every sampled frame.
    // It deliberately does NOT claim the proposer benefits from or controls the loop:
    // CR 732.2a is explicit that the ending point "need not be the player proposing the
    // shortcut" and that the described sequence is "for all players" (both verbatim). That a
    // non-benefiting BYSTANDER may therefore propose is an inference from those clauses, not
    // a quotation of them; its in-tree precedent is `analysis::loop_check::ShortcutProposal`'s
    // own doc — "a player may propose a shortcut whose deterministic outcome wins the game
    // for another player." (CR 732.3's fragmented-loop rule is a CONTRAST, not support.)
    if proposer != state.active_player {
        return Err(BoundedOfferRefusal::ProposerIsNotActivePlayer);
    }
    // (2b) CR 732.2a: with fewer than two retained frames there is no window, hence no
    // certificate is reachable at all — basis A needs `span >= 1` and basis B needs three
    // frames. Refusing HERE, before anything is materialized or classified, is what makes
    // "nothing spends before the ring gate" structural: the verdict container does not yet
    // exist for a consumer to ask.
    if state.loop_detect_ring.len() < 2 {
        return Err(BoundedOfferRefusal::NoCertification);
    }

    // (4) CERTIFICATION — two bases, first match wins, NEVER combined.
    //
    // Basis A is a fifth copy of the ring `find_map` scan (`:481` the `On` reconcile, `:668`
    // Path B, `:710` Path C, `:808` `find_live_loop_winner`). Recorded, not hidden: the repo
    // already made this call at `find_live_loop_winner`'s own doc — "a deliberate, isolated
    // copy … the `On` arm stays VERBATIM (byte-identity gate)" — and retargeting the four
    // shipped walks would edit byte-identity-gated paths inside a feature commit. Newest
    // prior first: the most recent recurrence is the least extrapolation.
    //
    // TWO PARALLEL VECS OVER ONE INDEX SPACE. They are built from the same `VecDeque` in the
    // same order, so `ring.len() == ring_live.len()` by construction and `span`, `[idx..]`
    // and basis B's `n - 1 - k` are unchanged expressions on both.
    // CR 104.4b comparand half — every certification reader, unchanged in value.
    let ring: Vec<&GameState> = state
        .loop_detect_ring
        .iter()
        .map(|f| &f.normalized)
        .collect();
    // CR 732.2a evaluable half — the period-touch domain. A normalized frame zeroes
    // `next_object_id` and strips trigger identity, so it is a comparand and never a board to
    // evaluate an announcement or a resolution against.
    let ring_live: Vec<&GameState> = state.loop_detect_ring.iter().map(|f| &f.live).collect();

    // The per-mint verdict door, constructed immediately after the ring materialization and
    // before anything asks it. It allocates an empty memo and a `u32` budget and classifies
    // NOTHING; the seam mints its own capability token because it, not its caller, resolves
    // the cap.
    let cap_value = match cap {
        ProbeCap::Shipped => PROBE_BUDGET,
        ProbeCap::Lowered(n) => n.min(PROBE_BUDGET),
        ProbeCap::RaisedTwiceLinks => 2 * state.stack.len() as u32,
    };
    let mut verdicts = PeriodVerdicts::for_period_with_cap(
        &ring_live,
        state,
        proposer,
        cap_value,
        CapAuthority(()),
    );

    // ONE exit for the meter: everything that can ask the door lives below, and the snapshot
    // is taken here rather than at each refusal so a future refusal arm cannot forget it.
    // `certification` is the exception BY NECESSITY — it is not a counter the container
    // accumulates but a choice made mid-walk, so it is written where it is decided and read
    // back here through the same one exit.
    let outcome = certified_bounded_cycle_offer(
        state,
        mandatory,
        proposer,
        &ring,
        &ring_live,
        &mut verdicts,
        &mut meter.certification,
    );
    meter.spent = verdicts.spent();
    meter.denied = verdicts.denied();
    meter.conjunct6_asks = verdicts.conjunct6_asks();
    meter.conjunct6_frozen_skips = verdicts.conjunct6_frozen_skips();
    meter.conjunct4_scans = verdicts.conjunct4_scans();
    outcome
}

/// CR 732.2a: certification (step 4/4b), the choice gate and the bound — everything that can
/// ask the verdict door, split out so its caller owns exactly one meter snapshot.
#[allow(clippy::too_many_arguments)]
fn certified_bounded_cycle_offer<'a>(
    state: &'a GameState,
    mandatory: bool,
    proposer: PlayerId,
    ring: &[&'a GameState],
    ring_live: &[&'a GameState],
    verdicts: &mut crate::analysis::resource::PeriodVerdicts<'a>,
    cert_out: &mut Option<crate::analysis::resource::PeriodCertification>,
) -> Result<WaitingFor, BoundedOfferRefusal> {
    use crate::analysis::decision_template::{
        DecisionPoint, DecisionPointKind, DecisionSlot, IterationCount,
    };
    use crate::analysis::resource::{
        certified_period_touch, PeriodCertification, PeriodTouch, PeriodicDelta, ResourceVector,
    };
    use crate::types::ability::TargetRef;

    let cur = ResourceVector::snapshot(state);
    // Written as an explicit newest-first walk rather than `find_map` because the candidate
    // body now threads `&mut verdicts` and carries an owned per-candidate `PeriodTouch` out.
    let mut basis_a: Option<(
        &GameState,
        Vec<DecisionPoint>,
        PeriodTouch<'_>,
        PeriodicDelta,
    )> = None;
    for idx in (0..ring.len()).rev() {
        // The span, in RETAINED RING FRAMES, that this candidate pair covers.
        // `ring.last()` is the sample `pass_priority_once_with_pipeline` recorded at THIS
        // beat, before the bridge ran, so the newest frame is the current state and the
        // span from `ring[idx]` is `len - 1 - idx`.
        //
        // A span of 0 is the pair `state` against its own snapshot. It is already refused
        // by `net_progress_for` on the resulting zero delta in every production
        // trajectory, but it is refused HERE too, explicitly: `materialize_fixed_shortcut`
        // now DELIMITS a committed cycle by this count, and a published `0` would mean
        // "one repetition spans no frames", which no drive can honour. Fail closed on the
        // degenerate pair rather than rely on a downstream conjunct to catch it.
        //
        // EVALUATED FIRST, before the window is built, touched or minted from — `span >= 1`
        // is `window.len() >= 2` identically, so this guard is also what keeps a degenerate
        // window out of the touch and the mint.
        let span = ring.len() - 1 - idx;
        if span < 1 {
            continue;
        }
        let prior = ring[idx];
        let window = &ring_live[idx..];
        // Built under `BoardCovered` unconditionally, because step 4 does not yet know which
        // disjunct will match; step 4b keeps it or rebuilds it.
        let touch_cover = certified_period_touch(window, state, PeriodCertification::BoardCovered);
        // (3) The published per-iteration choices (5a's single authority), now enumerated
        // over the CERTIFIED PERIOD's announced pairs rather than over the offer-beat stack.
        let points = bounded_cycle_pin_slots_for_window(&touch_cover, proposer);
        let slots: Vec<DecisionSlot> = points.iter().map(|p| p.slot.clone()).collect();
        let delta = ResourceVector::delta(&ResourceVector::snapshot(prior), &cur);
        // The existing disjunction, written as an `if / else if` that RECORDS the matching
        // disjunct instead of discarding it. Semantics are byte-for-byte the `||` it
        // replaces: the equality arm is still evaluated first and `_pinned` still runs only
        // when it fails. The two disjuncts are mutually exclusive — equality compares the
        // stack exactly (constant depth) while cover's item (2) forces strictly growing depth
        // — so "which disjunct" is a total function with no both-matched case.
        let cert = if crate::analysis::resource::loop_states_equal_modulo_resources(prior, state) {
            Some(PeriodCertification::BoardEqualOnly)
        } else if crate::analysis::resource::loop_states_cover_modulo_growth_pinned(
            prior,
            state,
            proposer,
            &slots,
            &touch_cover,
            verdicts,
        ) {
            Some(PeriodCertification::BoardCovered)
        } else {
            None
        };
        let Some(cert) = cert else {
            continue;
        };
        if !delta.net_progress_for(proposer) {
            continue;
        }
        // (4b) THE CERTIFIED TOUCH, keyed to the disjunct that actually matched. The cover
        // disjunct supplies both premises the frozen subtraction rests on; the equality
        // disjunct supplies only the depth one, so its period is rebuilt with the exemption
        // withdrawn. `announced` is identical on both, so the mint is not re-derived.
        let touch = match cert {
            PeriodCertification::BoardCovered => touch_cover,
            c => certified_period_touch(window, state, c),
        };
        // Recorded HERE and not at the `if / else if`: a candidate that certifies and then
        // dies on `net_progress_for` is not the certificate the mint carries forward, and a
        // meter that named it would attribute the offer to a pair the walk discarded.
        *cert_out = Some(cert);
        basis_a = Some((
            prior,
            points,
            touch,
            PeriodicDelta {
                // MEASURED span, not the former hardcoded `1`. The walk is `.rev()`, so
                // `idx` is usually `len - 2` and the span is 1 — but it is 1 by
                // MEASUREMENT, not by assumption, and it is NOT always 1: the
                // `interactive_3p_subset_lethal_does_not_crown` fixture's repetition
                // spans TWO frames (a gain-life resolution then a lose-life one), and
                // under the old hardcode its accepted drive committed nothing at all.
                frames_per_period: span as u32,
                delta,
                victim_slot: Vec::new(),
            },
        ));
        break;
    }
    // Basis B consults NO board predicate: a period whose frame-deltas repeated twice in the
    // retained ring is a signature on its own. Its certifying pair is the ring frame one
    // period back and the ring's newest frame — the very pair `ring_delta_signature`
    // measured, so the certificate's residual is derived from the same window as the delta.
    //
    // ⚠ WHAT ACTUALLY DECIDES A vs B ON A GROWING CASCADE — measured, because the intuitive
    // answer is wrong and cost this lane a mislabelled row. It is NOT "resource-purity": a
    // pure life↔life drain does not take basis A by recurring. BOTH known life-drain
    // fixtures GROW their stack every period, so `loop_states_equal_modulo_resources` is
    // FALSE on the certifying pair of each, and NEITHER certifies through the equal disjunct:
    //
    // * the basis-A fixture (`multiplayer_pure_life_drain_offers_at_three_and_four_players`,
    //   Blight-Priest + Exquisite Blood) certifies through
    //   `loop_states_cover_modulo_growth_pinned` at `ring[1]` — `stack[2->3]` at 3 players,
    //   `stack[3->5]` at 4. The one `eq == true` pair on its ring carries a zero δ and dies on
    //   `net_progress_for`, not on the board predicate.
    // * the basis-B fixture (`dina_untargeted_drain_4p_offers_at_three_live_opponents`) has
    //   that SAME disjunct vetoed at cover **gate (5)** — the off-stack fire-time condition
    //   guard — by a `ModifyCost { Reduce, {2} }` static on a library card, gated on
    //   `LifeGainedThisTurn { Controller } >= 1`: a projected axis read at fire time.
    //
    // So the discriminant is a FIRE-TIME CONDITION READING A PROJECTED AXIS, not the shape of
    // the resources the loop moves. And the composition worth remembering: gate (5)'s
    // `scope.cast_card_ids` relief — which exists precisely to excuse a self-cost modifier on
    // a card the window provably never casts — CANNOT fire for this class, because step (1b)
    // requires `last_loop_action_sequence` to be EMPTY, so `window_cast_card_ids` returns
    // `None` (no proof ⇒ scan everything). The requirement that DEFINES the bounded class is
    // exactly what disables the relief that would otherwise let cover succeed. Two
    // individually-correct constraints composing into a refusal neither intended.
    //
    // ⚠ NEVER attribute the basis from `frames_per_period`. BOTH bases now MEASURE it — basis A
    // from the certifying prior's ring index above, basis B from `ring_delta_signature`'s
    // derived `k` — so the two publish overlapping value ranges and NO value discriminates in
    // either direction. (Before fix round 1 basis A published a hardcoded `1`, which made
    // `!= 1` sufficient-but-not-necessary for "not basis A"; that inference is now dead too,
    // since a basis-A span of 2 is exactly what the `interactive_3p_subset_lethal_does_not_crown`
    // fixture publishes.) The only sound attribution is a discriminating probe: force
    // `ring_delta_signature` to return `None` (basis B's sole entry point is the `None =>`
    // arm below) — the rows that survive are basis A, the rows that fail are basis B.
    let (cert_prior, points, touch, mut periodic) = match basis_a {
        Some(hit) => hit,
        None => {
            let (k, delta) = crate::analysis::resource::ring_delta_signature(state)
                .ok_or(BoundedOfferRefusal::NoCertification)?;
            let n = ring.len();
            let start = n
                .checked_sub(1 + k as usize)
                .ok_or(BoundedOfferRefusal::NoCertification)?;
            let cert_prior = *ring
                .get(start)
                .ok_or(BoundedOfferRefusal::NoCertification)?;
            // Basis B EXEMPTS NOTHING, and that is derived rather than cautious: its
            // certificate consults no board predicate, so it supplies no premise that the
            // period cannot SHRINK the stack — a stack draining from the top under a ticking
            // monotone resource satisfies the delta signature and leaves a large frozen
            // bottom prefix the drain will reach. `announced` is unchanged; only the
            // subtraction is withdrawn.
            let window = ring_live
                .get(start..)
                .ok_or(BoundedOfferRefusal::NoCertification)?;
            let touch =
                certified_period_touch(window, state, PeriodCertification::ResourceSignatureOnly);
            *cert_out = Some(PeriodCertification::ResourceSignatureOnly);
            let points = bounded_cycle_pin_slots_for_window(&touch, proposer);
            (
                cert_prior,
                points,
                touch,
                PeriodicDelta {
                    frames_per_period: k,
                    delta,
                    victim_slot: Vec::new(),
                },
            )
        }
    };

    // (5) CR 732.2a: the conjunct that proves this class is DISJOINT from Path C's
    // revocable-∞ advantage mark. An `Advantage` cycle drives nobody toward a CR 704
    // threshold, so it has no bound to state and belongs to the other seam.
    if crate::analysis::loop_check::classify_win_kind(proposer, &periodic.delta)
        == crate::analysis::loop_check::WinKind::Advantage
    {
        return Err(BoundedOfferRefusal::AdvantageOnlyCycle);
    }

    // (6) CR 732.2a "predictable results": every per-iteration choice the cycle opens must be
    // a SPECIFIED one. `stack_choices_are_all_specified` is that question's authority — it
    // shares gates (3)/(6)'s own predicates and pin relief verbatim, so the relief here can
    // never be coarser than the mint that published the slots.
    //
    // Its own conjunct, not folded into step 4: basis A's disjunction may have matched on
    // exact recurrence (which says nothing about choices) and basis B consults no board
    // predicate at all. And deliberately NOT a second `loop_states_cover_modulo_growth_pinned`
    // call: on the dina 4p drain that predicate refuses 66 of the beats this seam reaches,
    // on a BOARD fact, without ever examining a choice.
    //
    // ⚠ ATTRIBUTION CORRECTED, and deliberately scoped to what was RE-MEASURED. An earlier
    // revision named the refuser as the cover predicate's item (1) `object_resource_axes_match`
    // STRICT compare. At the MINT/OFFER beat that is FALSE: instrumenting the cover gates on
    // dina's offer beat shows `object_resource_axes_match == true` at every gate-(1) refusal
    // observed (187 of 187 across the dina and the ≥3p life-drain drives); the actual refusals
    // are gate (5) (an off-stack fire-time condition reading a projected axis) and, on older
    // ring pairs, gate (1)'s `loop_states_equal` on the stack-cleared projected board. The 66
    // NON-OFFERING beats the original count came from were NOT re-measured in that round, so
    // the item-(1) attribution may still hold for them — it is left standing for that
    // population rather than overwritten with an unmeasured claim. Either way the conjunct's
    // JUSTIFICATION is unchanged: cover refuses on board facts, and this seam asks about
    // choices.
    let slots: Vec<DecisionSlot> = points.iter().map(|p| p.slot.clone()).collect();
    if !crate::analysis::resource::stack_choices_are_all_specified(
        state,
        proposer,
        &slots,
        Some(&touch),
        verdicts,
    ) {
        return Err(BoundedOfferRefusal::UnspecifiedChoiceWindow);
    }

    // (7) THE BOUND. `declarable_victims` is the union of the published slots' legal targets
    // — EMPTY for the untargeted class, where the victims are already in `delta.life`.
    let declarable_victims: Vec<PlayerId> = {
        let mut v: Vec<PlayerId> = points
            .iter()
            .filter_map(|p| match &p.kind {
                DecisionPointKind::Targets { legal_targets, .. } => Some(legal_targets),
                _ => None,
            })
            .flatten()
            .filter_map(|t| match t {
                TargetRef::Player(p) => Some(*p),
                _ => None,
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    // CR 704.5a: what ONE repetition charges to whichever seat a slot's pin names. The
    // max-vs-sum reasoning, the gain clamp and the fail-closed direction live on the
    // function; `elimination_bounds` then sums the published slots per declarable victim.
    // Extracted rather than inlined so the fork has a callable seam — `victim_slot` is empty
    // on every trajectory that offers today, so this value is dropped in production and only
    // `worst_seat_life_loss_is_the_max_seat_never_the_sum` discriminates max from sum.
    let worst_seat_life_loss: i64 = periodic.delta.worst_seat_life_loss();
    periodic.victim_slot = points
        .iter()
        .filter(|p| matches!(p.kind, DecisionPointKind::Targets { .. }))
        .map(|p| (p.slot.clone(), worst_seat_life_loss))
        .collect();
    // `.cloned()`, not `.copied()`: `(DecisionSlot, i64)` is not `Copy`.
    let slot_magnitude: std::collections::BTreeMap<DecisionSlot, i64> =
        periodic.victim_slot.iter().cloned().collect();
    let max_iterations =
        periodic
            .delta
            .elimination_bounds(state, &declarable_victims, &slot_magnitude);
    // A bound of 0 states no legal repetition. A bound AT the cap states no narrowing at all
    // — this producer's whole claim is that it measured a CR 704.5a / CR 704.5c / CR 104.3c
    // threshold inside the loop, so an unnarrowed result belongs to another seam. Checking
    // the closed range here makes `schema.is_bounded()` true BY CONSTRUCTION for every offer
    // this function mints, instead of an inference from step 5's `Advantage` rejection.
    if !(1..MAX_SHORTCUT_CYCLES).contains(&max_iterations) {
        return Err(BoundedOfferRefusal::NoNarrowedLegalCount);
    }

    // (8) The certificate, with the two fields the bounded class states differently from
    // Path A's spelled out at the site rather than mutated after the fact.
    // `cert_current` is the live `state` on both bases, exactly as before: `build_cert`'s only
    // use of the pair is `board_delta`, a comparand read.
    let base = build_cert(cert_prior, state, &periodic.delta, proposer);
    let certificate = crate::analysis::loop_check::LoopCertificate {
        per_cycle: Some(periodic),
        // CR 732.5: honest, and currently read by nothing in production — a loop nobody can
        // break is still not forced to end, so this records the fact without acting on it.
        mandatory,
        ..base
    };

    // (9) The schema. `Fixed(max_iterations)` is the SUGGESTION and `max_iterations` the
    // CEILING; the declare handler rejects any `Fixed(n)` above it and rejects `UntilLethal`
    // outright, both already shipped. The pre-built `points` go in directly — the bounded
    // path never calls `pinned_decisions_to_points`, whose legal sets are derived FROM the
    // declared pins and would let a declaration ratify itself.
    let schema = build_shortcut_schema(
        points,
        IterationCount::Fixed(max_iterations),
        max_iterations,
    );
    Ok(WaitingFor::LoopShortcut {
        proposer,
        predicted_winner: None,
        certificate,
        schema,
    })
}

/// CR 704.5a / CR 704.5c: a determinate lethal drain (0-or-less life / 10-poison) repeats
/// UntilLethal; every other CR 732.1b win seeds a `Fixed(1)` frontend count picker. Extracted
/// as a pure classifier so the exhaustive `WinKind` mapping is unit-testable without a
/// `GameState`.
fn shortcut_iteration_count(
    win_kind: crate::analysis::loop_check::WinKind,
) -> crate::analysis::decision_template::IterationCount {
    use crate::analysis::decision_template::IterationCount;
    use crate::analysis::loop_check::WinKind;
    match win_kind {
        WinKind::LethalDamage | WinKind::PoisonLoss => IterationCount::UntilLethal,
        WinKind::Decking | WinKind::ExtraTurns | WinKind::ImmediateWin | WinKind::Advantage => {
            IterationCount::Fixed(1)
        }
    }
}

/// CR 732.2a: reify a carried pin list into the READ-side decision points an offer publishes.
/// `pins` is the single-authority decision list (`build_recast_template` output for the
/// object-growth path; empty for a non-targeted drain) — never re-derived here. Legal sets come
/// from live engine queries (`is_convoke_eligible`); the frontend computes nothing.
fn pinned_decisions_to_points(
    pins: &[crate::analysis::decision_template::PinnedDecision],
    state: &GameState,
    controller: PlayerId,
) -> Option<Vec<crate::analysis::decision_template::DecisionPoint>> {
    use crate::analysis::decision_template::{DecisionPoint, DecisionPointKind, PinnedDecision};
    let mut points = Vec::with_capacity(pins.len());
    for pin in pins {
        let point = match pin {
            // CR 603.3b: trigger ordering is not a loop-declaration choice — no read-side peer.
            PinnedDecision::Order { .. } => continue,
            // CR 702.51a: the untapped creatures the controller may tap for convoke. Sorted by
            // the public inner id: `im::HashMap::values()` order is nondeterministic and this Vec
            // serializes to the wire (cf. `resolve_source`'s `min_by_key` for the same reason).
            PinnedDecision::ConvokeTaps { slot } => {
                let mut tappable: Vec<crate::types::identifiers::ObjectId> = state
                    .objects
                    .values()
                    .filter(|o| o.is_convoke_eligible(controller))
                    .map(|o| o.id)
                    .collect();
                tappable.sort_by_key(|id| id.0);
                DecisionPoint {
                    slot: slot.clone(),
                    kind: DecisionPointKind::ConvokeTaps { tappable },
                }
            }
            // FIX-1 (B1): reify the recorded fixed in-cycle choices. The drive replays these SAME
            // pins via `decision_template::resolve` (CR 608.2b ByIdentity live re-binding), so the
            // offer schema carries their read-side dual (one template = single source of truth).
            // CR 608.2b: resolve each pinned target to its live legal `TargetRef` — the pinned
            // identity IS the singleton legal set (a fixed declinable ∞ offer, no FE re-selection).
            PinnedDecision::Targets { slot, targets } => {
                // CR 732.2a: a proposal must describe a sequence "that may be legally taken
                // based on the current game state". If ANY pinned target no longer resolves,
                // the offer must be WITHDRAWN, not published — the `?` below is that
                // withdrawal. `filter_map`ping the failure away instead would publish a point
                // with a short `legal_targets` under `min_targets = targets.len()`: a
                // self-inconsistent, UNDECLARABLE point that fails downstream as
                // `IllegalPinValue`/`UnknownChoice` rather than as "there is no offer".
                // Dropping the point entirely is also wrong — it would let
                // `predictability_gate`'s coverage check pass trivially.
                let legal_targets: Vec<crate::types::ability::TargetRef> = targets
                    .iter()
                    .map(|t| {
                        crate::analysis::decision_template::resolve_target_ref(t, slot, 0, state)
                    })
                    .collect::<Option<Vec<_>>>()?;
                let count = targets.len().min(u32::MAX as usize) as u32;
                DecisionPoint {
                    slot: slot.clone(),
                    kind: DecisionPointKind::Targets {
                        legal_targets,
                        min_targets: count,
                        max_targets: count,
                        ordered: true,
                    },
                }
            }
            // CR 608.2d: the latched mana color — a read-only fixed point (no legal set to bound).
            PinnedDecision::ManaColor { slot, color } => DecisionPoint {
                slot: slot.clone(),
                kind: DecisionPointKind::ManaColor { color: *color },
            },
            PinnedDecision::Mode { slot, indices } => {
                let mut available_modes = indices.clone();
                available_modes.sort_unstable();
                available_modes.dedup();
                let count = indices.len().min(u32::MAX as usize) as u32;
                DecisionPoint {
                    slot: slot.clone(),
                    kind: DecisionPointKind::Mode {
                        available_modes,
                        min_modes: count,
                        max_modes: count,
                        allow_repeats: indices.len()
                            != indices
                                .iter()
                                .collect::<std::collections::HashSet<_>>()
                                .len(),
                    },
                }
            }
            PinnedDecision::MayChoice { slot, .. } => DecisionPoint {
                slot: slot.clone(),
                kind: DecisionPointKind::MayChoice,
            },
            PinnedDecision::UnlessBreak { slot, .. } => DecisionPoint {
                slot: slot.clone(),
                kind: DecisionPointKind::UnlessBreak,
            },
        };
        points.push(point);
    }
    Some(points)
}

/// CR 115.2 + CR 732.2a: does the ability's HEAD effect declare the "target opponent" PLAYER
/// filter — a `Typed` filter with no type constraints, no object properties, and
/// `controller: Opponent`, the shape `game::targeting::find_legal_targets` collapses to
/// players-only (`crates/engine/src/game/targeting.rs:192-193`)?
///
/// SHAPE ACCEPTANCE ONLY, and the `bool` return is what enforces it: the published legal
/// set must come from the announcement authority (`ability_utils::build_target_slots`), never
/// from here, because this predicate reads the HEAD effect's filter while the choice being
/// announced can belong to a CHAINED sub-ability (CR 601.2c reached via CR 603.3d). Handing
/// the filter back would re-open exactly that divergence, so it is not handed back.
///
/// What the `controller` conjunct contributes, and nothing else does: `controller: You` /
/// `None` ALSO collapses to players, so an all-`Player` legal set alone would admit a single
/// forced seat, which is not the per-opponent choice a bounded drain cycle pins. Measured on
/// the 3p drain board: `Typed{[], You, []}` builds ONE mandatory slot whose legal set is
/// `[Player(0)]`, and only this conjunct rejects it.
///
/// What the `type_filters` / `properties` conjuncts contribute (issue #2004 — "target token
/// you control" must not collapse to a player) is the MIRROR of that chained-slot
/// divergence: an object-shaped HEAD effect whose single announced slot is nonetheless a
/// player choice. When the head announces its own slot, an object-shaped filter is already
/// rejected upstream — it either enumerates OBJECTS (the caller's all-`Player` conjunct) or
/// enumerates nothing, making `build_target_slots` return `Err` (the caller's cardinality
/// conjunct). It is only when the head announces NOTHING
/// (`TargetChoiceTiming::Resolution`) and a chained `target opponent` sub-ability supplies
/// the one slot that these two conjuncts become the sole rejector — which is the board
/// `bounded_cycle_pin_slots_conjuncts_are_each_load_bearing` measures them on.
fn declares_opponent_player_target(ability: &crate::types::ability::ResolvedAbility) -> bool {
    use crate::types::ability::{ControllerRef, TargetFilter};
    let Some(TargetFilter::Typed(tf)) = ability.effect.target_filter() else {
        return false;
    };
    tf.type_filters.is_empty()
        && tf.properties.is_empty()
        && tf.controller == Some(ControllerRef::Opponent)
}

/// What ONE accepted stack entry publishes: the slot keys, plus the legal set the
/// ANNOUNCEMENT authority itself built for the target slot.
pub(crate) struct EntryPinSlots {
    /// CR 115.2 target choice — `index: 0`. `None` for shape (B), the may-only entry:
    /// announcing it surfaces NO choice at all (`targets.is_empty()` and zero built slots),
    /// so there is no CR 601.2c announcement choice for a pin to specify.
    pub(crate) target: Option<crate::analysis::decision_template::DecisionSlot>,
    /// CR 603.5 "may" gate — `index: 1`, `Some` only if `ability.optional` — the mint
    /// additionally refuses on recipient, stored auto-choice and prompt-cardinality grounds
    /// (see the `may` mint below), so `None` here does NOT imply the ability is mandatory.
    /// `DecisionSlot`'s sub-index disambiguates two choices of ONE ability instance (target
    /// vs. may gate).
    pub(crate) may: Option<crate::analysis::decision_template::DecisionSlot>,
    /// The legal set of the ONE announcement slot, taken VERBATIM from
    /// `ability_utils::build_target_slots` — the same authority that decided there is
    /// exactly one mandatory choice. Deriving it a second time from the head effect's
    /// filter would let the two disagree about WHICH choice is being published, which is
    /// the same class of divergence the cardinality conjunct closes about HOW MANY.
    /// Empty for shape (B), which publishes no target slot to carry a legal set for.
    pub(crate) legal_targets: Vec<crate::types::ability::TargetRef>,
}

/// CR 732.2a: the per-iteration choice slots ONE stack entry publishes for `proposer`, or
/// `None` when it publishes none.
///
/// SINGLE AUTHORITY, and that is the whole point of its existence: the MINT
/// ([`bounded_cycle_pin_slots_for_window`]) maps it over the certified period's announced
/// pairs, of which `state.stack` is the zero-window degenerate case, and the RELIEF
/// (`analysis::resource`'s CR 732.2a gate-(3)/(6) pin skip) calls it for one entry. Because
/// both sides ask the same function, the relief predicate cannot be COARSER than the mint
/// predicate — relieving a verdict the published pin does not specify is impossible by
/// construction rather than by convention.
///
/// The acceptance conjuncts, in the order the EXTENSION POINT's preconditions name them:
/// (c) `entry.controller == proposer` — CR 732.2a leaves every OTHER player owning their own
/// choices, so an opponent-controlled entry is never pinnable; the entry is a triggered
/// ability (a spell / activated ability re-announces from scratch); ANNOUNCING it requires
/// either exactly one mandatory choice over players whose head effect declares the
/// player-target shape (shape (A): asked of the announcement authority itself,
/// `ability_utils::build_target_slots` — the function the relief's own
/// `forced_unique_targeting` rebuilds slots with — rather than of a proxy, plus
/// [`declares_opponent_player_target`]), or NO announcement choice at all (shape (B):
/// `targets.is_empty()` and zero built slots); and its source object still exists, so the
/// slot can re-bind (CR 400.7 incarnation, fail-closed on absence).
///
/// SCOPE OF THE ANSWER: because the relief is a `continue` at gate (3), the relief
/// predicate must be no coarser than EVERY fact `stack_entry_has_no_ordering_input`
/// rejects on — not just the target one. Correspondence, in that function's own order:
/// entry kind (the destructure below), `pending_trigger_entry` (the ONE state-dependent
/// fact, enforced at the relief so this enumerator stays pure — see the block below),
/// `multi_target` / `distribution` / `target_constraints` (the block below), and the
/// target choice itself, which is the one fact the published slot actually answers.
pub(crate) fn entry_publishes_pin_slots(
    state: &GameState,
    entry: &StackEntry,
    proposer: PlayerId,
) -> Option<EntryPinSlots> {
    use crate::analysis::decision_template::DecisionSlot;
    if entry.controller != proposer {
        return None;
    }
    let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
        return None;
    };
    // The published slot answers ONE target (`min_targets: 1, max_targets: 1` below). A
    // variable-count choice (CR 601.2c "if the spell has a variable number of targets, the
    // player announces how many"), a divide/distribute assignment (CR 601.2d), or a
    // cross-target constraint (CR 601.2c "the same target can't be chosen multiple times" /
    // "must be chosen") — all reached for a triggered ability via CR 603.3d — is
    // announcement-time ordering input NO published slot specifies. These are the ABILITY's
    // own facts, so they live here, where mint and relief share them: the gate-(3) relief
    // is a `continue` that discharges the whole of `stack_entry_has_no_ordering_input`,
    // which rejects on each of them independently of its target check.
    //
    // The fourth fact that function rejects on — `pending_trigger_entry == entry.id`,
    // CR 603.3c mid-construction — is deliberately NOT here: it is a property of the
    // COMPARED STATE, not of the offer's schema, and reading it would break this
    // enumerator's PROMPT-state independence (see [`bounded_cycle_pin_slots`]: it never
    // reads `waiting_for`, nor `pending_trigger_entry`, which is set exactly while a prompt
    // is up). NOT a claim of purity over a three-field surface: the CR 603.5 recipient
    // conjunct below resolves a player through `optional_prompt_player` →
    // `resolve_effect_player_ref`, which reaches ELEVEN distinct `GameState` fields —
    // `state.players`, `state.seat_order`, `state.format_config`, `state.objects`,
    // `state.lki_cache`, `state.stack`, `state.current_trigger_event`,
    // `state.last_created_token_ids`, `state.last_revealed_ids`,
    // `state.last_zone_changed_ids` and `state.resolution_stack`. The contract is narrower
    // and exact — the mint is a function of the BOARD, never of the PROMPT — and it is what
    // keeps the mint's verdict stable across a prompted and an unprompted beat.
    // It is set exactly
    // while a `TriggerTargetSelection` prompt is up, so a mint that read it would publish
    // nothing on a prompted board — measured on dump B, where it zeroes the emblem slot.
    // It is enforced at the relief instead (`analysis::resource::entry_target_choice_is_pinned`),
    // which makes the relief strictly NARROWER than the mint — never coarser.
    if ability.multi_target.is_some()
        || ability.distribution.is_some()
        || !ability.target_constraints.is_empty()
    {
        return None;
    }
    // THE ANNOUNCEMENT AUTHORITY, not a proxy for it. Everything above is an ability FACT;
    // this is the only conjunct that asks the questions the published point actually
    // answers — "how many choices does announcing this entry require, is each one
    // mandatory, and WHICH objects or players may be chosen?" `Effect::target_filter()`
    // (below) cannot answer any of them: it reports the head effect's filter, while
    // CR 601.2c ("if the spell uses the word 'target' in multiple places, the same object
    // or player can be chosen once for each instance") — reached for a triggered ability
    // via CR 603.3d — makes a CHAINED sub-ability's own target a SECOND independent choice,
    // with its OWN legal set. `build_target_slots` is the function
    // `stack_entry_has_no_ordering_input` itself rebuilds slots with (via
    // `forced_unique_targeting`), so mint and relief now measure the same quantities.
    //
    // Exactly one MANDATORY slot over PLAYERS, and each of the three parts is load-bearing
    // — the first two discriminated by
    // `bounded_cycle_pin_slots_requires_a_single_mandatory_announcement_slot`, the third by
    // `bounded_cycle_pin_slots_legal_set_comes_from_the_announcement_authority`:
    // * `len() == 1` — a chained second "target" (2 slots) or an effect whose filter the
    //   SLOT BUILDER declines (0 slots: `triggers::extract_target_filter_from_effect`
    //   carves out `Sacrifice`/`UnattachAll`/… for which `Effect::target_filter()` still
    //   returns `Some`, and `target_choice_timing == Resolution` surfaces no stack slot at
    //   all) would leave the published `min/max_targets: 1` contradicting the announcement.
    // * `!optional` — `ability.optional_targeting` ("up to one target") makes the real
    //   minimum ZERO (CR 601.2c), and its slot may legally carry an EMPTY legal set, so a
    //   `min_targets: 1` point would over-state the choice the offer specifies.
    // * every legal target is a PLAYER (CR 115.2) — the head effect can declare the
    //   player shape while the ONE slot the announcement actually surfaces belongs to a
    //   chained sub-ability targeting OBJECTS (measured: head `LoseLife` at
    //   `TargetChoiceTiming::Resolution` contributing 0 slots + a chained
    //   `LoseLife{Typed{[Creature]}}` contributing 1, legal set three objects). A
    //   `TargetPin::Player` cannot specify such a choice, so publishing it would hand
    //   gate (3)'s `continue` a slot no pin can answer.
    //
    // `Err` (no legal target, CR 603.3d) also yields `None` — fail-closed, matching this
    // function's contract that the schema can only ever UNDER-publish. Purity survives:
    // `build_target_slots` never reads `state.waiting_for` (its only hit in
    // `ability_utils.rs` is a test at `:7722`).
    let source = object_decision_source(state, entry.source_id)?;
    // CR 603.5 + CR 732.2a: `entry.controller == proposer` above bounds who OWNS the entry;
    // it does NOT bound who the resolver ASKS, nor WHETHER it asks, nor HOW MANY TIMES.
    // Three mint-time conjunct groups, all FAIL-CLOSED pre-filters on the ONE gate a
    // `MayChoice` pin is for — the CR 603.5 gate inside `resolve_chain_body`
    // (`effects/mod.rs`, the `if ability.optional && !has_kind_driven_repeat(..)` block).
    // THIS IS THE ONE PLACE `may` IS MINTED, so the guards cover shape (A) and shape (B)
    // together rather than being restated per shape. Soundness over the OTHER FOUR
    // production producers of `WaitingFor::OptionalEffectChoice` is NOT claimed here; it is
    // discharged at the consumption point, where the instrument is total.
    //
    // (a) RECIPIENT. `optional_prompt_player` is THIS gate's own recipient authority —
    //     five of its branches route to a NON-controller and the last is EFFECT-AGNOSTIC
    //     (CR 503.1a + CR 608.2d, the `scoped_player` class whose printed member is Braids,
    //     Conjurer Adept — "At the beginning of each player's upkeep, that player may put an
    //     artifact, creature, or land card from their hand onto the battlefield."), so
    //     asking the same function the gate asks keeps THIS pair from drifting. Without
    //     it a proposer's pin can be spent as another seat's CR 603.5 choice.
    // (b) SECOND AUTHORITY. A stored "don't ask again" auto-choice ALREADY ANSWERS this
    //     may and the gate returns BEFORE setting any prompt, so a pin minted here would
    //     be silently unused — invisible even to a fail-closed inject arm. The key is
    //     built exactly as the gate builds it; `player` is `proposer` only because `&&`
    //     short-circuits left to right and (a) has already proved them equal. Present ⇒
    //     refuse; `may_trigger_origin: None` ⇒ no key exists ⇒ nothing to refuse.
    // (c) CARDINALITY. CR 732.2a: the shortcut describes THE sequence of choices, so one
    //     published slot may stand for exactly ONE CR 603.5 prompt. Production suppresses
    //     the single up-front gate for three `repeat_for` shapes and re-fires optionality
    //     PER ITERATION (CR 608.2c + CR 608.2d) instead. `has_kind_driven_repeat` keys on
    //     `repeat_for` ALONE — no `Effect` restriction — so an optional `PutCounter` /
    //     `Draw` / `Token` of that shape would otherwise mint ONE slot for N prompts. Ask
    //     production's own three predicates rather than re-deriving them here, which is
    //     the same authority-sharing rule (a) follows.
    let may = (ability.optional
        && crate::game::effects::optional_prompt_player(state, ability) == proposer
        && !crate::game::effects::has_kind_driven_repeat(ability)
        && !crate::game::effects::has_member_driven_repeat_after_hydration(state, ability)
        && !crate::game::effects::is_repeated_optional_payment(ability)
        && ability.may_trigger_origin.as_ref().is_none_or(|origin| {
            state
                .may_trigger_auto_choice(&crate::types::game_state::MayTriggerAutoChoiceKey {
                    player: proposer,
                    source_id: ability.source_id,
                    origin: origin.clone(),
                })
                .is_none()
        }))
    .then(|| DecisionSlot {
        source: source.clone(),
        index: 1,
    });
    let mut slots = super::ability_utils::build_target_slots(state, ability).ok()?;
    // SHAPE (B) — may-only. The announcement authority surfaced NO choice, so there is no
    // CR 601.2c target for a pin to specify and the entry publishes its CR 603.5 gate
    // alone. `ability.targets.is_empty()` is what makes "zero built slots" mean "declares
    // nothing" rather than "declared something the builder declined"; `optional` is
    // inherited from the `may` expression, which is `None` without it. A `may` the three
    // conjunct groups above suppressed leaves shape (B) with NO slot at all, so the whole
    // entry publishes `None` — the fail-closed direction.
    if slots.is_empty() {
        if !ability.targets.is_empty() {
            return None;
        }
        return Some(EntryPinSlots {
            target: None,
            may: Some(may?),
            legal_targets: vec![],
        });
    }
    if slots.len() != 1 {
        return None;
    }
    let slot = slots.swap_remove(0);
    if slot.optional
        || !slot
            .legal_targets
            .iter()
            .all(|target| matches!(target, crate::types::ability::TargetRef::Player(_)))
    {
        return None;
    }
    // SHAPE conjunct only — the legal set above is already the announcement authority's, and
    // the predicate's `bool` return makes re-deriving one from the head filter impossible
    // rather than merely discouraged. This rejects a head effect that is not the CR 115.2
    // "target opponent" declaration, which an all-`Player` legal set alone does not
    // (`controller: You` builds exactly one mandatory slot whose legal set is the
    // controller — measured).
    if !declares_opponent_player_target(ability) {
        return None;
    }
    // Shape (A) — targeted. Index 1 is kept for the may slot in BOTH shapes, so slot
    // identity is stable across them.
    Some(EntryPinSlots {
        target: Some(DecisionSlot { source, index: 0 }),
        may,
        legal_targets: slot.legal_targets,
    })
}

/// CR 732.2a: the per-iteration decision points a BOUNDED cycle shortcut must publish for
/// `proposer` — one `Targets` point per proposer-controlled triggered-ability SOURCE that
/// declares a single *player* target (CR 115.2), plus a `MayChoice` point (CR 603.5) when
/// that ability is optional.
///
/// Only a *published* slot is a "specified choice" in CR 732.2a's sense; an unpublished
/// per-opponent choice would make the proposal a conditional action. This is the SINGLE
/// authority for that slot set — the offer's cover call, its schema, the drive's cover call
/// and the per-cycle `predictability_gate` all read the same list.
///
/// A function of the BOARD, never of the PROMPT. NOT a purity claim over a three-field
/// `(state.stack, state.objects, proposer)` surface — that would be false: the CR 603.5
/// recipient conjunct in the body resolves a player through `optional_prompt_player` →
/// `resolve_effect_player_ref`, which reaches ELEVEN distinct `GameState` fields (enumerated
/// at that conjunct). What actually holds, and what the callers rely on, is the narrower
/// PROMPT-independence: it deliberately does **not**
/// read `state.waiting_for`, and it cannot: both production call sites run at
/// `WaitingFor::Priority` (`interactive_loop_bridge`'s destructure, and the drive's
/// `Priority{active}` settle arm), where no prompt and no materialized `legal_targets`
/// exist. The legal set is the one `ability_utils::build_target_slots` built for the
/// accepted announcement slot, carried through verbatim — so the SAME authority answers
/// how many choices exist and which targets each admits. That builder routes this filter
/// shape to the native authority [`crate::game::targeting::find_legal_targets`] (via
/// `ability_utils::legal_targets_for_ability_filter_uncapped`'s `relative_kind.is_none()`
/// / `!needs_ability_context` arm), whose empty-`Typed` players branch already excludes
/// departed seats — CR 800.4 (multiplayer games continue after players leave) + CR 102.1
/// (a player is one of the people in the game): a seat that has left the game is no longer
/// one of them, so it is not choosable by anything; player phasing per the CR 702.26b
/// MIRROR (permanent-phasing text, NEVER authority for players). NOT CR 800.4a, which
/// governs a departed player's objects, control effects and priority — not the legality of
/// a choice. Never a declaration, so the offer still cannot ratify its own pin. Note the
/// enumeration context
/// shifts with the authority: it is now the ABILITY's own `controller`/`source_id`
/// (CR 601.2c — the ability's controller announces its targets) rather than the offer's
/// `proposer` and the stack entry's `source_id`.
///
/// Fail-closed: an entry whose source object is gone yields NO point (rather than a point
/// with an unbindable slot), so the schema can only ever under-publish.
///
/// Class served: every proposer-controlled triggered ability on the stack whose declared
/// target is a player — never a named card. Command-zone sources (CR 114.2 emblems) are
/// included; [`slot_source_prompted`] is the matching half at replay time.
///
/// PER SOURCE, NOT PER ENTRY: N stack entries from ONE source mint N byte-identical
/// `DecisionSlot`s (real boards reach 35 entries on one source), and the sub-index
/// disambiguates choices WITHIN an ability instance, not instances of it
/// ([`crate::analysis::decision_template::DecisionSlot`]'s own doc). Publishing the same
/// slot N times would make the frontend render N identical pickers and
/// `predictability_gate` demand N pins for a choice [`inject_pinned_answer`] answers ONCE
/// per source (its `find_map` matches on the slot's SOURCE and is index-blind). So the
/// offer publishes the SET of open choices; one state-independent pin ("always target
/// P1") specifies every instance of it.
///
/// VISIBILITY: the `#[cfg(any(test, feature = "test-support"))]` gate this shipped behind
/// has LIFTED, exactly as its own note said it would — [`try_offer_bounded_cycle_shortcut`]
/// is the production caller the gate was waiting for. It stays `pub` because the
/// integration suite that pins its behaviour links the library.
pub fn bounded_cycle_pin_slots(
    state: &GameState,
    proposer: PlayerId,
) -> Vec<crate::analysis::decision_template::DecisionPoint> {
    // The DEGENERATE one-frame case of the window enumerator: with no window frame there is
    // no transition to observe, so `certified_period_touch`'s `window.is_empty()` branch seeds
    // `announced` from `state.stack` — same authority, same set, same order, same dedup, hence
    // byte-identical to the snapshot mint this used to be. The alias holds NO certificate, so
    // it passes `ResourceSignatureOnly`: the type is never allowed to claim one its caller
    // does not hold (and the empty-window branch yields `frozen_ids: ∅` on any value).
    bounded_cycle_pin_slots_for_window(
        &crate::analysis::resource::certified_period_touch(
            &[],
            state,
            crate::analysis::resource::PeriodCertification::ResourceSignatureOnly,
        ),
        proposer,
    )
}

/// CR 732.2a: the per-iteration choice slots ONE CERTIFIED PERIOD publishes for `proposer`.
///
/// Maps the single authority [`entry_publishes_pin_slots`] over `touch.announced`, with the
/// same per-slot dedup [`bounded_cycle_pin_slots`] applies — each pair evaluated against ITS
/// OWN carrying frame, which is the ring sample's LIVE half and never a `normalize_for_loop`
/// product: a normalized frame would key the stored-auto-choice refusal on `ObjectId(0)` and
/// would make `build_target_slots` take its `trigger_source: None` fall-through and publish a
/// WIDER legal set than the live board admits.
///
/// It calls the RAW mint rather than the verdict door on purpose: routing it through the door
/// would eagerly classify (and charge for) every announced pair at slot-enumeration time,
/// which is exactly the pre-population pass this design removes. The relief side reads the
/// CACHED mint through the door instead.
pub(crate) fn bounded_cycle_pin_slots_for_window(
    touch: &crate::analysis::resource::PeriodTouch<'_>,
    proposer: PlayerId,
) -> Vec<crate::analysis::decision_template::DecisionPoint> {
    use crate::analysis::decision_template::{DecisionPoint, DecisionPointKind};
    let mut points: Vec<DecisionPoint> = Vec::new();
    for (frame, entry) in &touch.announced {
        let Some(pins) = entry_publishes_pin_slots(frame, entry, proposer) else {
            continue;
        };
        // CR 601.2c: a `Targets` point only when the entry actually announces a choice.
        // Shape (B) publishes `target: None` — announcing it surfaces no target at all —
        // so a `min_targets: 1` point would over-state the sequence CR 732.2a describes.
        if let Some(target) = pins.target.filter(|t| !points.iter().any(|p| &p.slot == t)) {
            points.push(DecisionPoint {
                slot: target,
                kind: DecisionPointKind::Targets {
                    // VERBATIM the slot `ability_utils::build_target_slots` built for this
                    // announcement — not a second derivation. That is what makes WHICH
                    // choice is published and HOW MANY choices are published the same
                    // authority's answers, so they cannot disagree.
                    legal_targets: pins.legal_targets,
                    // Exactly one, and that cannot contradict the ANNOUNCEMENT: the
                    // acceptance test admits an entry only when
                    // `ability_utils::build_target_slots` — the authority that decides how
                    // many choices announcing it actually requires (CR 601.2c via
                    // CR 603.3d) — yields exactly one MANDATORY slot. An ability-fact
                    // check alone (`multi_target` / CR 601.2d division) does NOT bound
                    // the slot count: a chained sub-ability's own "target" is a second
                    // instance of the word and a second slot.
                    min_targets: 1,
                    max_targets: 1,
                    ordered: false,
                },
            });
        }
        // CR 603.5: "the choice is made when the ability resolves" — a "may" gate on the
        // same source is a SECOND per-iteration choice, published so the declaration must
        // pin it too.
        if let Some(may) = pins.may {
            if !points.iter().any(|p| p.slot == may) {
                points.push(DecisionPoint {
                    slot: may,
                    kind: DecisionPointKind::MayChoice,
                });
            }
        }
    }
    points
}

/// CR 732.2a: assemble a loop-shortcut offer's READ-side schema from its already-reified
/// decision `points`, its proposed repeat mode, and its CR 704 count bound.
///
/// `iteration_count` and `max_iterations` are separate inputs on purpose: the first is the
/// SUGGESTION the frontend seeds its picker with, the second is the LEGAL CEILING the
/// declared-count check enforces. A producer that cannot compute a real bound passes
/// `MAX_SHORTCUT_CYCLES`. The bounded-cycle producer narrows it; the drain and object-growth
/// producers do not — so the ceiling is live for bounded offers only.
fn build_shortcut_schema(
    points: Vec<crate::analysis::decision_template::DecisionPoint>,
    iteration_count: crate::analysis::decision_template::IterationCount,
    max_iterations: u32,
) -> crate::analysis::decision_template::ShortcutDecisionSchema {
    use crate::analysis::decision_template::{DecisionPointKind, ShortcutDecisionSchema};
    // CR 702.51a: engine-owned total of untapped convoke-eligible creatures across every
    // ConvokeTaps point — the frontend renders this directly instead of re-deriving it from
    // `points` (display-layer purity). Identical predicate/sum to the deleted React reduce.
    let convoke_tappable_count = points
        .iter()
        .filter_map(|p| match &p.kind {
            DecisionPointKind::ConvokeTaps { tappable } => Some(tappable.len()),
            _ => None,
        })
        .sum();
    ShortcutDecisionSchema {
        iteration_count,
        max_iterations,
        points,
        convoke_tappable_count,
    }
}

/// CR 732.4 + CR 104.4b: a net-progress mandatory loop draws ONLY if it drives NO player
/// toward a loss — no life drain, no poison, no decking. Any loss axis means a determinate
/// loser (Path A) or a staggered pod (fall through), never a draw. The live delta comes
/// from two `snapshot`s, so `damage_dealt` is empty (state-fed) and life loss surfaces as
/// a negative `life` delta.
fn has_no_loss_axis(delta: &crate::analysis::resource::ResourceVector) -> bool {
    // CR 704.5c: poison is now per-victim (`delta.poison`); a rising poison on ANY
    // player is a loss axis that vetoes the CR 732.4 draw.
    delta.life.values().all(|&n| n >= 0)
        && delta.library_delta.values().all(|&n| n >= 0)
        && delta.poison.values().all(|&n| n <= 0)
}

/// CR 800.4a: the seat that should receive priority when a loop-shortcut resolution hands
/// priority back. Priority passes to the next player in turn order still in the game — the
/// active player if it is still in the game, otherwise the next living seat in turn order
/// (elimination does not advance `active_player` when a non-acting seat concedes during the
/// APNAP window, so `active_player` may be a departed player).
fn living_priority_seat(state: &GameState) -> PlayerId {
    if crate::game::players::is_alive(state, state.active_player) {
        state.active_player
    } else {
        crate::game::players::next_player_in_turn_order(state, state.active_player)
    }
}

/// CR 732.2c + CR 704.5a: apply a confirmed loop shortcut. Reached ONLY on the Accept path
/// (every living opponent accepted). CR 608.2b re-validation is satisfied BY CONSTRUCTION:
/// the offer confirmed `proposal.predicted_winner` as the determinate winner over public board
/// state, and between the offer and the final Accept the dispatch admits ONLY the protocol
/// actions (`DeclareShortcut`/`RespondToShortcut`), none of which touch the board — so the
/// loop is provably still intact and the predicted winner remains valid. (A live ring re-scan
/// here is unsound: intervening finalize/SBA/layer steps drift the paused state away from the
/// sampled ring frames. The Shorten path — where a real board action CAN break the loop —
/// deliberately hands priority instead of reaching here, and re-detection re-fires the bridge
/// LIVE on a later beat.) `UntilLethal` ⇒ mark the unbounded axes + declare the terminal win;
/// `Fixed(N)` ⇒ Phase-4b finite materialization (`materialize_fixed_shortcut`), which drives
/// N whole cycles atomically, commits + stops early on a cross-lethal `GameOver` mid-drive, and
/// falls back to manual play (priority to `living_priority_seat`) on any abort.
///
/// The consumption-time proposer/winner-liveness guard below catches a `Concede` (CR 104.3a)
/// or a `Debug` that ELIMINATES either authority inside the still-open APNAP window. A `Debug` action
/// that drifts the board WITHOUT killing the proposer (e.g. debug-removing a loop permanent)
/// is deliberately out of scope: `debug_mode` is sandbox god-mode that can already produce
/// arbitrarily inconsistent states, so loop-shortcut soundness under arbitrary debug mutation
/// is not a competitive-correctness obligation.
fn apply_confirmed_shortcut(
    state: &mut GameState,
    result: &mut ActionResult,
    proposal: &crate::analysis::loop_check::ShortcutProposal,
) {
    // CR 104.3a / CR 104.2a / CR 800.4a: re-validate the proposer and any latched winner at
    // consumption. `GameAction::Concede` (and a board-mutating `Debug`) bypass the WaitingFor
    // dispatch, so either authority can leave during the APNAP window. A departed proposer
    // invalidates the sequence they suggested; a departed predicted winner cannot be crowned.
    if !crate::game::players::is_alive(state, proposal.proposer)
        || proposal
            .predicted_winner
            .is_some_and(|winner| !crate::game::players::is_alive(state, winner))
        // CR 732.2a + CR 603.5: `template.owner` decides WHOSE CR 603.5 choice a pin may
        // answer (the drive's seat guard in `inject_pinned_answer`). The two live ingresses
        // bind it at declare; a RESTORED `WaitingFor::RespondToShortcut` is plain serde and
        // the untrusted-restore scrubber rewrites only the two PRE-CAST waits, so it never
        // ran that firewall. Re-validate the SAME invariant at the one point every drive
        // passes through, so the seat guard is meaningful on every ingress and not only the
        // two live ones. Refusing (rather than forcing `owner = proposer`) keeps the
        // fail-closed direction every other conjunct at this seam uses: forcing would make a
        // tampered proposal runnable under a rewritten owner.
        || proposal
            .template
            .as_ref()
            .is_some_and(|t| t.owner != proposal.proposer)
    {
        priority::reset_priority(state);
        // CR 800.4a: priority passes to the next player in turn order still in the game.
        // The departed proposer may have been the active player (elimination does not advance
        // `active_player` when a non-acting seat concedes during the APNAP window), so route to
        // a LIVING holder rather than a possibly-departed `active_player`.
        let holder = living_priority_seat(state);
        state.waiting_for = WaitingFor::Priority { player: holder };
        result.waiting_for = state.waiting_for.clone();
        return;
    }
    match proposal.count {
        crate::analysis::decision_template::IterationCount::UntilLethal => {
            apply_until_lethal_shortcut(state, result, proposal)
        }
        crate::analysis::decision_template::IterationCount::Fixed(n) => {
            materialize_fixed_shortcut(state, result, proposal, n)
        }
    }
}

/// PR-7 Combo-UI Stage 2 (SOUNDNESS #2 — the E1 crown): CR 732.2a / CR 704.5a / CR 104.2a
/// win-derivation for a confirmed `UntilLethal` loop shortcut. NEVER an unconditional crown.
/// DRIVES one pin-faithful cycle of the confirmed loop, MEASURES the per-cycle
/// `ResourceVector::delta`, and re-runs the SAME offer-time authority
/// (`live_mandatory_loop_winner`) on the driven states. Crowns ONLY when that authority
/// names the proposer as the sole determinate winner; every other outcome (a subset-lethal
/// loop with >1 non-faller, an Advantage token-growth loop with no faller, an aborted drive)
/// falls back to manual play (CR 800.4a) — no wrong crown.
///
/// F2 hardening (crown SELF-soundness — a GameOver path must not depend on a future
/// hard-gated PR): for the ≥2-faller case, RE-VERIFY the offer's own
/// `fallers_lives_pairwise_equal` (CR 704.3 simultaneity) on the boundary/pre-drive faller
/// lives — a staggered-death unequal-absolute drain does NOT crown even though
/// `live_mandatory_loop_winner`'s ≥2-faller floor checks only per-cycle DELTAS.
///
/// SOUNDNESS FLAG (#20, belt+suspenders): when the PR-8 targeted-offer trigger reifies >2p
/// targeted loops, it should ALSO carry `fallers_lives_pairwise_equal` at OFFER time.
fn apply_until_lethal_shortcut(
    state: &mut GameState,
    result: &mut ActionResult,
    proposal: &crate::analysis::loop_check::ShortcutProposal,
) {
    // The board is unchanged since the offer (apply_confirmed_shortcut doc): `committed` is
    // the fully-committed pre-drive state to roll back to on any non-crown.
    let committed = state.clone();
    // The recurrence boundary: the loop's canonical per-cycle SETTLE beat
    // (`Priority{active_player}`), normalized — the same construction `materialize_fixed_shortcut`
    // captures (the cover/equal-modulo checks normalize internally, so this is a
    // self-contained canonical frame). `snapshot`'s life/poison/library axes are unaffected by
    // `normalize_for_loop`, so `before` is the pre-drive resource baseline.
    let boundary = {
        let mut seed = committed.clone();
        priority::reset_priority(&mut seed);
        seed.waiting_for = WaitingFor::Priority {
            player: seed.active_player,
        };
        seed.normalize_for_loop()
    };
    let before = crate::analysis::resource::ResourceVector::snapshot(&boundary);
    let period = shortcut_drive_period(proposal.template.as_ref());

    // DRIVE one representative cycle to produce the measured post-drive `work` state.
    let work: GameState = if !committed.last_loop_action_sequence.is_empty() {
        // Object-growth loop period (recast buyback+convoke, or a multi-activation mana engine)
        // declared `UntilLethal` by the AI (which hardcodes it for every optional offer). Drive
        // one real period on a clone under the re-entrancy guard; an inert Advantage token/mana
        // loop has NO life/poison faller ⇒ `live_mandatory_loop_winner` returns None below ⇒
        // manual fallback (this is the latent AI-mis-crown fix, first-class).
        let seq = committed.last_loop_action_sequence.clone();
        let controller = seq[0].controller;
        let expected_defs: Vec<Option<crate::types::ability::AbilityDefinition>> = seq
            .iter()
            .map(|c| loop_action_expected_def(&committed, c))
            .collect();
        let _probe = SimulationProbeGuard::enter();
        let mut w = committed.clone();
        priority::reset_priority(&mut w);
        w.waiting_for = WaitingFor::Priority { player: controller };
        match drive_loop_sequence_iteration(&mut w, &seq, 0, &expected_defs) {
            Ok(()) => w,
            Err(RecastAbort) => {
                return until_lethal_fallback(state, result, committed);
            }
        }
    } else {
        // Drain loop (targeted Vito class, non-targeted Cleric class, ω-covering cascade).
        // Drive `period` whole cycles, injecting the pinned answers (CR 603.3b ordering / CR
        // 608.2b targets) at each mid-cycle prompt. A cross-lethal mid-drive already applies
        // the win to `work` (CR 704.5a SBA).
        let cap = auto_pass_loop_max_iterations(&committed);
        let mut running = committed.clone();
        for i in 0..period {
            // The SAME single authority the `Fixed(N)` drive reads. Unreachably `Some` today —
            // `handle_declare_shortcut` rejects `UntilLethal` against a bounded offer, and the
            // bounded producer is the only one that publishes a signature — so this is
            // behaviour-identical to the former no-delimiter call; it is threaded so the two
            // drives cannot drift apart on what delimits a cycle.
            match drive_one_shortcut_cycle(
                &running,
                &boundary,
                proposal.template.as_ref(),
                i,
                cap,
                proposal.per_cycle.as_ref().map(|pd| pd.frames_per_period),
            ) {
                CycleOutcome::Recurred { state: s, .. } => running = *s,
                CycleOutcome::CrossLethal {
                    state: s,
                    winner,
                    mut events,
                } => {
                    // Commit + stop ONLY when the mid-drive lethal matches the winner measured
                    // at offer time; any other winner (or a draw) rolls back to manual play. `UntilLethal`
                    // IS unbounded ⇒ mark the axes on the committed state (contrast the
                    // finite `Fixed(N)` cross-lethal, which does not).
                    if let Some(winner) =
                        winner.filter(|winner| Some(*winner) == proposal.predicted_winner)
                    {
                        let mut w = *s;
                        w.mark_unbounded_loop(winner, &proposal.unbounded);
                        *state = w;
                        result.events.append(&mut events);
                        state.waiting_for = WaitingFor::GameOver {
                            winner: Some(winner),
                        };
                        result.waiting_for = state.waiting_for.clone();
                    } else {
                        until_lethal_fallback(state, result, committed);
                    }
                    return;
                }
                CycleOutcome::Abort => {
                    return until_lethal_fallback(state, result, committed);
                }
            }
        }
        running
    };

    // MEASURE + derive the winner via the offer-time authority, VERBATIM.
    let delta = crate::analysis::resource::ResourceVector::delta(
        &before,
        &crate::analysis::resource::ResourceVector::snapshot(&work),
    );
    match crate::analysis::loop_check::live_mandatory_loop_winner(&boundary, &work, &delta) {
        Some(winner) if Some(winner) == proposal.predicted_winner => {
            // F2 (CR 704.3 simultaneity): for ≥2 fallers, re-verify the offer's own pairwise
            // life-equality on the pre-drive faller lives. `live_mandatory_loop_winner`'s
            // ≥2-faller floor checks only per-cycle DELTAS, so a staggered-death unequal
            // ABSOLUTE-life drain would pass it — the offer's `fallers_lives_pairwise_equal`
            // is the missing absolute-life gate. Single-faller (2p) skips it (no simultaneity
            // to enforce); a non-targeted symmetric drain was certified pairwise-equal on the
            // frozen board, so it still passes.
            let fallers = fallers_of(&work, &delta);
            if fallers.len() >= 2
                && !crate::analysis::loop_check::fallers_lives_pairwise_equal(
                    &[&boundary],
                    &fallers,
                )
            {
                until_lethal_fallback(state, result, committed);
            } else {
                crown_until_lethal(state, result, proposal, winner);
            }
        }
        _ => until_lethal_fallback(state, result, committed),
    }
}

/// The faller partition of a measured per-cycle `delta`, over the living players of
/// `cycle_end` — EXACTLY the partition `live_mandatory_loop_winner` computes internally
/// (`delta.life<0 || delta.poison>0`). Exposed for the F2 ≥2-faller re-verification; NOT a
/// re-architecting of the win authority.
fn fallers_of(
    cycle_end: &GameState,
    delta: &crate::analysis::resource::ResourceVector,
) -> Vec<PlayerId> {
    cycle_end
        .players
        .iter()
        .filter(|p| !p.is_eliminated)
        .map(|p| p.id)
        .filter(|p| {
            delta.life.get(p).copied().unwrap_or(0) < 0
                || delta.poison.get(p).copied().unwrap_or(0) > 0
        })
        .collect()
}

/// CR 732.2a + CR 704.5a: crown the measured winner of the confirmed
/// unbounded drain (the former UntilLethal-arm body). Persists the unbounded axes (the ∞ HUD
/// producer) and declares the CR 704.5a win.
fn crown_until_lethal(
    state: &mut GameState,
    result: &mut ActionResult,
    proposal: &crate::analysis::loop_check::ShortcutProposal,
    winner: PlayerId,
) {
    state.mark_unbounded_loop(winner, &proposal.unbounded);
    result.events.push(GameEvent::GameOver {
        winner: Some(winner),
    });
    state.waiting_for = WaitingFor::GameOver {
        winner: Some(winner),
    };
    result.waiting_for = state.waiting_for.clone();
    match_flow::handle_game_over_transition(state);
}

/// CR 800.4a: the E1 crown refused (no determinate winner / aborted drive) ⇒ roll back to the
/// pre-drive committed board and hand priority to the living seat for manual play. Clears the
/// loop-detect ring so this same `apply()` does not instantly re-offer the (now-declined)
/// loop; a later beat re-detects genuinely. Mirrors the `materialize_fixed_shortcut` abort
/// tail.
fn until_lethal_fallback(state: &mut GameState, result: &mut ActionResult, committed: GameState) {
    *state = committed;
    // CR 732.2c: a declined shortcut must not instantly re-offer the SAME loop in this same
    // `apply()`. Clear both re-offer signals: the drain offer's `loop_detect_ring` AND the
    // object-growth offer's `last_loop_action_sequence` routing signal (a non-drain object-growth
    // loop, e.g. an AI-declared UntilLethal on an inert Advantage recast, would otherwise
    // re-fire `try_offer_object_growth_shortcut` on the next reconcile and livelock). A later
    // real re-cast re-captures the sequence and re-detects genuinely.
    state.loop_detect_ring.clear();
    state.last_loop_action_sequence.clear();
    priority::reset_priority(state);
    state.waiting_for = WaitingFor::Priority {
        player: living_priority_seat(state),
    };
    result.waiting_for = state.waiting_for.clone();
}

/// CR 732.2a: how many whole cycles one shortcut drive must aggregate before the measured
/// delta is complete. A `RoundRobin`/`Piecewise` target schedule rotates its OBJECT sources
/// over its length, so a full period is that length; every other pin (a `Constant` target, a
/// `Player` pin, a non-target pin, or no template at all) settles in ONE cycle. Returns the
/// max schedule length over the template's `Targets` pins, defaulting to 1.
///
/// DORMANT for every Stage-2 crownable loop (Ruling B): `TargetSchedule` rotates DecisionSource
/// objects, not players, and `live_mandatory_loop_winner` crowns on PLAYER fallers — an
/// object-rotating loop produces no player faller, so it never crowns; the only crownable >2p
/// player drain pins ALL opponents every cycle (`TargetPin::Player` is constant, period 1). The
/// seam is built for generality; a multi-cycle aggregation is fail-safe (an object loop reaching
/// the arm measures 1 cycle, finds no faller, does not crown).
///
/// CR 732.2a SAFETY LIMIT: the returned period is clamped to `MAX_SHORTCUT_CYCLES`. Both
/// consumers derive their `0..period` range from this one helper (`validate_pins` and
/// `apply_until_lethal_shortcut`), so the clamp bounds validate + drive coherently;
/// crown-soundness holds — every crownable loop has period 1, so the clamp only truncates a
/// hostile over-cap schedule into the conservative manual-fallback arm, never a mis-crown.
fn shortcut_drive_period(
    template: Option<&crate::analysis::decision_template::DecisionTemplate>,
) -> crate::analysis::decision_template::IterationIndex {
    use crate::analysis::decision_template::{PinnedDecision, TargetPin, TargetSchedule};
    let Some(t) = template else { return 1 };
    t.decisions
        .iter()
        .filter_map(|pin| match pin {
            PinnedDecision::Targets { targets, .. } => targets
                .iter()
                .map(|tp| match tp {
                    TargetPin::Scheduled(TargetSchedule::RoundRobin(v)) => v.len() as u32,
                    TargetPin::Scheduled(TargetSchedule::Piecewise(v)) => v.len() as u32,
                    TargetPin::Scheduled(TargetSchedule::Constant(_))
                    | TargetPin::ByIdentity(_)
                    | TargetPin::Player(_) => 1,
                })
                .max(),
            _ => None,
        })
        .max()
        .unwrap_or(1)
        // CR 732.2a SAFETY LIMIT: the drive period is STRUCTURALLY unbounded in the engine —
        // its length is the client template schedule's own length. On the WS transport the
        // 8 KB inbound-frame cap (phase-server/src/main.rs:409/1420) already bounds a hostile
        // schedule to a few hundred entries (~1-2 s stall, not a million-cycle remote DoS),
        // but in-process callers (WASM/Tauri/local) bypass that cap, so clamp here AT THE
        // SOURCE for every caller. Real schedules rotate over a handful of object sources
        // (period ≪ cap), so this is invisible to every legitimate loop; a clamped-shorter
        // drive measures a smaller (more conservative) delta ⇒ FEWER crowns / more manual
        // fallbacks, never a wrong crown.
        .clamp(1, MAX_SHORTCUT_CYCLES)
}

/// CR 732.2a: the index range the declare-time firewall must validate a pin over — the range
/// the ACCEPTED COUNT will actually drive, read off the two drive loops themselves:
/// `materialize_fixed_shortcut` drives `for i in 0..n` for a `Fixed(n)`, and
/// `apply_until_lethal_shortcut` drives whole periods for `UntilLethal`, whose length is
/// [`shortcut_drive_period`].
///
/// THIS IS NOT `shortcut_drive_period`, and the difference is the whole point of the fix.
/// That helper answers "how many cycles must one measurement aggregate" — a schedule property
/// with nothing to do with the declared count. Validating over it both ACCEPTED a pin whose
/// driven image leaves the offer's PUBLISHED legal set at an index the count reaches, and
/// REFUSED conforming declarations whose count is shorter than the schedule.
///
/// NO CONSERVATIVE PADDING. Widening the range with `.max(shortcut_drive_period(..))` is
/// bug-preserving — it re-imports the schedule-derived period the invariant exists to remove,
/// and at a count of 1 over a length-2 rotation `Ok` is the CORRECT answer. Do not re-derive
/// and re-add that term.
///
/// PRECONDITION, discharged at its call site: the count is already cap-checked, because
/// `handle_declare_shortcut` runs the `MAX_SHORTCUT_CYCLES` / `max_iterations` match ABOVE
/// the pin-validation block. Without that ordering a hostile `Fixed(4e9)` would become a
/// four-billion-iteration validation loop. Exactly ONE call site consumes this helper.
///
/// Exhaustive over `IterationCount` with no wildcard, so a future variant build-breaks here
/// and forces a range decision instead of silently inheriting one.
fn shortcut_validated_range(
    count: &crate::analysis::decision_template::IterationCount,
    template: Option<&crate::analysis::decision_template::DecisionTemplate>,
) -> crate::analysis::decision_template::IterationIndex {
    match count {
        crate::analysis::decision_template::IterationCount::Fixed(n) => *n,
        crate::analysis::decision_template::IterationCount::UntilLethal => {
            shortcut_drive_period(template)
        }
    }
}

/// PR-7 Combo-UI Stage 2: the typed result of driving ONE whole loop-shortcut cycle on a
/// clone. Exhaustive at both call sites (`materialize_fixed_shortcut`, `apply_until_lethal_
/// shortcut`) — no silent `_` that could crown or roll back on an unhandled outcome.
enum CycleOutcome {
    /// The cycle recurred (constant-depth equal-modulo-resources or ω-covering) ⇒ `state` is
    /// the committed post-cycle board; `events` are its accumulated events.
    Recurred {
        state: Box<GameState>,
        events: Vec<GameEvent>,
    },
    /// CR 704.5a: the cycle crossed lethal mid-drive ⇒ the win is already applied to `state`
    /// (`waiting_for = GameOver{winner}`); `events` include the terminal `GameOver`.
    CrossLethal {
        state: Box<GameState>,
        winner: Option<PlayerId>,
        events: Vec<GameEvent>,
    },
    /// Runaway beat cap, an unpinned prompt, or an engine error ⇒ abort to manual play.
    Abort,
}

/// PR-7 Combo-UI Stage 2: drive ONE whole cycle of a confirmed loop shortcut on a fresh clone
/// of `committed`, seeded to the canonical settle beat (`Priority{active_player}`, the same
/// beat the detector ring samples). Recurrence is detected against `boundary` (normalized).
/// Behavior-identical to the former inline `materialize_fixed_shortcut` beat loop EXCEPT the
/// old `Ok(_) => break 'cycles` abort on a mid-cycle prompt now delegates to
/// [`inject_pinned_answer`] (CR 603.3b ordering / CR 608.2b pinned targets) and continues.
/// Uses the INTERNAL `apply_action` path throughout (via `pass_priority_once_with_pipeline`
/// and the injector), never the top-level reconcile boundary, so the detection hook cannot
/// recurse mid-drive.
///
/// # Two cycle delimiters, and why the second one exists
///
/// `frames_per_period` is the published [`crate::analysis::resource::PeriodicDelta`] span, or
/// `None` for every offer whose producer states no per-period signature. When it is `Some(k)`,
/// a cycle ALSO completes once `k` retained ring frames have been recorded since the cycle
/// began.
///
/// Board recurrence alone is not a delimiter for the class
/// [`try_offer_bounded_cycle_shortcut`] mints on certification basis **B**: that basis consults
/// no board predicate at all — it certifies a periodic *delta* over a ring window — so
/// `loop_states_equal_modulo_resources` and `loop_states_cover_modulo_growth` are both FALSE at
/// every settle beat by construction. Without the frame delimiter such a drive can only end at
/// the beat cap (`Abort`, committing zero cycles) or by crossing lethal, and the declared `n`
/// is inert: `Fixed(1)` and `Fixed(3)` produce byte-identical boards.
///
/// The frame count is the same quantity `frames_per_period` names, measured the same way: the
/// single `record_loop_detect_sample` call site lives in `pass_priority_once_with_pipeline`,
/// which is the very function this loop steps, so a driven beat samples the ring under exactly
/// the gates an observed beat does. A new frame is detected by `Arc` identity of the ring's
/// back rather than by length, because the ring evicts at `LOOP_DETECT_RING_CAP` and a length
/// delta reads 0 once it is full.
fn drive_one_shortcut_cycle(
    committed: &GameState,
    boundary: &GameState,
    template: Option<&crate::analysis::decision_template::DecisionTemplate>,
    iteration: crate::analysis::decision_template::IterationIndex,
    cycle_beat_cap: usize,
    frames_per_period: Option<u32>,
) -> CycleOutcome {
    let mut work = committed.clone();
    priority::reset_priority(&mut work);
    work.waiting_for = WaitingFor::Priority {
        player: work.active_player,
    };
    let mut ev: Vec<GameEvent> = Vec::new();
    let mut beat = 0usize;
    let mut frames_this_cycle = 0u32;

    loop {
        beat += 1;
        if beat > cycle_beat_cap {
            return CycleOutcome::Abort; // runaway backstop
        }
        let ring_back_before = work.loop_detect_ring.back().map(std::sync::Arc::as_ptr);
        // A FRESH per-beat buffer (see the former inline note): reusing one growing buffer
        // would make `run_post_action_pipeline` re-scan prior beats' events and re-fire
        // already-consumed triggers.
        let mut beat_events: Vec<GameEvent> = Vec::new();
        match pass_priority_once_with_pipeline(&mut work, &mut beat_events, None) {
            // Cross-lethal: COMMIT + STOP. The GameOver event + transition are already in
            // `work`/`beat_events`.
            Ok(WaitingFor::GameOver { winner }) => {
                ev.append(&mut beat_events);
                return CycleOutcome::CrossLethal {
                    state: Box::new(work),
                    winner,
                    events: ev,
                };
            }
            // Active-player settle beat: cycle complete iff the board recurred (constant-depth
            // equal-modulo-resources OR ω-covering growth) or the published period's worth of
            // ring frames has elapsed. This is the ONLY beat kind the ring samples at (the
            // sampler's own gate is `Priority{player == active_player}`), so the frame counter
            // is advanced here and nowhere else.
            Ok(WaitingFor::Priority { player }) if player == work.active_player => {
                ev.append(&mut beat_events);
                let ring_back_after = work.loop_detect_ring.back().map(std::sync::Arc::as_ptr);
                if ring_back_after.is_some() && ring_back_after != ring_back_before {
                    frames_this_cycle += 1;
                }
                let norm = work.normalize_for_loop();
                if crate::analysis::resource::loop_states_equal_modulo_resources(boundary, &norm)
                    || crate::analysis::resource::loop_states_cover_modulo_growth(boundary, &norm)
                    || frames_per_period.is_some_and(|k| frames_this_cycle >= k)
                {
                    return CycleOutcome::Recurred {
                        state: Box::new(work),
                        events: ev,
                    };
                }
                continue; // active beat, not yet recurred ⇒ keep driving within the cap
            }
            // Opponent's mid-cycle priority window ⇒ keep driving.
            Ok(WaitingFor::Priority { .. }) => {
                ev.append(&mut beat_events);
                continue;
            }
            // Any OTHER prompt (OrderTriggers / TriggerTargetSelection / …): answer it from the
            // pins and continue. An unpinned prompt fails closed ⇒ abort to manual.
            Ok(other) => {
                ev.append(&mut beat_events);
                match inject_pinned_answer(&mut work, template, iteration, &other) {
                    Ok(()) => continue,
                    Err(RecastAbort) => return CycleOutcome::Abort,
                }
            }
            Err(_) => return CycleOutcome::Abort, // engine error ⇒ abort to manual
        }
    }
}

/// PR-7 Combo-UI Stage 2: answer ONE mid-drive prompt during a loop-shortcut cycle, using the
/// INTERNAL reconcile-free `apply_action` path (mirrors `drive_loop_action_iteration`, so the
/// detection hook cannot recurse mid-drive). Fail-closed: any prompt kind with no Stage-2
/// producer ⇒ `Err(RecastAbort)`.
///
/// There is deliberately NO top-level `template.ok_or(...)` guard: the `OrderTriggers` arm is
/// TEMPLATE-INDEPENDENT (the real 2p Vito drive raises OrderTriggers with a `template = None`
/// declaration, and the forced-unique target auto-selects at dispatch), so a top guard would
/// wrongly abort it. Each pin-consuming arm therefore carries its own guard: the
/// `TriggerTargetSelection` arm is the only arm that consumes a CR 608.2b `Targets` pin, and the
/// `OptionalEffectChoice` arm consumes the CR 603.5 `MayChoice` pin and carries its own seat +
/// beat guards on top of the same `template.ok_or(..)`.
fn inject_pinned_answer(
    work: &mut GameState,
    template: Option<&crate::analysis::decision_template::DecisionTemplate>,
    iteration: crate::analysis::decision_template::IterationIndex,
    prompt: &WaitingFor,
) -> Result<(), RecastAbort> {
    use crate::analysis::decision_template::{ConcreteDecision, ConcreteTarget, MayChoiceOption};
    match prompt {
        // CR 603.3b / CR 732.2a: auto-order the confirmed shortcut's simultaneous
        // same-controller triggers by identity order (0..len). Template-INDEPENDENT and
        // delta-safe: the per-cycle net drain is order-invariant (both opponents drain
        // regardless of order; pins fix WHICH opponent, not the ordering). Answered via the
        // INTERNAL `handle_order_triggers` (`apply_action`), NOT `drain_order_triggers_with_
        // identity` — the latter routes through `reconcile_terminal_result`, which would
        // re-enter the loop-detection/offer hook mid-drive and could crown via a different
        // authority, bypassing E1's own measure.
        WaitingFor::OrderTriggers { player, triggers } => {
            let order: Vec<usize> = (0..triggers.len()).collect();
            apply_action(work, *player, GameAction::OrderTriggers { order }, None)
                .map_err(|_| RecastAbort)?;
            Ok(())
        }
        // CR 608.2b: choose this trigger's targets from the pin whose source matches the
        // prompt's `source_id` (per-source, so two distinct drainers pinned to distinct
        // opponents each receive the correct target). The template guard lives HERE.
        WaitingFor::TriggerTargetSelection {
            player, source_id, ..
        } => {
            let template = template.ok_or(RecastAbort)?;
            let source_id = source_id.ok_or(RecastAbort)?;
            let decisions = crate::analysis::decision_template::resolve(template, iteration, work)
                .map_err(|_| RecastAbort)?;
            let targets = decisions
                .into_iter()
                .find_map(|d| match d {
                    ConcreteDecision::Targets { slot, targets }
                        if slot_source_prompted(work, &slot.source, source_id) =>
                    {
                        Some(targets)
                    }
                    _ => None,
                })
                .ok_or(RecastAbort)?;
            let refs: Vec<TargetRef> = targets
                .into_iter()
                .map(|t| match t {
                    ConcreteTarget::Object(id) => TargetRef::Object(id),
                    ConcreteTarget::Player(p) => TargetRef::Player(p),
                })
                .collect();
            apply_action(
                work,
                *player,
                GameAction::SelectTargets { targets: refs },
                None,
            )
            .map_err(|_| RecastAbort)?;
            Ok(())
        }
        // CR 603.5 + CR 732.2a: answer an in-cycle "may" from the pin its owner declared.
        //
        // The recipient is read OFF THE PROMPT, which is the only TOTAL instrument:
        // `WaitingFor::OptionalEffectChoice` has five production producers and exactly one
        // consults `effects::optional_prompt_player`, so the mint's recipient conjunct is a
        // PREDICTION over one of five producers while this comparison is an OBSERVATION of
        // the prompt in hand. Precondition (c) of the pin extension point — "only the acting
        // player's own choices are pinnable" (`analysis::resource`) — is what it enforces.
        //
        // `template.owner` is only a legitimate comparand because `handle_declare_shortcut`
        // firewalls it to the engine-issued `LoopShortcutOffer.proposer` at declare (and
        // `apply_confirmed_shortcut` re-validates the same invariant for the restore ingress,
        // which never runs the declare handler). Without those two, this test compares an
        // attacker-chosen value against itself.
        WaitingFor::OptionalEffectChoice {
            player, source_id, ..
        } => {
            let template = template.ok_or(RecastAbort)?;
            if *player != template.owner {
                return Err(RecastAbort);
            }
            // CR 603.5 vs CR 603.3c + CR 700.2b: pin the BEAT as well as the seat. A
            // `MayChoice` pin binds the RESOLUTION-time question (CR 603.5). While a trigger
            // is still mid-construction the engine asks a same-`source_id`
            // ANNOUNCEMENT-time one instead — the optional-modal gate raised out of
            // `begin_pending_trigger_target_selection`, which runs with the construction
            // cursor (`pending_trigger`) still live. `slot_source_prompted` cannot separate
            // them: it matches the SOURCE OBJECT and both prompts carry it. So a live cursor
            // means the prompt in hand may be the announcement-time question the pin does not
            // answer ⇒ fail-closed.
            if work.pending_trigger.is_some() {
                return Err(RecastAbort);
            }
            let decisions = crate::analysis::decision_template::resolve(template, iteration, work)
                .map_err(|_| RecastAbort)?;
            let take = decisions
                .into_iter()
                .find_map(|d| match d {
                    ConcreteDecision::MayChoice { slot, take }
                        if slot_source_prompted(work, &slot.source, *source_id) =>
                    {
                        Some(take)
                    }
                    _ => None,
                })
                .ok_or(RecastAbort)?;
            apply_action(
                work,
                *player,
                GameAction::DecideOptionalEffect {
                    accept: take == MayChoiceOption::Take,
                },
                None,
            )
            .map_err(|_| RecastAbort)?;
            Ok(())
        }
        // CR 732.2a "no conditional actions": any other prompt (mode / unless / X) has no
        // Stage-2 pin producer ⇒ fail-closed. `may` left this list when the mint gained its
        // `EntryPinSlots.may` producer and the arm above; the remainder is still unpinnable.
        _ => Err(RecastAbort),
    }
}

/// CR 608.2b + CR 114.2: does this SLOT's source identify the ability instance that raised
/// the prompt carrying `source_id`?
///
/// [`crate::analysis::decision_template::resolve_source`] is deliberately BATTLEFIELD-ONLY,
/// and that filter IS the CR 608.2b (`docs/MagicCompRules.txt:2789`) legality re-check for
/// `ByIdentity` **target** pins — a pinned target that left the battlefield must stop
/// matching. It must not be widened. But a SLOT's source only identifies WHICH ability
/// instance prompts, and CR 114.2 (`:828`) puts a planeswalker EMBLEM — "both owned and
/// controlled by that player" — in the **command zone**, where it stays for the whole game
/// and raises its triggers from. So the command-zone disjunct lives HERE, at the caller,
/// scoped to object identity + the pinned CR 400.7 incarnation.
///
/// Graveyard / exile / hand sources still fail ⇒ the caller aborts to manual play.
fn slot_source_prompted(
    state: &GameState,
    src: &crate::analysis::decision_template::DecisionSource,
    source_id: ObjectId,
) -> bool {
    if crate::analysis::decision_template::resolve_source(src, state) == Some(source_id) {
        return true;
    }
    // CR 114.2: the command-zone arm. `AllCopies` is card-identity matching and an emblem
    // has no card, so only `ThisObject` participates.
    let crate::types::game_state::YieldTarget::ThisObject {
        source_id: pinned_id,
        incarnation,
        ..
    } = src
    else {
        return false;
    };
    *pinned_id == source_id
        && state.objects.get(pinned_id).is_some_and(|o| {
            o.zone == crate::types::zones::Zone::Command
                && (incarnation.is_none() || *incarnation == Some(o.incarnation))
        })
}

/// PR-7 Phase 4b: CR 732.2a finite materialization of a confirmed `Fixed(N)` loop
/// shortcut. Drives `n` whole cycles of the constant-depth (or ω-covering) loop,
/// committing atomically per cycle. If a cycle crosses lethal, the win arrives
/// mid-drive already applied to `work` (CR 704.5a via `run_post_action_pipeline`'s
/// SBA pass) ⇒ COMMIT + STOP, un-clamped — `n` may be ≥ the true cycles-to-lethal
/// (CR 732.2a "a specified number of times" places no upper bound relative to the
/// board). Any unexpected prompt / stale-incarnation replay failure (CR 400.7) /
/// runaway beat count ⇒ abort to manual play: roll back to the last fully-committed
/// cycle and hand priority to the living seat (CR 800.4a) — exactly the pre-4b
/// decline-stub behavior, never a wrong crown.
fn materialize_fixed_shortcut(
    state: &mut GameState,
    result: &mut ActionResult,
    proposal: &crate::analysis::loop_check::ShortcutProposal,
    n: u32,
) {
    // PR-7 Phase 4d-ii / P7 v3 (CR 732.2a): an object-growth loop (buyback recast, or a
    // multi-activation mana engine) settles with an EMPTY stack and grows a projected resource,
    // so the per-beat auto-pass drive below never recognizes its recurrence. Route it to the
    // INJECTOR instead, which drives one real period per cycle on a clone. A non-empty
    // `last_loop_action_sequence` (armed only on a buyback token cast or an accumulated
    // activation period) is the routing signal; the `seq` rides `state.last_loop_action_sequence`
    // (carried on the clone since the offer). The drain path below is byte-identical for every
    // other loop.
    //
    // CR 732.2c: record the count the shortcut was ACCEPTED at. "Once the last player has
    // either accepted or shortened the shortcut proposal, the shortcut is taken" — its ending
    // point is fixed at N, so the CR 500.5 boundary collapse prompt may only offer `0..=N`.
    // Re-asking with a wider range would let the controller take a longer sequence than the
    // one the table agreed to.
    //
    // STASH-GATED, and it must stay that way. A bound with no deferred materialization to
    // bound is unclearable — all three clears (`take_pending_materialization`,
    // `clear_collapsed_materializations`, `clear_unbounded_loop`) are keyed on the stash, and
    // the field is `#[serde(default)]`-persistent — so it would outlive its accept and
    // silently cap the NEXT accept's agreed count forever (a mana accept at `Fixed(1)`
    // capping a later, unanimously agreed `Fixed(500)` object-growth collapse at 1). Only the
    // object-growth route below registers anything, and even it registers CONDITIONALLY: a
    // mana engine grows no token/counter/life axis and registers nothing at all. So the gate
    // is a measured STASH-GREW check taken ACROSS the call — testing before it would be
    // unconditionally false, since that call is what registers. Length-delta rather than
    // `contains_key`, so a non-registering accept cannot `min`-shrink a bound that an
    // earlier, larger, genuinely-registering accept owns.
    //
    // MINIMUM, not overwrite: `register_pending_materialization` APPENDS, so a controller who
    // accepts twice before the CR 500.5 boundary owns ONE stash holding both accepts' items,
    // and the boundary applies ONE submitted amount to every item in it. Overwriting the bound
    // would let a later `Fixed(1000)` accept re-scale an earlier `Fixed(1)` accept's items
    // 1000×, materializing growth the table never agreed to. The minimum is the only bound
    // that no accept in the stash can exceed. Conservative on purpose: the later accept is
    // UNDER-delivered (its agreed 1000 caps at the earlier 1) rather than the earlier one
    // being over-delivered — divergence from the table's agreement in the safe direction.
    //
    // The exact fix is a per-accept bound, deferred for its WIRE-COMPATIBILITY COST — not
    // because it is unrepresentable. A bound carried ON each item, or the accept-grouped
    // `Vec<MaterializationBatch { n, items }>` this is tracked as, survives the boundary's
    // pause-safety `sort_by_key` fine: the sort moves each payload along with its key. What
    // it costs is a shape change to `pending_unbounded_materialization`, a SAVED-GAME field,
    // plus the `cr733/authority_matrix` census fixture that pins its composition. (Only a
    // PARALLEL per-item bound VECTOR would be positionally unsyncable across that sort; that
    // is the shape being rejected here, not per-accept binding as such.)
    if !state.last_loop_action_sequence.is_empty() {
        let stashed_before = state
            .pending_unbounded_materialization
            .get(&proposal.proposer)
            .map_or(0, Vec::len);
        materialize_object_growth_shortcut(state, result, proposal);
        if state
            .pending_unbounded_materialization
            .get(&proposal.proposer)
            .map_or(0, Vec::len)
            > stashed_before
        {
            state
                .pending_materialization_count
                .entry(proposal.proposer)
                .and_modify(|bound| *bound = (*bound).min(n))
                .or_insert(n);
        }
        return;
    }

    let template = proposal.template.clone();
    // CR 732.2a: the per-period signature the offer published, carried verbatim onto the
    // proposal. `None` for every producer that states none, and that `None` is what keeps
    // every pre-bounded offer's drive byte-identical: no frame delimiter, no conformance
    // check, board recurrence alone — exactly the shipped behavior.
    let per_cycle = proposal.per_cycle.as_ref();

    // Last fully-completed cycle (clean owned O(1) rollback); starts at the offer state —
    // `apply_confirmed_shortcut`'s doc comment establishes the board is unchanged since the
    // offer (Declare/Accept touch only the protocol, never the board).
    let mut committed = state.clone();

    // The recurrence boundary is the loop's canonical per-cycle SETTLE beat —
    // `Priority{active_player}` — the same beat-kind the detector ring samples
    // (`resolved_this_beat` gate above). `committed.waiting_for` is still
    // `RespondToShortcut`/`LoopShortcut` at entry (never `Priority`), so seed a settled
    // priority beat before capturing the boundary. `reset_priority` zeroes
    // `priority_pass_count` and sets `priority_player`; `waiting_for` is set explicitly
    // here (reset_priority does not touch it). `loop_states_equal_modulo_resources` /
    // `loop_states_cover_modulo_growth` both normalize internally (`project_out_resources`
    // → `normalize_for_loop`), so the extra `.normalize_for_loop()` here is redundant with
    // that internal call but harmless (idempotent) — kept for a self-contained boundary
    // value whose `waiting_for`/ring fields are already canonical at the call sites below.
    let boundary = {
        let mut seed = committed.clone();
        priority::reset_priority(&mut seed);
        seed.waiting_for = WaitingFor::Priority {
            player: seed.active_player,
        };
        seed.normalize_for_loop()
    };

    let cycle_beat_cap = auto_pass_loop_max_iterations(&committed);

    'cycles: for i in 0..n {
        // CR 732.2a predictability firewall: `predictability_gate(t, &[])` is a WIRED
        // FORMAL no-op this phase — empty `required_slots` ⇒ always `Ok`
        // (decision_template.rs). The loop-body slot enumerator that would populate
        // `required_slots` ships with the deferred mid-drive injector; a choice-free
        // drain has no slots to pin. The REAL load-bearing firewall is `resolve` below.
        if let Some(t) = &template {
            if crate::analysis::decision_template::predictability_gate(t, &[]).is_err() {
                break 'cycles; // unreachable with &[]; wired for the injector phase
            }
            // CR 608.2b (target-legality re-check) + CR 400.7 (object incarnation
            // re-bind): re-resolve every pinned decision against the last COMMITTED
            // board before driving this cycle. Stale/absent source ⇒ IllegalTarget /
            // MissingSource ⇒ abort to manual play.
            if crate::analysis::decision_template::resolve(t, i, &committed).is_err() {
                break 'cycles;
            }
        }

        // Drive one whole cycle via the shared driver. Behavior-identical to the former
        // inline beat loop for a non-targeted `Fixed(N)` drain (which raises no mid-cycle
        // prompt, so the injector is inert); a targeted drive additionally answers each
        // OrderTriggers / target prompt from the pins.
        match drive_one_shortcut_cycle(
            &committed,
            &boundary,
            template.as_ref(),
            i,
            cycle_beat_cap,
            per_cycle.map(|pd| pd.frames_per_period),
        ) {
            CycleOutcome::Recurred {
                state: s,
                mut events,
            } => {
                // CR 732.2a "predictable results" + CR 704.5a: the CONFORMANCE CHECK the
                // published signature exists for. `elimination_bounds` divided the CR 704
                // headroom by `per_cycle.delta`, so a committed cycle that moved a
                // DIFFERENT amount invalidates the very bound the table agreed to — the
                // remaining repetitions could carry a seat past a threshold inside the
                // proposal. Measured before commit, on the same axes the bound reads, and
                // fail-closed: a divergent cycle is dropped whole and the drive hands back
                // to manual play with the last conforming cycle intact.
                if let Some(pd) = per_cycle {
                    let actual = crate::analysis::resource::ResourceVector::delta(
                        &crate::analysis::resource::ResourceVector::snapshot(&committed),
                        &crate::analysis::resource::ResourceVector::snapshot(&s),
                    );
                    if actual != pd.delta {
                        break 'cycles;
                    }
                }
                committed = *s; // ATOMIC: commit state ...
                result.events.append(&mut events); // ... with its events together
                continue 'cycles;
            }
            // Cross-lethal: COMMIT + STOP. CR 704.5a: the win is already applied to `work`
            // (SBA → GameOver in `events`, `waiting_for = GameOver`). Do NOT roll back, NOT
            // `mark_unbounded_loop` (finite ≠ unbounded — contrast the UntilLethal arm).
            CycleOutcome::CrossLethal {
                state: s,
                winner,
                mut events,
            } => {
                *state = *s;
                result.events.append(&mut events);
                result.waiting_for = WaitingFor::GameOver { winner };
                return;
            }
            // Runaway cap / unpinned prompt / engine error ⇒ abort to manual. The aborting
            // cycle's events were already dropped (no partial-cycle event leak).
            //
            // ⚠ THE TWO LETHAL ARMS ARE ASYMMETRIC, and a future drive must learn that here
            // rather than by accident. A cycle that takes EVERY remaining opponent to 0 at once
            // reaches `WaitingFor::GameOver` and lands in the `CrossLethal` arm above: it
            // COMMITS and the game ends. A cycle that takes ONE seat to 0 while >= 2 players
            // survive raises no `GameOver` (CR 104.2a crowns nobody), the loop's shape changes
            // under it as the drained seat leaves, no settle beat recurs, and it arrives HERE —
            // THAT CYCLE rolls back whole while every PRIOR conforming cycle stays committed,
            // `eliminated` is empty, priority is handed back. (It is not a whole-drive rollback:
            // the `break` below falls through to `*state = committed`, which is the last WHOLE
            // cycle, not the offer state.) Both are out of contract for a legitimately-derived
            // bound (`elimination_bounds` reserves `life - 1` of CR 704.5a headroom, so a
            // within-bound count crosses no threshold), so either arm means the published bound
            // was wrong. The atomic per-cycle refusal is the designed property — NO HALF-APPLIED
            // PERIOD, EVER — and it is why the out-of-contract cycle is dropped rather than
            // materialized: its remaining repetitions were bounded by a delta the board stops
            // moving the moment a drain target leaves the game. Rows:
            // `bounded_fixed_drive_stops_at_the_first_lethal_cycle` (total wipe) and
            // `bounded_fixed_drive_rolls_back_a_partial_crossing_cycle` (partial).
            CycleOutcome::Abort => break 'cycles,
        }
    }

    // Reached by: n cycles done with no cross-lethal, OR any abort (`break 'cycles`).
    // Commit the last WHOLE cycle; the aborting iteration's `ev` was already dropped (no
    // partial-cycle event leak). Ring-clear BEFORE handback so this same `apply()` does
    // not instantly re-emit a fresh offer for the same (now-interrupted) loop; a later
    // beat re-detects genuinely.
    *state = committed;
    state.loop_detect_ring.clear();
    priority::reset_priority(state);
    state.waiting_for = WaitingFor::Priority {
        player: living_priority_seat(state),
    };
    result.waiting_for = state.waiting_for.clone();
}

/// PR-7 Phase 4d-ii: the injector aborted a driven recast cycle ⇒ fall closed to manual
/// play. No payload — a marker so the drive loop is exhaustive over `WaitingFor` with an
/// explicit `Err` on any unpinned prompt (S1, CR 732.2a "no conditional actions").
#[derive(Debug)]
struct RecastAbort;

/// CR 602.2a / CR 605.3a / CR 732.2a (G4): capture the `AbilityDefinition` an indexed
/// activation names, so the drive can re-validate the positional `ability_index` by `Eq` each
/// iteration (a layer re-eval that reorders/removes the granted ability ⇒ fail-closed abort).
/// `None` for a `Recast` or a subtype-derived land-mana fallback with no printed definition.
fn loop_action_expected_def(
    state: &GameState,
    ctx: &crate::types::game_state::LoopActionContext,
) -> Option<crate::types::ability::AbilityDefinition> {
    match &ctx.action {
        crate::types::game_state::LoopAction::Recast { .. } => None,
        crate::types::game_state::LoopAction::Activate {
            source_id,
            ability_index,
        } => state
            .objects
            .get(source_id)?
            .abilities
            .get(*ability_index)
            .cloned(),
        crate::types::game_state::LoopAction::TapLandForMana { selection } => {
            selection.ability_index.and_then(|ability_index| {
                state
                    .objects
                    .get(&selection.source.object_id)?
                    .abilities
                    .get(ability_index)
                    .cloned()
            })
        }
    }
}

/// P7 v3 (CR 602.2a + CR 732.2a): append a driving activation to the current loop-action period
/// (`state.last_loop_action_sequence`). A CONTROLLER CHANGE resets to a fresh single-step period
/// (a period belongs to one controller — a mid-period controller switch is a different loop); a
/// LENGTH CAP bounds an adversarial/incidental run of unrelated activations. Callers gate on
/// `samples() && !in_simulation_probe()`, so the detection/materialize drive never grows the
/// sequence (it is COMPARED byte-for-byte across the cover frames, resource.rs).
fn accumulate_loop_action_step(
    state: &mut GameState,
    step: crate::types::game_state::LoopActionContext,
) {
    // ponytail: cap at 16 steps — a real loop period is 2-4 activations; raise only if a real
    // >16-action period appears. Bounds a hostile/incidental run before the drive+cover reject it.
    const MAX_LOOP_PERIOD_STEPS: usize = 16;
    let controller_changed = state
        .last_loop_action_sequence
        .first()
        .is_some_and(|s| s.controller != step.controller);
    if controller_changed || state.last_loop_action_sequence.len() >= MAX_LOOP_PERIOD_STEPS {
        state.last_loop_action_sequence.clear();
    }
    state.last_loop_action_sequence.push(step);
}

/// CR 605.3a + CR 732.2a: record one successful off-stack mana activation as a driving
/// action in the current loop period. Both public mana-action surfaces delegate here so
/// sampling/probe gates, battlefield-source validation, and context construction cannot drift.
fn record_mana_loop_action_step(
    state: &mut GameState,
    controller: PlayerId,
    source_id: ObjectId,
    action: crate::types::game_state::LoopAction,
) {
    if !state.loop_detection.samples() || in_simulation_probe() {
        return;
    }
    let Some(source) = state
        .objects
        .get(&source_id)
        .filter(|object| object.zone == Zone::Battlefield)
    else {
        state.last_loop_action_sequence.clear();
        return;
    };
    let step = crate::types::game_state::LoopActionContext {
        card_id: source.card_id,
        controller,
        action,
        convoke: None,
        // Fixed in-cycle choices are appended at their own reducer arms via `record_loop_pin`.
        pins: Vec::new(),
    };
    accumulate_loop_action_step(state, step);
}

/// FIX-1 (CR 732.2a): append a recorded fixed in-cycle player choice (tap-cost target, mana
/// color, or proliferate target) to the CURRENT loop-period step — the driving `Activate` step
/// the choice belongs to (`last_mut`; the Relic activation for the Kilo loop, whose cost/trigger
/// choices are all answered before the next driving activation appends a new step). Gated EXACTLY
/// like the samplers (`samples() && !in_simulation_probe()`): #4603-Off never records, and the
/// detection/materialize drive (under `SimulationProbeGuard`) REPLAYS pins without re-recording
/// them — keeping the sequence byte-stable across the cover's `s_n`/`s_n1`/`s_n2` frames. No-op
/// unless a period is accumulating for `controller` (there is no step to attach the pin to
/// otherwise, and a mid-period controller mismatch is a different loop).
fn record_loop_pin(
    state: &mut GameState,
    controller: PlayerId,
    pin: crate::analysis::decision_template::PinnedDecision,
) {
    if !state.loop_detection.samples() || in_simulation_probe() {
        return;
    }
    if let Some(step) = state.last_loop_action_sequence.last_mut() {
        if step.controller == controller {
            step.pins.push(pin);
        }
    }
}

/// FIX-1 (CR 608.2d): the WUBRG color of a `SingleColor` mana choice, for pinning an "add one mana
/// of any color" loop-neutrality choice. `None` for a colorless single choice or a `Combination`
/// (not this pinnable loop class — the drive then aborts unpinned at the `ChooseManaColor` beat,
/// fail-safe: no false offer).
fn pinnable_mana_color(
    choice: &crate::types::game_state::ManaChoice,
) -> Option<crate::types::mana::ManaColor> {
    use crate::types::game_state::ManaChoice;
    use crate::types::mana::{ManaColor, ManaType};
    match choice {
        ManaChoice::SingleColor(ManaType::White) => Some(ManaColor::White),
        ManaChoice::SingleColor(ManaType::Blue) => Some(ManaColor::Blue),
        ManaChoice::SingleColor(ManaType::Black) => Some(ManaColor::Black),
        ManaChoice::SingleColor(ManaType::Red) => Some(ManaColor::Red),
        ManaChoice::SingleColor(ManaType::Green) => Some(ManaColor::Green),
        ManaChoice::SingleColor(ManaType::Colorless) | ManaChoice::Combination(_) => None,
    }
}

/// FIX-1 (CR 400.7): a live-object identity source for a pin — `ThisObject` bound to the object's
/// CURRENT incarnation, so a re-entered permanent (new incarnation) stops matching and the loop is
/// correctly re-detected rather than falsely replayed. `None` if the object is absent.
pub(crate) fn object_decision_source(
    state: &GameState,
    id: ObjectId,
) -> Option<crate::types::game_state::YieldTarget> {
    let o = state.objects.get(&id)?;
    Some(crate::types::game_state::YieldTarget::ThisObject {
        source_id: id,
        incarnation: Some(o.incarnation),
        trigger_description: None,
    })
}

/// FIX-1 (CR 608.2b): the concrete targets of the recorded `Targets` pin whose slot source
/// re-binds LIVE to `source_id` this iteration (the beat's cost / trigger source, e.g. the Relic
/// cost source for a tap-cost pin or the Kilo trigger source for a proliferate pin). Resolving the
/// WHOLE `template` means ANY pin that no longer resolves to a live legal object (a target left
/// its zone) aborts the whole beat fail-closed — a broken loop never certifies. `Err(RecastAbort)`
/// if no `Targets` pin's source matches `source_id`.
fn pinned_targets_for_source(
    template: &crate::analysis::decision_template::DecisionTemplate,
    iteration: crate::analysis::decision_template::IterationIndex,
    clone: &GameState,
    source_id: ObjectId,
) -> Result<Vec<crate::analysis::decision_template::ConcreteTarget>, RecastAbort> {
    use crate::analysis::decision_template::{resolve, resolve_source, ConcreteDecision};
    let decisions = resolve(template, iteration, clone).map_err(|_| RecastAbort)?;
    for d in decisions {
        if let ConcreteDecision::Targets { slot, targets } = d {
            if resolve_source(&slot.source, clone) == Some(source_id) {
                return Ok(targets);
            }
        }
    }
    Err(RecastAbort)
}

/// FIX-1 (CR 608.2d): the recorded mana color of the `ManaColor` pin whose slot source is
/// `source_id` (the driving mana ability's source). `Err(RecastAbort)` if unpinned.
fn pinned_mana_color_for_source(
    template: &crate::analysis::decision_template::DecisionTemplate,
    iteration: crate::analysis::decision_template::IterationIndex,
    clone: &GameState,
    source_id: ObjectId,
) -> Result<crate::types::mana::ManaColor, RecastAbort> {
    use crate::analysis::decision_template::{resolve, resolve_source, ConcreteDecision};
    let decisions = resolve(template, iteration, clone).map_err(|_| RecastAbort)?;
    for d in decisions {
        if let ConcreteDecision::ManaColor { slot, color } = d {
            if resolve_source(&slot.source, clone) == Some(source_id) {
                return Ok(color);
            }
        }
    }
    Err(RecastAbort)
}

/// CR 601.2b + CR 608.2b + CR 400.7: drive ONE full recast iteration on the clone by
/// answering each mid-cast prompt from `template` (the ConvokeTaps pin) + `ctx` (the
/// buyback decision). Reuses the ENTIRE cast state machine via the INTERNAL `apply_action`
/// path (never the top-level `apply`/reconcile boundary, so the detection hook cannot
/// recurse), adding ZERO casting rules. EXHAUSTIVE over `WaitingFor`: any unpinned prompt
/// ⇒ `Err(RecastAbort)` ⇒ fail-closed to manual (no silent `_` that would fabricate a
/// bogus offer). `clone` MUST be at `Priority{ctx.controller}` with an empty stack.
fn drive_loop_action_iteration(
    clone: &mut GameState,
    template: &crate::analysis::decision_template::DecisionTemplate,
    ctx: &crate::types::game_state::LoopActionContext,
    iteration: crate::analysis::decision_template::IterationIndex,
    expected_def: Option<&crate::types::ability::AbilityDefinition>,
) -> Result<(), RecastAbort> {
    use crate::types::game_state::LoopAction;
    // Dispatch the OPENER on the captured action; the beat-loop tail below is action-agnostic.
    match &ctx.action {
        // CR 400.7 + CR 601.2a: re-find the recast card LIVE in its castable zone (a fresh
        // incarnation on each hand-return). Absent ⇒ abort (B3: a no-buyback recast went to
        // the graveyard). Lowest ObjectId ⇒ deterministic.
        LoopAction::Recast { from_zone, .. } => {
            let recast_id = clone
                .objects
                .values()
                .filter(|o| {
                    o.card_id == ctx.card_id
                        && o.zone == *from_zone
                        && o.controller == ctx.controller
                })
                .map(|o| o.id)
                .min_by_key(|id| id.0)
                .ok_or(RecastAbort)?;
            apply_action(
                clone,
                ctx.controller,
                GameAction::CastSpell {
                    object_id: recast_id,
                    card_id: ctx.card_id,
                    targets: vec![],
                    payment_mode: crate::types::game_state::CastPaymentMode::Auto,
                },
                None,
            )
            .map_err(|_| RecastAbort)?;
        }
        // CR 602.2a: re-activate the pinned permanent's ability. G3: pin by `ObjectId` (a plain
        // token is `CardId(0)`, so a card-identity re-find would match the fodder the loop
        // manufactures). G4: re-validate the positional `ability_index` against the captured
        // def by `Eq` — a layer re-eval that reordered/removed it ⇒ fail-closed abort (CR 602.5a
        // legality is then the reducer's job — an illegal 2nd activation returns Err below).
        LoopAction::Activate {
            source_id,
            ability_index,
        } => {
            let expected = expected_def.ok_or(RecastAbort)?;
            let src = clone.objects.get(source_id).ok_or(RecastAbort)?;
            if src.zone != Zone::Battlefield
                || src.controller != ctx.controller
                || src.card_id != ctx.card_id
                || src.abilities.get(*ability_index) != Some(expected)
            {
                return Err(RecastAbort);
            }
            apply_action(
                clone,
                ctx.controller,
                GameAction::ActivateAbility {
                    source_id: *source_id,
                    ability_index: *ability_index,
                },
                None,
            )
            .map_err(|_| RecastAbort)?;
        }
        // CR 605.3a: replay the exact semantic land-mana choice. Indexed rows retain their
        // captured definition identity; subtype-derived fallback rows have no definition and
        // retain their typed mana identity in `selection`. The reducer re-enumerates live options
        // and requires one exact semantic match before it mutates the clone.
        LoopAction::TapLandForMana { selection } => {
            let source_id = selection.source.object_id;
            let source = clone.objects.get(&source_id).ok_or(RecastAbort)?;
            if source.zone != Zone::Battlefield
                || source.controller != ctx.controller
                || source.card_id != ctx.card_id
            {
                return Err(RecastAbort);
            }
            match selection.ability_index {
                Some(ability_index) if source.abilities.get(ability_index) != expected_def => {
                    return Err(RecastAbort);
                }
                Some(_) => {}
                None if expected_def.is_some() => return Err(RecastAbort),
                None => {}
            }
            apply_action(
                clone,
                ctx.controller,
                GameAction::TapLandForMana {
                    selection: selection.clone(),
                },
                None,
            )
            .map_err(|_| RecastAbort)?;
        }
    }

    let beat_cap = auto_pass_loop_max_iterations(clone);
    for _ in 0..beat_cap {
        let actor = crate::game::turn_control::authorized_submitter(clone).ok_or(RecastAbort)?;
        match clone.waiting_for.clone() {
            // CR 601.2f/702.27a: re-pay (or decline) the buyback additional cost — RECAST-only.
            // CR 732.2a "can't include conditional actions": an activation that opens an
            // optional-cost window is not a pinned shortcut ⇒ fail-closed abort.
            WaitingFor::OptionalCostChoice { .. } => {
                let LoopAction::Recast { uses_buyback, .. } = &ctx.action else {
                    return Err(RecastAbort);
                };
                apply_action(
                    clone,
                    actor,
                    GameAction::DecideOptionalCost {
                        pay: uses_buyback.pays(),
                    },
                    None,
                )
                .map_err(|_| RecastAbort)?;
            }
            // CR 601.2h + CR 702.51a/b: resolve the ConvokeTaps pin LIVE, tap each chosen
            // creature, then finalize the (now convoke-paid) cost. Affinity auto-reduces
            // the generic against the grown board with NO pin (CR 702.41a).
            WaitingFor::ManaPayment { .. } => {
                let decisions =
                    crate::analysis::decision_template::resolve(template, iteration, clone)
                        .map_err(|_| RecastAbort)?;
                use crate::analysis::decision_template::ConcreteDecision;
                for d in decisions {
                    // EXHAUSTIVE (mirrors the same-diff triggers.rs precedent): a recast
                    // template emits ONLY ConvokeTaps pins, so every other decision kind is
                    // unpinned for this class ⇒ fail-CLOSED abort. Listing the variants (no
                    // `_`) makes a future ConcreteDecision variant BUILD-BREAK here rather than
                    // be silently dropped.
                    match d {
                        ConcreteDecision::ConvokeTaps { creatures, .. } => {
                            for (object_id, mana_type) in creatures {
                                apply_action(
                                    clone,
                                    actor,
                                    GameAction::TapForConvoke {
                                        object_id,
                                        mana_type,
                                    },
                                    None,
                                )
                                .map_err(|_| RecastAbort)?;
                            }
                        }
                        ConcreteDecision::Order { .. }
                        | ConcreteDecision::Targets { .. }
                        | ConcreteDecision::Mode { .. }
                        | ConcreteDecision::MayChoice { .. }
                        | ConcreteDecision::UnlessBreak { .. }
                        // CR 608.2d: a ManaColor pin is consumed at the `ChooseManaColor` beat
                        // (E11), never at a convoke `ManaPayment` beat ⇒ fail-closed here.
                        | ConcreteDecision::ManaColor { .. } => return Err(RecastAbort),
                    }
                }
                apply_action(clone, actor, GameAction::PassPriority, None)
                    .map_err(|_| RecastAbort)?;
            }
            // CR 601.2i: the spell is on the stack ⇒ pass to let it resolve; an empty stack
            // at a priority beat is the per-cycle SETTLE boundary — iteration complete.
            WaitingFor::Priority { .. } => {
                if clone.stack.is_empty() {
                    return Ok(());
                }
                apply_action(clone, actor, GameAction::PassPriority, None)
                    .map_err(|_| RecastAbort)?;
            }
            // FIX-1 (E11) CR 605.1a + CR 608.2b: the driving mana ability's tap cost ("tap an
            // untapped legendary creature you control") — replay the recorded tap-target pin,
            // matched by the mana-ability COST SOURCE (from `resume`). Only a `TapCreatures` cost
            // resuming a MANA ABILITY is a pinned loop cost; every other PayCost shape is unpinned
            // for this class ⇒ falls to the fail-closed `_` below.
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                resume: CostResume::ManaAbility { mana_ability },
                ..
            } => {
                let cost_source = mana_ability.source_id;
                let targets = pinned_targets_for_source(template, iteration, clone, cost_source)?;
                let cards: Vec<ObjectId> = targets
                    .into_iter()
                    .map(|t| match t {
                        crate::analysis::decision_template::ConcreteTarget::Object(id) => Ok(id),
                        // A tap cost taps OBJECTS; a player pin here is malformed ⇒ fail-closed.
                        crate::analysis::decision_template::ConcreteTarget::Player(_) => {
                            Err(RecastAbort)
                        }
                    })
                    .collect::<Result<_, _>>()?;
                apply_action(clone, actor, GameAction::SelectCards { cards }, None)
                    .map_err(|_| RecastAbort)?;
            }
            // FIX-1 (E11) CR 608.2d: "add one mana of any color" — replay the recorded color pin
            // (matched by the mana-ability source), fixing the loop's mana-neutrality color (Blue
            // to pay Freed's `{U}`). A resolving-effect color choice is not a pinned mana-ability
            // loop cost ⇒ fail-closed.
            WaitingFor::ChooseManaColor { context, .. } => {
                let source = match &context {
                    crate::types::game_state::ManaChoiceContext::ManaAbility(p) => p.source_id,
                    crate::types::game_state::ManaChoiceContext::ResolvingEffect(_) => {
                        return Err(RecastAbort)
                    }
                };
                let color = pinned_mana_color_for_source(template, iteration, clone, source)?;
                apply_action(
                    clone,
                    actor,
                    GameAction::ChooseManaColor {
                        choice: crate::types::game_state::ManaChoice::SingleColor(color.into()),
                        count: 1,
                    },
                    None,
                )
                .map_err(|_| RecastAbort)?;
            }
            // FIX-1 (E11) CR 701.34a: the driving permanent's becomes-tapped proliferate trigger —
            // replay the recorded proliferate-target pin, matched by the pending proliferate's
            // trigger source id (Kilo). Replaying the RECORDED selection (never "all eligible")
            // keeps an opponent's counters/poison out of the growth ⇒ no loss axis introduced.
            WaitingFor::ProliferateChoice { .. } => {
                let prolif_source = clone
                    .active_proliferate_frame()
                    .map(|pending| pending.source_id)
                    .ok_or(RecastAbort)?;
                let targets = pinned_targets_for_source(template, iteration, clone, prolif_source)?;
                let target_refs: Vec<crate::types::ability::TargetRef> = targets
                    .into_iter()
                    .map(|t| match t {
                        crate::analysis::decision_template::ConcreteTarget::Object(id) => {
                            crate::types::ability::TargetRef::Object(id)
                        }
                        crate::analysis::decision_template::ConcreteTarget::Player(p) => {
                            crate::types::ability::TargetRef::Player(p)
                        }
                    })
                    .collect();
                apply_action(
                    clone,
                    actor,
                    GameAction::SelectTargets {
                        targets: target_refs,
                    },
                    None,
                )
                .map_err(|_| RecastAbort)?;
            }
            // CR 732.2a "no conditional actions": any other prompt (target / mode / X /
            // may) is unpinned for this recast class ⇒ fail-closed abort.
            _ => return Err(RecastAbort),
        }
    }
    Err(RecastAbort)
}

/// P7 v3 (CR 732.2a): drive ONE full loop PERIOD — the ordered sequence of driving actions — on
/// the clone by driving each captured step in order through `drive_loop_action_iteration` (which
/// settles every beat to its OWN empty-stack `Priority` boundary, CR 601.2i). A 1-element
/// sequence is the single-action recast/token case (byte-identical to the pre-P7 single drive); a
/// 2+ element sequence is a multi-activation engine (e.g. Basalt Monolith's off-stack mana beat,
/// CR 605.3b, then its on-stack `{3}: Untap` beat). Each step's `expected_def` re-validates its
/// `Activate` `ability_index` by `Eq` (G4); a `Recast` step's is `None`. ANY step's `RecastAbort`
/// aborts the whole period fail-closed — a partial/broken period never certifies (the drive+cover
/// IS the period-boundary check, so no explicit boundary detection is needed in the reducer).
fn drive_loop_sequence_iteration(
    clone: &mut GameState,
    seq: &[crate::types::game_state::LoopActionContext],
    iteration: crate::analysis::decision_template::IterationIndex,
    expected_defs: &[Option<crate::types::ability::AbilityDefinition>],
) -> Result<(), RecastAbort> {
    for (step, expected) in seq.iter().zip(expected_defs.iter()) {
        // Each step's template carries its OWN convoke pin (only a `Recast` step has convoke; an
        // `Activate` step yields an empty template) — build per-step so a mixed period stays honest.
        let template = build_recast_template(step);
        drive_loop_action_iteration(clone, &template, step, iteration, expected.as_ref())?;
    }
    Ok(())
}

/// CR 601.2h + CR 702.51a: the CR 732.2a decision template for a buyback+convoke recast
/// loop. Carries a single `ConvokeTaps` pin (when the recast pays convoke) whose slot is
/// the CARD-identity source (`AllCopies` — survives the per-iteration incarnation churn,
/// CR 400.7). The presence of the pin is the object-growth routing signal.
fn build_recast_template(
    ctx: &crate::types::game_state::LoopActionContext,
) -> crate::analysis::decision_template::DecisionTemplate {
    use crate::analysis::decision_template::{
        DecisionGroupKey, DecisionKind, DecisionSlot, IterationCount, PinnedDecision, ReplayMode,
    };
    let source = crate::types::game_state::YieldTarget::AllCopies {
        card_id: ctx.card_id,
        trigger_description: None,
    };
    // FIX-1 (B2#8): the recorded fixed in-cycle choices (tap-cost target, mana color, proliferate
    // target) drive the replay; a convoke recast additionally carries its live-rebinding
    // ConvokeTaps pin. `build_shortcut_schema` reifies this SAME list (one template, single source
    // of truth — CR 608.2b live re-binding).
    let mut decisions = ctx.pins.clone();
    if ctx.convoke.is_some() {
        decisions.push(PinnedDecision::ConvokeTaps {
            slot: DecisionSlot {
                source: source.clone(),
                index: 0,
            },
        });
    }
    crate::analysis::decision_template::DecisionTemplate {
        owner: ctx.controller,
        decisions,
        // The count is a placeholder — the real `Fixed(N)` comes from the proposer's
        // `DeclareShortcut`; nothing reads `template.replay.count`.
        replay: ReplayMode::Scheduled {
            count: IterationCount::Fixed(0),
        },
        key: DecisionGroupKey::from_sources(&[source], DecisionKind::LoopChoice),
    }
}

/// CR 400.7: normalize a settle frame for the object-growth board cover — strip the
/// self-returning recast card and clear the per-cycle token-id bookkeeping. Both churn a
/// FRESH ObjectId every cycle (the card via its hand→stack→hand round-trip; the
/// `last_created_token_ids` anaphora slot via each new token), which the id-keyed
/// stable-engine compare would read as a false board drift. The recast card's presence in
/// `ctx.from_zone` is a verified loop invariant (the hook precondition + the injector's
/// per-cycle re-find), and `last_created_token_ids` is pure ephemeral anaphora bookkeeping
/// (no observer reads it at the empty-stack settle beat), so clearing them identically from
/// every frame is fail-safe — any OTHER stable object still compares by id.
fn normalize_recast_frame(
    state: &GameState,
    ctx: &crate::types::game_state::LoopActionContext,
) -> GameState {
    let mut s = state.clone();
    // CR 400.7 (M15-b): stripping the self-returning recast card is RECAST-ONLY. An `Activate`
    // ctx has `from_zone == Battlefield` (its source is a resident permanent), so applying the
    // strip would DELETE the driving permanent from every comparison frame. The three token-id
    // bookkeeping clears below apply to BOTH actions.
    if let crate::types::game_state::LoopAction::Recast { from_zone, .. } = &ctx.action {
        let ids: Vec<ObjectId> = s
            .objects
            .values()
            .filter(|o| {
                o.card_id == ctx.card_id && o.zone == *from_zone && o.controller == ctx.controller
            })
            .map(|o| o.id)
            .collect();
        for id in &ids {
            s.objects.remove(id);
        }
        if let Some(p) = s.players.iter_mut().find(|p| p.id == ctx.controller) {
            p.hand.retain(|id| !ids.contains(id)); // allow-raw-zone: prunes a discarded recast comparison-frame CLONE (fn takes &GameState, returns a normalized clone) - not a gameplay zone event
            p.graveyard.retain(|id| !ids.contains(id)); // allow-raw-zone: prunes a discarded recast comparison-frame CLONE (fn takes &GameState, returns a normalized clone) - not a gameplay zone event
            p.library.retain(|id| !ids.contains(id)); // allow-raw-zone: prunes a discarded recast comparison-frame CLONE (fn takes &GameState, returns a normalized clone) - not a gameplay zone event
        }
    }
    // CR 608.2 anaphora / display bookkeeping: the "last created token / revealed /
    // zone-changed" id slots churn a fresh id each cycle. No observer reads them at the
    // empty-stack settle beat, so clearing them is fail-safe for the board cover.
    s.last_created_token_ids.clear();
    s.last_revealed_ids.clear();
    s.last_zone_changed_ids.clear();
    s
}

/// CR 111.10: the content class of the reproduced token — the single battlefield object
/// present in `after` but absent from `before` (the one predefined token the recast
/// creates). `None` unless EXACTLY one new battlefield object appeared (the target class
/// creates one Saproling; zero or several ⇒ not this shape ⇒ fail-closed).
fn derived_fodder_class(
    before: &GameState,
    after: &GameState,
) -> Option<crate::game::game_object::GameObject> {
    let mut new_ids = after
        .battlefield
        .iter()
        .copied()
        .filter(|id| !before.battlefield.contains(id));
    let id = new_ids.next()?;
    if new_ids.next().is_some() {
        return None;
    }
    after.objects.get(&id).cloned()
}

/// The reproduced fodder class of one accepted object-growth period, plus whether that
/// period's per-cycle cost TAPS a fodder member. CR 702.51a: a convoke/tap-cost period taps a
/// fodder each cycle → the ∞ pile is genuinely TAPPED; a mana-paid period creates the fodder
/// untapped (CR 110.5b) and taps nothing → untapped-growth. Both measured on the SAME
/// clone-drive that derives the class.
struct PeriodFodder {
    class: crate::game::game_object::GameObject,
    taps_fodder: bool,
}

/// CR 732.2a / CR 111.1: seed a `Priority{controller}` window and drive ONE iteration of
/// `last_loop_action_sequence` on THROWAWAY clones, returning the `(before, after)` frames.
/// The shared seed+drive kernel of the accept-time re-derivations — `current_period_fodder`
/// (object-growth ∞ pile) and `current_period_counter_targets` (counter-growth ∞ targets)
/// both diff these two frames. `None` when the sequence is empty. Mirrors the detection
/// drive exactly: same `SimulationProbeGuard` re-entrancy guard (HELD across the drive so
/// the injector's internal `apply_action` never recurses into the shortcut hooks), same
/// `drive_loop_sequence_iteration`.
///
/// The accept beat's `waiting_for` is `RespondToShortcut`, NOT `Priority`, so the recast
/// cannot proceed from `state` as-is — seed a `Priority{controller}` window on the driven
/// frame exactly as `apply_until_lethal_shortcut` does before its identical drive.
///
/// INV (clone-only): takes `&GameState` (SHARED borrow) ⇒ a live write is TYPE-IMPOSSIBLE.
/// The `Priority{controller}` seed and the drive both mutate `before`/`after`, which are
/// THROWAWAY clones (`state.clone()` → `before.clone()`); live `state.waiting_for` is never
/// touched, so this cannot corrupt the real accept flow (INV-1, mirrors
/// `try_offer_object_growth_shortcut`).
fn drive_one_period_frames(state: &GameState) -> Option<(GameState, GameState)> {
    let seq = state.last_loop_action_sequence.clone();
    if seq.is_empty() {
        return None;
    }
    let controller = seq[0].controller;
    let expected_defs: Vec<Option<crate::types::ability::AbilityDefinition>> = seq
        .iter()
        .map(|c| loop_action_expected_def(state, c))
        .collect();
    let _probe = SimulationProbeGuard::enter();
    // Seed + drive on THROWAWAY clones only (never `state`): `before` is the pre-drive frame,
    // `after` the post-one-period frame; callers diff the two clones.
    let mut before = state.clone();
    priority::reset_priority(&mut before);
    before.waiting_for = WaitingFor::Priority { player: controller };
    let mut after = before.clone();
    drive_loop_sequence_iteration(&mut after, &seq, 0, &expected_defs).ok()?;
    Some((before, after))
}

/// CR 732.2a / CR 111.1: re-derive the reproduced fodder class of the accepted
/// object-growth period by driving ONE iteration of `last_loop_action_sequence` on a
/// clone (`drive_one_period_frames`), and measure whether that period taps a fodder member.
/// `None` when the sequence is empty or the period reproduces no single new battlefield
/// object (a multi-activation mana engine → no fodder pile to display). Same
/// `derived_fodder_class` single-new-object rule as the detection drive. Called at
/// materialize (with the sequence still intact) to snapshot the ∞ pile and its tapped-growth
/// axis. The post-drive `derived_fodder_class` / `tapped_fodder_members` inspections are pure
/// (they never read the probe flag), so running them after the shared kernel's guard has
/// dropped is behavior-preserving.
fn current_period_fodder(state: &GameState) -> Option<PeriodFodder> {
    let controller = state.last_loop_action_sequence.first()?.controller;
    let (before, after) = drive_one_period_frames(state)?;
    let class = derived_fodder_class(&before, &after)?;
    // CR 702.51a: the period taps a fodder iff the driven tapped-fodder multiset GREW across the
    // one-period drive. `select_convoke_taps` sorts fodder (`is_token`) FIRST, so a convoke/
    // tap-cost period taps a reproduced fodder → this grows; a mana-paid untapped-growth period
    // taps nothing → this is FALSE. This is exactly the tapped-growth axis the
    // `board_covers_modulo_fodder` `>=` untapped cover (resource.rs) does not distinguish.
    let taps_fodder = crate::analysis::resource::tapped_fodder_members(&after, controller, &class)
        .len()
        > crate::analysis::resource::tapped_fodder_members(&before, controller, &class).len();
    Some(PeriodFodder { class, taps_fodder })
}

/// CR 732.2a / CR 701.34a (proliferate): re-derive the per-object `(ObjectId, CounterType)`
/// targets whose PRESERVED `Generic` counters strictly grew across one accepted
/// counter-growth period — the DISPLAY-only `∞` counter channel. The offer certificate's
/// unbounded axis is object-AGNOSTIC (`Counter(Other, Other)`), so the concrete object id /
/// counter type is NOT recoverable from the axis; re-derive it the same way
/// `current_period_fodder` derives the fodder class — drive ONE period on a clone (shared
/// `drive_one_period_frames`) and diff `Generic` counters (`grown_generic_counter_targets`).
/// Empty when the sequence is empty or the period grows no `Generic` counter (a mana / token
/// / object-growth loop). General over the class (proliferate charge / One-Ring burden),
/// never one card. DISPLAY-ONLY: the caller marks the pill to render `∞` without mutating the
/// real counter count.
fn current_period_counter_targets(
    state: &GameState,
) -> Vec<(ObjectId, crate::types::counter::CounterType)> {
    let Some((before, after)) = drive_one_period_frames(state) else {
        return Vec::new();
    };
    crate::analysis::resource::grown_generic_counter_targets(&before, &after)
}

/// CR 122.1 + CR 732.2a: re-derive the per-object BENEFICIAL counter growth (with per-cycle
/// δ) of the accepted period by driving ONE iteration on a clone (`drive_one_period_frames`)
/// and diffing beneficial-materializable counters (`grown_beneficial_counter_deltas`). The
/// batched-collapse δ source for the whole beneficial class (+1/+1 / loyalty / defense /
/// charge) — the widened analog of `current_period_counter_targets` (DISPLAY, Generic-only).
/// Empty when the sequence is empty or the period grows no beneficial counter (a mana / token
/// / life loop). Only reached in the UNOBSERVED batched route (the firewall gates it).
fn current_period_counter_growth(
    state: &GameState,
) -> Vec<crate::types::game_state::CounterGrowth> {
    let Some((before, after)) = drive_one_period_frames(state) else {
        return Vec::new();
    };
    crate::analysis::resource::grown_beneficial_counter_deltas(&before, &after)
        .into_iter()
        .map(
            |(object, counter, per_cycle_delta)| crate::types::game_state::CounterGrowth {
                object,
                counter,
                per_cycle_delta,
            },
        )
        .collect()
}

/// CR 119.3 + CR 732.2a: re-derive the per-player life GAIN δ of the accepted period by
/// driving ONE iteration on a clone and diffing life totals (`grown_life_deltas`). The
/// batched-collapse δ source for the life axis. Empty when the sequence is empty or the
/// period gains no life. Only reached in the UNOBSERVED batched route (the firewall gates it).
fn current_period_life_growth(state: &GameState) -> Vec<(PlayerId, u32)> {
    let Some((before, after)) = drive_one_period_frames(state) else {
        return Vec::new();
    };
    crate::analysis::resource::grown_life_deltas(&before, &after)
}

/// CR 732.2a: detect an object-growth recast loop by driving TWO iterations on a clone;
/// on success returns the offer certificate for the CALLER to install. Takes a SHARED
/// `&GameState` ⇒ a live write is TYPE-IMPOSSIBLE (INV-1); the sole live write
/// (`waiting_for = LoopShortcut`) is done by the mutable-borrow caller (INV-2: OFFER,
/// never auto-resolve, CR 732.2a). Both driven iterations run inside ONE
/// `SimulationProbeGuard` so the injector's internal `apply_action` never recurses into
/// this hook or any `!in_simulation_probe()`-gated shortcut logic.
fn try_offer_object_growth_shortcut(
    state: &GameState,
) -> Option<(
    crate::analysis::loop_check::LoopCertificate,
    crate::analysis::decision_template::ShortcutDecisionSchema,
)> {
    let seq = state.last_loop_action_sequence.clone();
    if seq.is_empty() {
        return None;
    }
    let WaitingFor::Priority { player: caster } = state.waiting_for else {
        return None;
    };
    // The whole PERIOD must belong to the priority holder. A multi-controller / interleaved
    // sequence is fail-closed here; the per-step drive's controller re-find is the runtime
    // backstop (T-HET). Faithful generalization of the pre-P7 `ctx.controller != caster` check.
    if seq.iter().any(|c| c.controller != caster) {
        return None;
    }
    // STEP D (CR 104.4b / CR 601.2a / CR 602.2 / CR 605.3a): only OFFER a VOLUNTARILY-repeatable
    // (optional) loop — every driving step must be a player-initiated cast/activation. Replaces
    // the pre-P7 `no_living_player_has_meaningful_priority_action` offer gate (HAZARD A: that
    // predicate + its leaf `is_meaningful_priority_activation` (mana_sources.rs) stay byte-identical
    // for the MANDATORY `:431`/`:515` lethal/draw paths). A mana engine's activations are voluntary
    // (CR 605.3a) so it offers; a future mandatory driving variant is forced to return `false`.
    if !seq.iter().all(|c| c.action.is_voluntarily_repeatable()) {
        return None;
    }
    // CR 602.2a / CR 732.2a (G4): the per-step ability def each `Activate` step names, so the drive
    // can re-validate its positional `ability_index` by `Eq` each iteration; `None` for `Recast`.
    let expected_defs: Vec<Option<crate::types::ability::AbilityDefinition>> = seq
        .iter()
        .map(|c| loop_action_expected_def(state, c))
        .collect();
    // CR 732.2a: a shortcut "can't include conditional actions, where the outcome of a game
    // event determines the next action." A driving ability whose body bears an auto-resolved
    // coin flip (CR 705.1) / die roll (CR 706.1a) / random selection (CR 701.9a/b) has more
    // than one equally-likely outcome ⇒ not a legal shortcut. Reject it STATICALLY, before
    // driving (cheap + compile-time exhaustive over `Effect`), scanning EVERY step of the period
    // (exhaustive): a `Recast` re-finds its card in the castable origin zone (which ALSO proves
    // recastability) and scans the combined spell ability; an `Activate` pins the driving
    // permanent by `ObjectId` (G3) and scans the activated ability's own def. Fail-closed: an
    // undeterminable ability (no combined Spell def, or a missing source/index) does not offer.
    // (A2 determinism gate — the static half; the post-drive rng-position check below is the
    // complete runtime backstop that additionally catches external triggered/replacement
    // randomness firing in the cycle.)
    for (c, expected_def) in seq.iter().zip(expected_defs.iter()) {
        let bears_randomness = match &c.action {
            crate::types::game_state::LoopAction::Recast { from_zone, .. } => {
                let recast_obj = state.objects.values().find(|o| {
                    o.card_id == c.card_id && o.zone == *from_zone && o.controller == c.controller
                })?;
                let spell_def = crate::game::casting::combined_spell_ability_def(recast_obj)?;
                crate::game::ability_scan::spell_ability_bears_randomness(&spell_def)
            }
            crate::types::game_state::LoopAction::Activate { .. } => {
                crate::game::ability_scan::spell_ability_bears_randomness(expected_def.as_ref()?)
            }
            crate::types::game_state::LoopAction::TapLandForMana { selection } => {
                match selection.ability_index {
                    Some(_) => crate::game::ability_scan::spell_ability_bears_randomness(
                        expected_def.as_ref()?,
                    ),
                    None => false,
                }
            }
        };
        if bears_randomness {
            return None;
        }
    }

    // Drive two whole PERIODS (three settle frames) under the re-entrancy guard.
    let _probe = SimulationProbeGuard::enter();
    let s_n = state.clone();
    let mut clone = state.clone();
    drive_loop_sequence_iteration(&mut clone, &seq, 0, &expected_defs).ok()?;
    let s_n1 = clone.clone();
    drive_loop_sequence_iteration(&mut clone, &seq, 1, &expected_defs).ok()?;
    let s_n2 = clone;

    // CR 732.2a: any randomness CONSUMED during the deterministic detection drive means the
    // real loop is outcome-dependent (a coin flip CR 705.1 / die roll CR 706.1a / random
    // selection CR 701.9b / shuffle) and is not a predictable shortcut. The seeded ChaCha20
    // stream position advances iff randomness was drawn; the driven clone started as
    // `state.clone()` (an equal baseline), so a word-position delta disqualifies the offer.
    // This is the RUNTIME backstop to the static scan above: the fodder-cover's
    // `fire_time_conditions_read_growing_class` already rejects a randomness-bearing *permanent*
    // ability whose effect classifies `Axes::CONSERVATIVE` (`FlipCoin`/`RollDie`; a few
    // dice-adjacent effects like `RollToVisitAttractions` classify `Axes::NONE` and slip the
    // cover — this check catches those too), but it does NOT scan the resolving
    // recast *spell's* own body — so a coin flip in the recast body advances the RNG yet passes
    // the cover. This check closes that gap even when the static scan's `collect_effects` walk
    // misses a nested payload. Fail-closed / strictly-more-conservative (only turns OFFERs into
    // NO-OFFERs). (A2 determinism gate — discharges the b132ad9f8 "fail-closed-modulo-auto-
    // randomness" carry.)
    if s_n2.rng.get_word_pos() != state.rng.get_word_pos() {
        return None;
    }

    // CR 400.7: normalize each frame (strip the self-returning recast card + clear churning
    // token-id bookkeeping) BEFORE the cover fork so both arms share the normalized frames. Uses
    // `seq[0]`'s action to dispatch the recast-strip — an all-`Activate` period (the mana-engine
    // class) only clears token-id bookkeeping; a 1-element `Recast` strips its card as before.
    let (cs_n, cs_n1, cs_n2) = (
        normalize_recast_frame(&s_n, &seq[0]),
        normalize_recast_frame(&s_n1, &seq[0]),
        normalize_recast_frame(&s_n2, &seq[0]),
    );
    // CR 732.2a board recurrence on BOTH pairs — two disjoint recurrence shapes:
    //  - fodder-growth (a token was reproduced each period, `derived_fodder_class` is `Some`):
    //    cover modulo the inert reproduced fodder class (the P3 object-growth path, unchanged).
    //  - pure resource growth (NO new battlefield object — the multi-activation mana-engine class):
    //    the board returns EQUAL modulo projected resources (mana grows +N/period, board identical).
    //    PROBE-1 measured `loop_states_equal_modulo_resources` TRUE on real Basalt+Power sequence
    //    boundaries. A PARTIAL period never reaches here board-equal (the drive re-taps a tapped
    //    source and aborts first), so the drive+cover IS the period-boundary check.
    let cover_ok = match derived_fodder_class(&s_n, &s_n1) {
        Some(mut fodder) => {
            crate::analysis::resource::project_object_for_loop(&mut fodder);
            crate::analysis::resource::loop_states_cover_modulo_fodder_growth(
                &cs_n, &cs_n1, &fodder,
            ) && crate::analysis::resource::loop_states_cover_modulo_fodder_growth(
                &cs_n1, &cs_n2, &fodder,
            )
        }
        None => {
            // FIX-2 (CR 732.2a / CR 104.4b): the multi-activation / pure-counter class returns
            // EQUAL modulo projected resources OR covers modulo preserved-`Generic` counter growth
            // (Pentad charge, One Ring burden — the whole preserved-`Generic` family, not one
            // card). The base `loop_states_equal_modulo_resources` PRESERVES `Generic` counters, so
            // a +1-charge/cycle loop is UNEQUAL there; the counter-growth cover accepts it. Sound:
            // the offer is declinable and never crowns a `GameOver` (the cover's own doc,
            // `resource.rs`), and is deliberately NOT wired into any Path-A/Path-B lethal seam.
            let cover = |a: &GameState, b: &GameState| {
                crate::analysis::resource::loop_states_equal_modulo_resources(a, b)
                    || crate::analysis::resource::loop_states_cover_modulo_counter_growth(a, b)
            };
            cover(&cs_n, &cs_n1) && cover(&cs_n1, &cs_n2)
        }
    };
    if !cover_ok {
        return None;
    }

    // CR 119 / CR 122.1 / CR 704.5g sign-check on the second pair (RAW un-projected frames):
    // net progress for the caster, no loss axis for anyone, every driving consumable
    // non-decreasing (energy / poison / player-counters / object-counters) and no
    // damage_marked increase.
    let mut delta = crate::analysis::resource::ResourceVector::delta(
        &crate::analysis::resource::ResourceVector::snapshot(&s_n1),
        &crate::analysis::resource::ResourceVector::snapshot(&s_n2),
    );
    // CR 111.10: `tokens_created` is an EVENT-fed axis (0 under a snapshot diff), but the
    // cover above already proved the battlefield grows ONLY by inert reproduced tokens, so
    // the battlefield growth IS the per-cycle tokens-created count — the unbounded axis. Feed
    // it so `net_progress_for` sees the progress and the certificate names TokensCreated.
    let board_growth = s_n2.battlefield.len() as i64 - s_n1.battlefield.len() as i64;
    if board_growth > 0 {
        delta.tokens_created += board_growth;
    }
    if !delta.net_progress_for(caster)
        || !has_no_loss_axis(&delta)
        || !crate::analysis::resource::driving_resources_non_decreasing(&s_n1, &s_n2, caster)
    {
        return None;
    }

    // (The CR 104.4b optionality gate moved ABOVE the drive as STEP D's
    // `seq.iter().all(is_voluntarily_repeatable)` — HAZARD A: it no longer routes through
    // `no_living_player_has_meaningful_priority_action`, which stays scoped to the mandatory
    // `:431`/`:515` lethal/draw paths.)
    let certificate = build_cert(&s_n1, &s_n2, &delta, caster);
    // CR 732.2a (CARRY, don't re-derive): the schema's decision list is the SAME
    // `build_recast_template` output the drive uses — `[ConvokeTaps]` when `seq[0]` is a convoke
    // recast, else `[]` (a multi-activation period carries no convoke pin). Legal sets are derived
    // against the live offer-time board.
    let schema_template = build_recast_template(&seq[0]);
    // CR 732.2a: an UNBOUNDED object-growth offer is not repeated a CR 704-limited number of
    // times — it is materialized once as an unbounded axis — so it states no narrowed count
    // bound and keeps the global safety limit.
    let schema = build_shortcut_schema(
        // CR 732.2a: an unresolvable pin WITHDRAWS the offer rather than publishing an
        // undeclarable point — see `pinned_decisions_to_points`.
        pinned_decisions_to_points(&schema_template.decisions, state, caster)?,
        shortcut_iteration_count(certificate.win_kind),
        MAX_SHORTCUT_CYCLES,
    );
    Some((certificate, schema))
}

/// PR-7 Phase 4d-ii / P7 v3 (CR 732.2a): "materialize" a confirmed UNBOUNDED object-growth
/// shortcut (fodder/token reproduction, or a multi-activation mana engine). An unbounded loop is
/// NOT replayed a discrete number of times — that would both CAP the infinite at N and be O(N)
/// (measured ≈0.4 s per materialized token; 500 Saprolings drove for 212 s). Instead persist the
/// certificate's unbounded axes for the controller through the SAME single writer the reconcile /
/// determinate crown uses (`mark_unbounded_loop`; see the reconcile seam above). The ω-cover has
/// already proved the growing class is inert + unobserved, and `board_covers_modulo_fodder`'s
/// tapped-split proved the UNTAPPED remainder is B1-preserved (finite) while the total strictly
/// grows — so the TAPPED members are exactly the unbounded pile. The board therefore needs NO
/// mutation: the finite untapped reals stay as-is, and the pre-existing tapped fodder ARE the ∞
/// pile (the HUD / battlefield render the marked axis as `∞`). For a mana engine the axes are
/// `Mana(_)`, feeding the existing infinite-mana pool reseed. Every OFFERED growth loop is
/// certified-unbounded, so `proposal.unbounded` is non-empty (an empty set is a harmless no-op).
/// Then consume the recast context + hand priority to the living seat (CR 800.4a) — exactly as the
/// old drive did — so this same `apply()` does not instantly re-offer; a later manual recast
/// re-arms the context and a later beat re-detects genuinely.
fn materialize_object_growth_shortcut(
    state: &mut GameState,
    result: &mut ActionResult,
    proposal: &crate::analysis::loop_check::ShortcutProposal,
) {
    // CR 732.2a: reuse the single `unbounded_resources` writer (never mutate the map inline). The
    // proposer is the loop controller (the offer required the whole period to be theirs).
    state.mark_unbounded_loop(proposal.proposer, &proposal.unbounded);
    // CR 732.2a / CR 110.1: snapshot the ∞ pile — the proposer's tapped fodder-class members —
    // for `DerivedViews::unbounded_pile`. Re-derive the fodder class HERE (the sequence is still
    // intact; the `.clear()` below wipes it) by driving one period on a clone. A mana-engine loop
    // reproduces no token ⇒ `current_period_fodder` is `None` ⇒ no pile (correct).
    // DISPLAY (hoisted, unconditional — runs for BOTH the observed and unobserved routes so an
    // observed token+X loop keeps its on-battlefield ∞ pile accept→boundary): seed the pile's
    // anchors and register it, capturing the token copiable profile for the batched Tokens stash.
    let token_profile: Option<crate::types::ability::CopiableValues> =
        if let Some(period) = current_period_fodder(state) {
            let class = &period.class;
            // CR 732.2a / CR 707.2: capture the fodder's copiable profile NOW, while the recast
            // sequence is still intact (`.clear()` below wipes it and `current_period_fodder`
            // derives from it). At the next phase/step boundary the loop controller names a finite
            // N and N tapped copy-tokens are minted from this profile (the deferred shortcut
            // count). Stored as CopiableValues, NOT an ObjectId: the board is not frozen
            // accept→boundary, and a token's oracle_id is empty so a ResidualPermanent could not
            // recreate it. A mana-engine loop has no fodder class (`None`) → no token stash.
            let profile = crate::game::printed_cards::intrinsic_copiable_values(class);
            // CR 702.51a (convoke optional) + CR 732.2a: seed the ∞ pile's tapped anchor AND the
            // W+1 untapped remainder ONLY when the certified period actually TAPS a fodder each
            // cycle (`period.taps_fodder`) AND the live board has no tapped fodder yet (a one-shot
            // bootstrap tapped a creature OUTSIDE the fodder class, e.g. convoking the {B}{G}
            // cost-reducer for {G}). `board_covers_modulo_fodder`'s `>=` untapped cover
            // (resource.rs) admits pure untapped-partition growth, so a mana-paid untapped-growth
            // loop also reaches here with an empty tapped-fodder set — `is_empty()` alone
            // over-fires; `period.taps_fodder == false` there → no spurious seed. The untapped
            // seed is CR 702.51a's optional-convoke final cast (pay {G} from mana, make a
            // Saproling without tapping one → +1 untapped); it is excluded from the ∞ pile because
            // `tapped_fodder_members` filters `o.tapped`.
            if period.taps_fodder
                && crate::analysis::resource::tapped_fodder_members(state, proposal.proposer, class)
                    .is_empty()
            {
                seed_representative_fodder(
                    state,
                    result,
                    proposal.proposer,
                    &profile,
                    /*tapped=*/ true,
                );
                seed_representative_fodder(
                    state,
                    result,
                    proposal.proposer,
                    &profile,
                    /*tapped=*/ false,
                );
            }
            // Re-read AFTER the mint so the pile names the freshly-seeded tapped anchor (if any);
            // `register_unbounded_loop_pile` is a no-op on the still-empty set for the untapped
            // (non-seeded) case, preserving pre-existing untapped-growth behavior. The untapped
            // remainder seed is EXCLUDED here (`tapped_fodder_members` filters `o.tapped`).
            let pile =
                crate::analysis::resource::tapped_fodder_members(state, proposal.proposer, class);
            state.register_unbounded_loop_pile(proposal.proposer, pile);
            Some(profile)
        } else {
            None
        };
    // CR 732.2a / CR 701.34a: snapshot the per-object ∞ COUNTER targets for DISPLAY
    // (DerivedViews::unbounded_counters). Distinct from the object-growth ∞ pile above: a
    // counter-growth loop's certified unbounded axis is object-agnostic (Counter(Other,
    // Other)), so re-derive the concrete (object, counter) pairs by driving one period on a
    // clone and diffing Generic counters — WHILE the recast sequence is still intact (the
    // `.clear()` below wipes it). DISPLAY-ONLY: the object's real counter count is NOT mutated
    // (CR 701.34a already added the real counter on each live cycle; this only marks the pill
    // to render ∞). A mana / token / object-growth loop grows no Generic counter ⇒ empty ⇒
    // no-op writer. Runs in BOTH routes (display is unconditional).
    let counter_targets = current_period_counter_targets(state);
    state.register_unbounded_counter_targets(proposal.proposer, counter_targets);
    // ROUTE the STASH element only (the DISPLAY above is unconditional). `proposal.unbounded` IS
    // the ∞-mark set `mark_unbounded_loop` wrote. Capture-before-clear: `last_loop_action_sequence`
    // and the δ derivations all read BEFORE the `.clear()` tail below.
    //
    // AXIS-AWARE routing: a loop that grows a batchable COUNTER or LIFE axis OBSERVED by the current
    // board must DRIVE the whole loop (the batched δ apply would miscount the observer — a lump
    // life gain fires a "whenever you gain life" trigger once not N×, and `apply_counter_addition`
    // bypasses the counter doubler pipeline). Everything else BATCHES. A pure token/mana loop grows
    // no counter/life axis (`growths`/`life` empty) → its only observer surface is token creation,
    // already vetted by the OFFER-time fodder firewall → it always batches even when the board
    // carries an unrelated life/counter observer (plan §5 Note; the observedness firewall is
    // AXIS-SPECIFIC so an incidental board observer never mis-routes a disjoint-axis loop).
    let growths = current_period_counter_growth(state);
    let life = current_period_life_growth(state);
    let counter_observed =
        !growths.is_empty() && crate::analysis::resource::counter_growth_is_observed(state);
    let life_observed =
        !life.is_empty() && crate::analysis::resource::life_growth_is_observed(state);
    // CR 732.2a + CR 603.6a: a life axis the board RE-EARNS on a battlefield entry also belongs on
    // the concrete replay. Not an observedness question (the batched arithmetic is right) but a
    // ROUTE one: the batched `Tokens` collapse mints N real tokens whose real CR 603.6a entries
    // re-earn the same life the batched `Life` already applied, so the accept pays twice.
    //
    // The conjuncts are AXIS-shaped, never effect-shaped: a life axis grew (`!life.is_empty()`),
    // the collapse will mint the tokens that re-earn it (`token_profile.is_some()` — a mana-only
    // collapse mints nothing, so nothing re-fires), and the board has an entry trigger at all.
    // Testing the trigger's EFFECT for `GainLife` would be under-approximate: life reaches
    // `apply_life_gain` from four resolvers, including CR 702.15b lifelink on an ETB damage
    // trigger (the Terror of the Peaks shape), which no effect-shape test can see.
    let life_etb_sourced = !life.is_empty()
        && token_profile.is_some()
        && crate::analysis::resource::board_has_functioning_etb_trigger(state);
    // M5: hoisted out of the branch so an empty period can never register NOTHING — a route
    // flipped to the replay falls back to the batched arm instead of silently dropping the whole
    // materialization. (Unreachable today: `growths`/`life` are derived from the same
    // `drive_one_period_frames`, which returns `None` on an empty sequence, so every route
    // predicate is already false there. Kept explicit so a future route conjunct cannot
    // reintroduce the hole.)
    let sequence = state.last_loop_action_sequence.clone();
    if (counter_observed || life_observed || life_etb_sourced) && !sequence.is_empty() {
        // CR 732.2a: OBSERVED batchable growth — one DriveSequence collapses the WHOLE loop (all
        // axes); replaying the captured sequence recreates every per-cycle effect honoring
        // observers. Do NOT also register batched items (the routes are exclusive per accept).
        state.register_pending_materialization(
            proposal.proposer,
            crate::types::game_state::PersistentAxisMaterialization::DriveSequence {
                sequence,
                collapsed_axes: proposal.unbounded.clone(),
            },
        );
    } else {
        // UNOBSERVED fast path — register each grown persistent axis for the batched N×δ collapse.
        if let Some(profile) = token_profile {
            state.register_pending_materialization(
                proposal.proposer,
                crate::types::game_state::PersistentAxisMaterialization::Tokens(Box::new(profile)),
            );
        }
        if !growths.is_empty() {
            state.register_pending_materialization(
                proposal.proposer,
                crate::types::game_state::PersistentAxisMaterialization::Counters(growths),
            );
        }
        for (player, per_cycle_delta) in life {
            state.register_pending_materialization(
                proposal.proposer,
                crate::types::game_state::PersistentAxisMaterialization::Life {
                    player,
                    per_cycle_delta,
                },
            );
        }
    }
    state.loop_detect_ring.clear();
    state.last_loop_action_sequence.clear();
    priority::reset_priority(state);
    state.waiting_for = WaitingFor::Priority {
        player: living_priority_seat(state),
    };
    result.waiting_for = state.waiting_for.clone();
}

/// CR 732.2a: replay a captured loop-action period `n` times through real `apply()` at the CR
/// 500.5 step/phase boundary, committing each period atomically — observers (Heliod / Corpsejack)
/// fire each cycle, so an OBSERVED loop's N-cycle result is exact where a single batched N×δ would
/// be wrong. The simulation guard is HELD across the whole drive so the injector's internal
/// `apply_action` never recurses into the shortcut offer/detection hooks (`in_simulation_probe`
/// gates those only). Aborts to the successful prefix if the loop can no longer replay — the
/// machinery left the board between accept and boundary (CR 800.4a / CR 400.7) — committing the
/// cycles that did replay. `n` is pre-clamped `[0, MAX_SHORTCUT_CYCLES]` at the prompt. This is the
/// re-introduction of the removed accept-time drive (commit 6d9344af1), bounded to observed loops
/// at the boundary; the private `drive_loop_sequence_iteration` / `loop_action_expected_def` /
/// `RecastAbort` cannot be named from `engine_resolution_choices`, so the drive lives here.
pub(crate) fn drive_persistent_axis_collapse(
    state: &mut GameState,
    seq: &[crate::types::game_state::LoopActionContext],
    n: u32,
) {
    let Some(controller) = seq.first().map(|c| c.controller) else {
        return;
    };
    // Derive `expected_defs` ONCE from the base (reloaded) boundary state — each `Activate` step's
    // named ability def for `Eq` re-validation; `Recast` re-finds its card + combined spell def live.
    let expected_defs: Vec<Option<crate::types::ability::AbilityDefinition>> = seq
        .iter()
        .map(|c| loop_action_expected_def(state, c))
        .collect();
    let _guard = SimulationProbeGuard::enter(); // held across the whole drive
    let mut committed = state.clone();
    for i in 0..n {
        let mut work = committed.clone();
        // The accept beat cleared the sequence and handed priority to the living seat; re-seed a
        // Priority window for the loop CONTROLLER (not `active_player`: `reset_priority` grants the
        // active player, but the loop may be an instant-speed period on an opponent's turn).
        priority::reset_priority(&mut work);
        work.priority_player = controller;
        work.waiting_for = WaitingFor::Priority { player: controller };
        if drive_loop_sequence_iteration(&mut work, seq, i, &expected_defs).is_err() {
            break; // commit the successful prefix (CR 800.4a hands priority back)
        }
        committed = work;
    }
    *state = committed;
    // `_guard` drops HERE — before the caller re-drains — so the restored beat is offer-eligible.
}

/// CR 732.2a / CR 111.1 / CR 110.5b / CR 707.2: when an accepted convoke/tap-cost object-growth
/// loop was DEMONSTRATED by tapping a creature OUTSIDE the reproduced fodder class (e.g. convoking
/// the {B}{G} cost-reducer for {G}), no tapped fodder member exists on the live board yet. Mint
/// ONE representative fodder token from the sustainable period's captured copiable profile — the
/// SAME copy-token mint the boundary collapse uses (single token authority) — so Part-1's
/// `unbounded_loop_pile`/`∞` badge has a live anchor (`tapped: true`) and CR 702.51a's mana-paid
/// capping cast's untapped remainder is realized (`tapped: false`). CR 111.1: the mint creates a
/// token. CR 110.5b: a permanent enters untapped unless told otherwise — `tapped` names that
/// status directly. CR 707.2: copiable values carry name/P-T/color/abilities but NOT tapped
/// status, so `CopyTokenSpec.tapped` sets it explicitly. The untapped working set is untouched (a
/// new token is added; no existing fodder is tapped), so the finite remainder is preserved.
fn seed_representative_fodder(
    state: &mut GameState,
    result: &mut ActionResult,
    owner: PlayerId,
    profile: &crate::types::ability::CopiableValues,
    tapped: bool,
) {
    let batch = crate::types::game_state::PendingCopyTokenBatch {
        owner,
        count: 1,
        copy: Box::new(crate::types::proposed_event::CopyTokenSpec {
            values: Box::new(profile.clone()),
            display_source: crate::game::game_object::DisplaySource::Token,
            printed_ref: None,
            token_image_ref: None,
            extra_keywords: vec![],
            additional_modifications: vec![],
            tapped,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(0),
            controller: owner,
        }),
    };
    crate::game::effects::token_copy::drive_copy_token_batches(
        state,
        VecDeque::from([batch]),
        EffectKind::CopyTokenOf,
        ObjectId(0),
        &mut result.events,
    );
}

/// Immutable data from a `WaitingFor::LoopShortcut` offer, grouped for declaration handling.
struct LoopShortcutOffer<'a> {
    proposer: PlayerId,
    predicted_winner: Option<PlayerId>,
    certificate: &'a crate::analysis::loop_check::LoopCertificate,
    schema: &'a crate::analysis::decision_template::ShortcutDecisionSchema,
}

/// CR 732.2a (MagicCompRules.txt:6372) + CR 800.4a (MagicCompRules.txt:6408): reject a
/// shortcut declaration and hand priority back to the next living seat — the manual-play
/// handback every reject path in `handle_declare_shortcut` lands on. Single
/// authority: a sixth reject path added later cannot forget to sync
/// `result.waiting_for`.
fn reject_shortcut_declaration(state: &mut GameState, result: &mut ActionResult) {
    priority::reset_priority(state);
    state.waiting_for = WaitingFor::Priority {
        player: living_priority_seat(state),
    };
    result.waiting_for = state.waiting_for.clone();
}

/// CR 732.2a: the proposer declared the loop shortcut. Build the public proposal and open
/// the APNAP accept-or-shorten window over the proposer's living opponents (turn order). No
/// opponents (solitaire / all eliminated) ⇒ take the shortcut immediately.
fn handle_declare_shortcut(
    state: &mut GameState,
    offer: LoopShortcutOffer<'_>,
    count: crate::analysis::decision_template::IterationCount,
    template: Option<crate::analysis::decision_template::DecisionTemplate>,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    let mut result = ActionResult {
        events: std::mem::take(events),
        waiting_for: state.waiting_for.clone(),
        log_entries: vec![],
    };
    // CR 732.2a fail-closed firewall: validate the declared pins against the offered schema
    // BEFORE `template` is moved into `proposal` and BEFORE APNAP opens. Coverage
    // (`predictability_gate`) and value-legality (`validate_pins`) both consult the SAME
    // single authority — the schema's exposed slots — so a rejection lands cleanly at a
    // manual-play handback (Priority to the living seat, no APNAP, no drive, no crown). The
    // offer window closes; a later beat re-detects the loop if it still closes. Validating
    // once at declare suffices: the board is frozen through Accept (apply_confirmed_shortcut
    // doc), and the drive's per-iteration `resolve` (CR 608.2b) is the runtime backstop.
    //
    // A CHOICE-FREE offer (empty schema — a non-targeted drain) exposes no decisions to
    // validate: its win derivation is pin-independent (the E1 measure is the authority), and
    // any template the caller supplies is inert for the drive (the loop raises no target
    // prompt). This preserves the established `Fixed(N)` drain behavior (the resolve-firewall
    // materialize tests drive a synthetic pin against the empty drain schema).
    // ⚠ ORDER IS LOAD-BEARING: the count cap runs BEFORE the pin validation below, because
    // `shortcut_validated_range` derives the validated range FROM the declared count and so
    // must not be handed an unchecked one — a `Fixed(4_000_000_000)` would otherwise become
    // a four-billion-iteration validation loop. Observation-equivalence of the reorder is
    // structural: all six refusal arms across the three blocks (this match, the CR 732.2a +
    // CR 603.5 `template.owner` firewall between them, and the pin-validation block) land on
    // the same single authority (`reject_shortcut_declaration`), and
    // `handle_declare_shortcut` pushes NO events at all, so no row can observe which block
    // refused first.
    // IMPLEMENTATION BUDGET BOUND (see MAX_SHORTCUT_CYCLES) — deliberately NOT labelled as a
    // CR 732.2a constraint: the rules place no ceiling on how many times a shortcut may be
    // repeated (CR 732.2a's own example runs to a million), so this ceiling is ours, not the
    // game's. The label matters because a maintainer applying the CR 732.2a iff to a branch
    // that wears a CR number will either trust it wrongly or delete it wrongly. Reject an
    // over-cap Fixed count at
    // the single authority — BEFORE the proposal is built — into the same fail-closed
    // manual-play handback the pin validation above uses. This is THE catastrophic remote
    // vector: `Fixed(u32)` scalar-encodes up to ~4.3e9 cycles in ~10 bytes, sailing through
    // the 8 KB WS frame cap → one GameState clone + drive per cycle. Both confirmation paths
    // (solitaire-immediate below, APNAP Accept) consume this one proposal, and both drive
    // helpers (materialize_fixed_shortcut / materialize_object_growth_shortcut) read `n` from
    // it, so this one check bounds every Fixed drive on every transport. The drive helpers do
    // NOT re-check.
    // Exhaustive (no wildcard) so a future `IterationCount` variant — e.g. the reserved
    // `UntilResource`, which would carry its OWN unbounded count — build-breaks HERE and
    // forces a bound decision rather than silently regressing this cap.
    match &count {
        crate::analysis::decision_template::IterationCount::Fixed(n)
            if *n > MAX_SHORTCUT_CYCLES =>
        {
            reject_shortcut_declaration(state, &mut result);
            return Ok(result);
        }
        // CR 732.2a: the per-offer CR 704 bound, enforced at the same single authority as the
        // global cap. A `Fixed(n)` above `max_iterations` would contain a conditional action —
        // some living player crosses a CR 704.5a / CR 704.5c / CR 104.3c loss threshold inside
        // the proposal, and what happens next depends on that — so it is not a legal shortcut.
        crate::analysis::decision_template::IterationCount::Fixed(n)
            if *n > offer.schema.max_iterations =>
        {
            reject_shortcut_declaration(state, &mut result);
            return Ok(result);
        }
        // CR 732.2a: `UntilLethal` names no count at all, so it can only be legal when the
        // offer states no narrowed bound. An offer that DID narrow its bound is one whose
        // producer measured a CR 704 threshold inside the loop; running it "until lethal"
        // would run past that threshold.
        crate::analysis::decision_template::IterationCount::UntilLethal
            if offer.schema.is_bounded() =>
        {
            reject_shortcut_declaration(state, &mut result);
            return Ok(result);
        }
        // Under-cap `Fixed` and `UntilLethal` (period-bounded by `shortcut_drive_period`)
        // proceed to the proposal.
        crate::analysis::decision_template::IterationCount::Fixed(_)
        | crate::analysis::decision_template::IterationCount::UntilLethal => {}
    }
    // CR 732.2a + CR 603.5: the declared template's `owner` is CLIENT-SUPPLIED — the
    // `GameAction::DeclareShortcut { template }` payload arrives here verbatim — and it is
    // the comparand `inject_pinned_answer` uses to decide WHOSE CR 603.5 choice a pin may
    // answer. Bind it to the engine-issued seat here, at declare, or that seat guard
    // compares an attacker-chosen value against itself. `offer.proposer` is engine state,
    // copied from `WaitingFor::LoopShortcut { proposer }`.
    //
    // PLACEMENT IS LOAD-BEARING: this sits OUTSIDE the `!offer.schema.points.is_empty()`
    // block below, so it is reached for every declaration regardless of schema emptiness —
    // an empty-schema offer skips `predictability_gate` / `validate_pins` entirely and would
    // otherwise reach the proposal with an unvalidated owner. It is the SIXTH sibling of the
    // five refusal arms and lands on their single authority (`reject_shortcut_declaration`),
    // so no row can observe which refusal fired first — the "sixth reject path added later"
    // that authority's doc anticipates. Defence in depth for the RESTORE ingress (a persisted
    // `WaitingFor::RespondToShortcut` never runs this handler) lives on
    // `apply_confirmed_shortcut`'s consumption guard.
    if template.as_ref().is_some_and(|t| t.owner != offer.proposer) {
        reject_shortcut_declaration(state, &mut result);
        return Ok(result);
    }
    if !offer.schema.points.is_empty() {
        match &template {
            Some(t) => {
                let required: Vec<crate::analysis::decision_template::DecisionSlot> =
                    offer.schema.points.iter().map(|p| p.slot.clone()).collect();
                // CR 732.2a: validate over the range the ACCEPTED COUNT will drive, not
                // over the schedule's own period. `shortcut_drive_period` answers a
                // different question (how many cycles one measurement must aggregate), and
                // using it here both ACCEPTED a pin whose driven image leaves the published
                // set at an index the count reaches, and REFUSED conforming declarations
                // whose count is shorter than the schedule.
                let validated_range = shortcut_validated_range(&count, Some(t));
                if crate::analysis::decision_template::predictability_gate(t, &required).is_err()
                    || crate::analysis::decision_template::validate_pins(
                        offer.schema,
                        t,
                        validated_range,
                        state,
                    )
                    .is_err()
                {
                    reject_shortcut_declaration(state, &mut result);
                    return Ok(result);
                }
            }
            // CR 732.2a: a `template: None` declaration against a NON-EMPTY schema skips the
            // validation above entirely — the pins the offer published are never checked. That
            // is legitimate for exactly one drive shape: the object-growth route, which
            // re-derives its template from `state.last_loop_action_sequence` (the same routing
            // discriminant `materialize` dispatches on) and never reads `proposal.template`.
            // With an EMPTY sequence there is nothing to re-derive from, so a pin-consuming
            // drive would run with no pins at all — fail closed into the same manual-play
            // handback the validation failure above uses. Both conjuncts are required: keying
            // on `template.is_none()` alone breaks the shipped object-growth declarations.
            None if state.last_loop_action_sequence.is_empty() => {
                reject_shortcut_declaration(state, &mut result);
                return Ok(result);
            }
            None => {}
        }
    }
    let proposal = crate::analysis::loop_check::ShortcutProposal {
        proposer: offer.proposer,
        predicted_winner: offer.predicted_winner,
        count,
        unbounded: offer.certificate.unbounded.clone(),
        win_kind: offer.certificate.win_kind,
        template,
        // CR 732.2a: the drive reads ONE authority for what a conformant cycle looks like —
        // the confirmed certificate's own signature, copied, never re-derived.
        per_cycle: offer.certificate.per_cycle.clone(),
    };
    // CR 732.2b: living opponents in APNAP turn order, starting after the proposer.
    let opps: Vec<PlayerId> = crate::game::players::apnap_order_from(
        state,
        Some(crate::types::ability::ControllerRef::You),
        offer.proposer,
    )
    .into_iter()
    .filter(|&p| p != offer.proposer)
    .collect();
    if let Some((&first, rest)) = opps.split_first() {
        state.waiting_for = WaitingFor::RespondToShortcut {
            player: first,
            remaining_players: rest.to_vec(),
            proposal,
        };
        result.waiting_for = state.waiting_for.clone();
    } else {
        // CR 732.2c: nobody else to poll ⇒ take the shortcut.
        apply_confirmed_shortcut(state, &mut result, &proposal);
    }
    Ok(result)
}

/// CR 732.2a: the priority holder MAY decline the auto-offered loop shortcut — "the player
/// with priority may suggest a shortcut" makes suggesting optional, so forcing a proposal is
/// wrong. Restore ordinary priority (the living seat, mirroring the `handle_declare_shortcut`
/// pin-rejection handback) so the post-return reconcile hands the controller a normal window
/// instead of re-nagging the SAME offer. This is the `until_lethal_fallback` tail minus the
/// board rollback: decline is pre-drive, so no board mutation ever occurred.
///
/// Re-offer suppression, by seam:
/// - Interactive bridge (Seam 1, `find_live_loop_winner` reads `loop_detect_ring`, gated by
///   `!stack.is_empty()`): suppressed by the GENERAL deliberate-action invariant, not by this
///   handler. `apply_action` (engine.rs:3006-3011) invalidates `loop_detect_ring` for every
///   deliberate (non-`PassPriority`/`OrderTriggers`) action; `DeclineShortcut` is a deliberate
///   break, so the ring is already empty before this handler runs. Seam-1 suppression is the
///   shared invariant every cast/activate/play-land relies on — the handler does NOT re-clear
///   the ring (re-clearing would special-case `DeclineShortcut` to distrust an engine-wide
///   invariant). The interactive e2e's "no re-offer" assertion guards this end-to-end: a future
///   regression excluding `DeclineShortcut` from that allowlist would fail it loudly.
/// - Object-growth (Seam 2, gated by `!last_loop_action_sequence.is_empty()`): the deliberate-action
///   clear does NOT touch `last_loop_action_sequence`, so `state.last_loop_action_sequence.clear()` here
///   is the genuinely load-bearing suppressor — without it the post-return reconcile re-fires
///   `try_offer_object_growth_shortcut` within this same `apply()`.
///
/// A genuine re-recurrence or a fresh re-cast re-arms the offer naturally. Proposer-only
/// authorization is enforced upstream by `check_actor_authorization`
/// (`WaitingFor::acting_player` == `LoopShortcut.proposer`), so offer fields are unused here.
fn handle_decline_shortcut(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    let mut result = ActionResult {
        events: std::mem::take(events),
        waiting_for: state.waiting_for.clone(),
        log_entries: vec![],
    };
    // Seam 1 (loop_detect_ring) is already invalidated by apply_action's deliberate-action
    // ring-clear (engine.rs:3006-3011) — see doc. Only Seam 2 is the handler's gap:
    state.last_loop_action_sequence.clear(); // Seam 2: load-bearing object-growth offer-gate clear (CR 732.2a)
    priority::reset_priority(state);
    state.waiting_for = WaitingFor::Priority {
        player: living_priority_seat(state),
    };
    result.waiting_for = state.waiting_for.clone();
    Ok(result)
}

/// CR 732.2b/c: one opponent answered the shortcut offer. Mirrors the
/// `OpponentMayChoice`/`UnlessPayment` APNAP fan-out (drain-one-advance via
/// `remaining_players`). Accept advances to the next opponent, or — when the last accepts —
/// takes the shortcut. Shorten conservatively hands THAT opponent a real priority window
/// (CR 732.2c "a different choice"); the shortcut is NOT auto-applied, and a later beat
/// re-detects the loop (a fresh offer if it still closes, normal play if broken).
fn handle_respond_to_shortcut(
    state: &mut GameState,
    player: PlayerId,
    remaining_players: Vec<PlayerId>,
    proposal: crate::analysis::loop_check::ShortcutProposal,
    response: crate::analysis::loop_check::ShortcutResponse,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    let mut result = ActionResult {
        events: std::mem::take(events),
        waiting_for: state.waiting_for.clone(),
        log_entries: vec![],
    };
    match response {
        crate::analysis::loop_check::ShortcutResponse::Accept => {
            // CR 800.4a: never advance the offer onto a player who has left the game. A
            // queued opponent can concede AFTER the window opened (Concede bypasses the
            // `WaitingFor` dispatch, so `remaining_players` is never self-healed), so drop
            // any departed seats before advancing. CR 732.2b: the queue is already in APNAP
            // turn order, so the first surviving remainder is the next living opponent.
            let mut living = remaining_players
                .into_iter()
                .filter(|&p| crate::game::players::is_alive(state, p));
            if let Some(next) = living.next() {
                state.waiting_for = WaitingFor::RespondToShortcut {
                    player: next,
                    remaining_players: living.collect(),
                    proposal,
                };
                result.waiting_for = state.waiting_for.clone();
            } else {
                // CR 732.2c: the last living opponent accepted ⇒ take the shortcut
                // (F1 re-validates the proposer's own liveness before crowning).
                apply_confirmed_shortcut(state, &mut result, &proposal);
            }
        }
        crate::analysis::loop_check::ShortcutResponse::Shorten { .. } => {
            // CR 732.2c (Phase-3 conservative): hand this opponent a real priority window
            // instead of taking the shortcut. Finite-K materialization is Phase 4.
            priority::reset_priority(state);
            state.priority_player = player;
            state.waiting_for = WaitingFor::Priority { player };
            result.waiting_for = state.waiting_for.clone();
        }
    }
    Ok(result)
}

fn remember_public_reveals(state: &mut GameState, events: &[GameEvent], journal_start: usize) {
    // The journal is truncated at turn boundaries, so an action that
    // auto-advances across a turn leaves `journal_start` past the current end.
    // Clamp with `get(..)` so a truncated journal yields no this-action
    // controller reveals rather than panicking on an out-of-bounds slice.
    let controller_reveals = state
        .resolved_rules_journal
        .entries()
        .get(journal_start..)
        .unwrap_or(&[])
        .iter()
        .filter_map(|entry| entry.command.as_ref())
        .filter_map(|command| match command {
            ResolvedRulesCommand::Information(information)
                if matches!(
                    information.audience,
                    ResolvedInformationAudience::Controller(_)
                ) && matches!(information.edit, ResolvedInformationEdit::Reveal) =>
            {
                Some(&information.occurrences)
            }
            _ => None,
        })
        .flatten()
        .map(|occurrence| occurrence.object_id)
        .collect::<HashSet<_>>();

    for event in events {
        if let GameEvent::CardsRevealed { card_ids, .. } = event {
            let unpublished = card_ids
                .iter()
                .copied()
                .filter(|card_id| !controller_reveals.contains(card_id))
                .collect::<Vec<_>>();
            state
                .resolve_and_apply_information(
                    &unpublished,
                    ResolvedInformationAudience::Public,
                    ResolvedInformationLifetime::UntilZoneChange,
                    ResolvedInformationEdit::Reveal,
                )
                .expect("published reveal occurrences must be live and distinct");
        }
    }
}

/// Engine-level authorization guard. Any *game action* must come from the
/// `authorized_submitter` for the current `WaitingFor` (which already accounts
/// for turn-decision-controller effects like Mindslaver). Two exception classes:
///
/// - `Concede` self-authenticates via its own `player_id` field — but we still
///   require it to match `actor` so a player cannot concede someone else on
///   their behalf (CR 104.3a). It is no longer an action after the game has
///   ended, when there is no authorized submitter.
/// - **Preference actions** (SetPhaseStops, SetPriorityPassingMode,
///   CancelAutoPass, ReorderHand) are per-player UI
///   settings. They have no CR semantics, mutate only the submitter's own
///   preference slot, and may legitimately fire at any time — e.g. the human
///   toggles a phase stop while the AI holds priority. The downstream handlers
///   route by `actor`, so any seat may set its own preferences regardless of
///   `WaitingFor`. `SetAutoPass` is deliberately NOT exempt: its handler
///   stores the mode for the `WaitingFor::Priority` player and immediately
///   passes that priority, so it must come from the authorized submitter.
fn check_actor_authorization(
    state: &GameState,
    actor: PlayerId,
    action: &GameAction,
) -> Result<(), EngineError> {
    if action.is_actor_scoped_preference()
        || matches!(
            action,
            GameAction::Debug(_)
                | GameAction::GrantDebugPermission { .. }
                | GameAction::RevokeDebugPermission { .. }
        )
    {
        return Ok(());
    }
    // CR 103.5: For simultaneous-decision states (MulliganDecision,
    // OpeningHandBottomCards), authorize against the full pending set so any
    // pending player may submit in any order. Falls back to single-player
    // semantics for every other variant.
    let authorized = turn_control::authorized_submitters(state);
    if authorized.is_empty() {
        return Err(EngineError::WrongPlayer);
    }
    if let GameAction::Concede { player_id } = action {
        // CR 104.3a: A player may concede at any time in an unfinished game —
        // but only themselves. `GameOver` has no authorized submitter, so it
        // cannot admit a second concession.
        if *player_id != actor {
            return Err(EngineError::WrongPlayer);
        }
        return Ok(());
    }
    if !authorized.contains(&actor) {
        return Err(EngineError::WrongPlayer);
    }
    Ok(())
}

/// Engine-internal convenience: apply `action` as the player the engine is
/// currently waiting on. Intended for simulation (AI search, legal-action
/// probing) and tests — *not* for transport adapters, which must pass a
/// transport-authenticated `actor` to [`apply`] directly.
///
/// For [`GameAction::Concede`] the concede payload's `player_id` is used as
/// the actor, so tests can concede any player without first maneuvering the
/// `WaitingFor` state onto that player.
pub fn apply_as_current(
    state: &mut GameState,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    apply_as_current_with_mode(state, action, PublicFinalizeMode::Immediate)
}

/// Simulation-apply variant of [`apply_as_current`] for throwaway clones that
/// are never rendered: either the caller discards the mutated state (the AI
/// `SimulationFilter` legality oracle reads only `.is_ok()`) or it keeps the
/// state solely to read *game-logic* fields for evaluation (the AI search
/// rollout/expansion). `finalize_rules_state` still runs, so the result is
/// rules-correct; only `finalize_display_state` — the board-global
/// `derive_display_state` sweep computing frontend-only hints (mana
/// availability `has_mana_ability`/`available_mana_pips`, devotion,
/// summoning-sickness display) that no rules, enumeration, or AI-evaluation
/// path consults — is skipped. On a large board this removes an
/// O(battlefield) mana sweep from every legality probe AND every AI search
/// node expansion; that per-node sweep, compounded across the un-timed
/// `resolveAll` batch loop, was the AI-vs-AI "won't advance" wedge (#4798).
pub fn apply_as_current_for_simulation(
    state: &mut GameState,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    apply_as_current_with_mode(state, action, PublicFinalizeMode::DeferredDisplay)
}

fn apply_as_current_with_mode(
    state: &mut GameState,
    action: GameAction,
    mode: PublicFinalizeMode,
) -> Result<ActionResult, EngineError> {
    let actor = match &action {
        GameAction::Concede { player_id } => *player_id,
        // CR 103.5: For simultaneous-decision states, pick the first pending
        // player as the simulation representative. `authorized_submitters`
        // returns the full set; `first()` is deterministic (seat-ordered).
        _ => {
            let submitters = turn_control::authorized_submitters(state);
            submitters.first().copied().ok_or_else(|| {
                EngineError::InvalidAction(
                    "apply_as_current: no authorized submitter (game over?)".to_string(),
                )
            })?
        }
    };
    apply_action_boundary(state, actor, action, mode)
}

/// The action boundary at which a typed cost-move root is allowed to resume.
/// Keeping this finite boundary vocabulary prevents a cost payment from being
/// drained by an unrelated effect continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CostMoveDrainBoundary {
    ReplacementDelivered { action_event_start: usize },
    ReplacementPrevented { action_event_start: usize },
    PriorityBoundary,
}

/// CR 601.2h + CR 602.2b + CR 605.3b + CR 616.1: Drain the one typed cost-move
/// root eligible at this exact reducer boundary. Replacement delivery happens
/// before ordinary continuations; the common Priority boundary only resumes
/// Delve and mana-ability cursors after those continuations have settled.
pub(crate) fn drain_pending_cost_move_resume(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    boundary: CostMoveDrainBoundary,
) -> Result<Option<WaitingFor>, EngineError> {
    let eligible = match boundary {
        CostMoveDrainBoundary::ReplacementDelivered { .. } => matches!(
            state.pending_cost_move_resume,
            Some(
                PendingCostMoveResume::Cast { .. }
                    | PendingCostMoveResume::SacrificeForCost { .. }
                    | PendingCostMoveResume::WardSacrificePayment { .. }
                    | PendingCostMoveResume::ReplacementMayCost { .. }
                    | PendingCostMoveResume::CollectEvidencePayment { .. }
                    | PendingCostMoveResume::UnlessBouncePayment { .. }
                    | PendingCostMoveResume::DelveManaPayment { .. }
                    | PendingCostMoveResume::ManaAbilityPayment { .. }
                    | PendingCostMoveResume::ActivationMillPayment { .. }
                    | PendingCostMoveResume::LoyaltyActivation { .. }
            )
        ),
        // CR 606.4 + CR 616.1: a fully-prevented loyalty counter add (e.g. an
        // opponent's Solemnity would prevent the counters) must still complete the
        // parked activation instead of wedging, so `LoyaltyActivation` is eligible
        // at the Prevented boundary as well.
        CostMoveDrainBoundary::ReplacementPrevented { .. } => matches!(
            state.pending_cost_move_resume,
            Some(
                PendingCostMoveResume::Cast { .. }
                    | PendingCostMoveResume::SacrificeForCost { .. }
                    | PendingCostMoveResume::WardSacrificePayment { .. }
                    | PendingCostMoveResume::ReplacementMayCost { .. }
                    | PendingCostMoveResume::Foretell { .. }
                    | PendingCostMoveResume::CollectEvidencePayment { .. }
                    | PendingCostMoveResume::UnlessBouncePayment { .. }
                    | PendingCostMoveResume::DelveManaPayment { .. }
                    | PendingCostMoveResume::ManaAbilityPayment { .. }
                    | PendingCostMoveResume::ActivationMillPayment { .. }
                    | PendingCostMoveResume::LoyaltyActivation { .. }
            )
        ),
        CostMoveDrainBoundary::PriorityBoundary => matches!(
            state.pending_cost_move_resume,
            Some(
                PendingCostMoveResume::DelveManaPayment { .. }
                    | PendingCostMoveResume::ManaAbilityPayment { .. }
            )
        ),
    };
    if !eligible {
        return Ok(None);
    }

    let action_event_start = match boundary {
        CostMoveDrainBoundary::ReplacementDelivered { action_event_start }
        | CostMoveDrainBoundary::ReplacementPrevented { action_event_start } => {
            Some(action_event_start)
        }
        CostMoveDrainBoundary::PriorityBoundary => None,
    };
    let waiting_for = if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::Cast { .. } | PendingCostMoveResume::SacrificeForCost { .. })
    ) {
        casting_costs::resume_interrupted_cost_payment(state, events, action_event_start)?
    } else if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::WardSacrificePayment { .. })
    ) {
        engine_payment_choices::resume_ward_sacrifice_payment(state, events)?
    } else if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::ReplacementMayCost { .. })
    ) {
        super::costs::resume_replacement_may_cost_move(state, events)?
    } else if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::Foretell { .. })
    ) {
        super::casting::resume_foretell_cost_move(state, events)
    } else if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::CollectEvidencePayment { .. })
    ) {
        super::effects::collect_evidence::resume_cost_move_payment(state, events)?
    } else if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::UnlessBouncePayment { .. })
    ) {
        engine_payment_choices::resume_unless_bounce_cost_move(state, events)?
    } else if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::DelveManaPayment { .. })
    ) {
        resume_delve_mana_payment(state)
    } else if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::ManaAbilityPayment { .. })
    ) {
        mana_abilities::resume_mana_ability_cost_move(state, events)?
    } else if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::ActivationMillPayment { .. })
    ) {
        casting_costs::resume_activation_mill_cost_payment(state, events)?
    } else if matches!(
        state.pending_cost_move_resume,
        Some(PendingCostMoveResume::LoyaltyActivation { .. })
    ) {
        super::planeswalker::resume_loyalty_activation(state, events)?
    } else {
        unreachable!("eligible cost-move root must remain parked")
    };
    state.waiting_for = waiting_for.clone();
    Ok(Some(waiting_for))
}

pub(super) fn resume_pending_continuation_if_priority(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        // CR 118.3b + CR 119.4 + CR 616.1: A life-payment replacement child
        // drains to its recorded resolution boundary before the paid outer
        // action resumes. Ordinary continuations then drain only if that
        // deferred owner actually completed in this pass.
        let deferred_life_boundary = state
            .pending_deferred_life_cost_resume
            .as_ref()
            .map(crate::types::game_state::DeferredLifeCostResume::resume_at_resolution_depth);
        if deferred_life_boundary.is_none_or(|boundary| state.resolution_stack.len() > boundary) {
            super::life_safety::observe_boundary_carrier(state);
            effects::drain_pending_continuation(state, events);
        }
        if matches!(state.waiting_for, WaitingFor::Priority { .. })
            && deferred_life_boundary.is_none_or(|boundary| state.resolution_stack.len() > boundary)
        {
            super::life_safety::observe_boundary_carrier(state);
            effects::resume_resolution_frames(state, events);
        }
        let mut drained_deferred_life = false;
        if matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state
                .pending_deferred_life_cost_resume
                .as_ref()
                .is_some_and(|resume| {
                    state.resolution_stack.len() <= resume.resume_at_resolution_depth()
                })
        {
            super::life_safety::observe_boundary_carrier(state);
            let waiting_for = drain_pending_deferred_life_cost_resume(state, events)?;
            state.waiting_for = waiting_for;
            drained_deferred_life = true;
        }
        if matches!(state.waiting_for, WaitingFor::Priority { .. })
            && drained_deferred_life
            && state.pending_deferred_life_cost_resume.is_none()
        {
            super::life_safety::observe_boundary_carrier(state);
            effects::drain_pending_continuation(state, events);
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                super::life_safety::observe_boundary_carrier(state);
                effects::resume_resolution_frames(state, events);
            }
        }
        // CR 614.6 + CR 500.5: An interactive cross-event substitute may be
        // the child that suspended the APNAP phase-transition drain. Resume
        // that typed owner only after the post-replacement frame has
        // terminally drained; ordinary phase-boundary prompts use other states
        // and are intentionally unaffected.
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            super::life_safety::observe_boundary_carrier(state);
            turns::resume_phase_transition_after_post_replacement(state, events);
        }
        // CR 605.3b + CR 616.1: A post-replacement prompt reaches this common
        // boundary only after ordinary continuations drain. The shared typed
        // dispatcher owns the remaining eligible payment roots.
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            super::life_safety::observe_boundary_carrier(state);
            let _ = drain_pending_cost_move_resume(
                state,
                events,
                CostMoveDrainBoundary::PriorityBoundary,
            )?;
        }
    }
    settle_resolving_stack_entry_after_continuation_resume(state);
    Ok(())
}

/// CR 608.2c: A paused stack object remains the active resolution owner until
/// its continuation and every typed resolution frame have drained. Once this
/// priority boundary proves that completion, settle the exact carrier before
/// any deferred trigger can start its own resolution.
pub(super) fn settle_resolving_stack_entry_after_continuation_resume(state: &mut GameState) {
    if !matches!(state.waiting_for, WaitingFor::Priority { .. })
        || !resolving_stack_entry_can_settle(state)
    {
        return;
    }
    settle_finished_resolving_stack_entry(state);
}

/// CR 608.2c: Once a resolution has completed, its carrier must settle before
/// a trigger-selection prompt created by that resolution can construct a new
/// stack object. Unlike the priority-boundary wrapper above, this is called at
/// the exact completion point when the prompt has already replaced Priority.
pub(super) fn settle_resolving_stack_entry_before_trigger_selection(state: &mut GameState) {
    if !resolving_stack_entry_can_settle(state) {
        return;
    }
    settle_finished_resolving_stack_entry(state);
}

fn resolving_stack_entry_can_settle(state: &GameState) -> bool {
    state.resolving_stack_entry.is_some()
        && state.resolving_trigger_firing.is_some()
            == state
                .resolving_stack_entry
                .as_ref()
                .is_some_and(|entry| matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. }))
        && state.active_ability_continuation().is_none()
        && state.active_spell_resolution().is_none()
        && state.pending_cast.is_none()
        && state.pending_resolution_completion.is_none()
        && triggers::resolution_completion_can_settle(state)
}

fn settle_finished_resolving_stack_entry(state: &mut GameState) {
    debug_assert!(
        resolving_stack_entry_can_settle(state),
        "only a fully completed resolution carrier may settle"
    );
    super::stack::finish_resolving_stack_entry(
        state,
        super::lifecycle::DelayedTerminalDisposition::Resolved,
    );
    // CR 400.7j: resolution-scoped self-move state ends with its carrier.
    state.resolution_source_relatch = None;
}

/// CR 118.3b + CR 119.4 + CR 616.1: Resume the exact outer cost action after
/// the life-loss replacement's post-effect has drained back to its recorded
/// resolution-stack boundary.
fn drain_pending_deferred_life_cost_resume(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(resume) = state.pending_deferred_life_cost_resume.take() else {
        return Ok(state.waiting_for.clone());
    };
    let resume_for_restore = resume.clone();
    let result = (|| -> Result<WaitingFor, EngineError> {
        match resume {
            crate::types::game_state::DeferredLifeCostResume::Cast {
                player,
                pending,
                remaining_life_payments,
                resume_at_resolution_depth,
            } => {
                let pending = pending.ok_or_else(|| {
                    EngineError::InvalidAction(
                        "Deferred life payment is missing its cast or activation root".to_string(),
                    )
                })?;
                let mut remaining = remaining_life_payments.into_iter();
                while let Some(amount) = remaining.next() {
                    match super::life_costs::pay_life_as_cast_or_activation_cost(
                        state, player, amount, events,
                    ) {
                        super::life_costs::PayLifeCostResult::Paid { .. } => {}
                        super::life_costs::PayLifeCostResult::PaidWithDeferredSubstitution {
                            ..
                        }
                        | super::life_costs::PayLifeCostResult::DeferredReplacementChoice {
                            ..
                        } => {
                            state.pending_deferred_life_cost_resume =
                                Some(crate::types::game_state::DeferredLifeCostResume::Cast {
                                    player,
                                    pending: Some(pending),
                                    remaining_life_payments: remaining.collect(),
                                    resume_at_resolution_depth,
                                });
                            return Ok(state.waiting_for.clone());
                        }
                        super::life_costs::PayLifeCostResult::InsufficientLife
                        | super::life_costs::PayLifeCostResult::Prohibited => {
                            return Err(EngineError::ActionNotAllowed(
                                "Cannot complete deferred life cost".to_string(),
                            ));
                        }
                    }
                }
                if pending.prepaid_actual_mana_spent.is_some() {
                    state.pending_cast = Some(pending);
                    super::casting_costs::finalize_automatic_mana_payment(state, player, events)
                } else {
                    super::casting_costs::finish_pending_cost_or_cast(
                        state, player, *pending, events,
                    )
                }
            }
            crate::types::game_state::DeferredLifeCostResume::PayAmount {
                player, total, ..
            } => Ok(super::engine_resolution_choices::finish_pay_amount_choice(
                state, player, total, events,
            )),
            crate::types::game_state::DeferredLifeCostResume::ManaRoot {
                player,
                resume,
                remaining_life_payments,
                resume_at_resolution_depth,
            } => {
                let mut remaining = remaining_life_payments.into_iter();
                while let Some(amount) = remaining.next() {
                    match super::life_costs::pay_life_as_cost(state, player, amount, events) {
                        super::life_costs::PayLifeCostResult::Paid { .. } => {}
                        super::life_costs::PayLifeCostResult::PaidWithDeferredSubstitution {
                            ..
                        }
                        | super::life_costs::PayLifeCostResult::DeferredReplacementChoice {
                            ..
                        } => {
                            state.pending_deferred_life_cost_resume =
                                Some(crate::types::game_state::DeferredLifeCostResume::ManaRoot {
                                    player,
                                    resume,
                                    remaining_life_payments: remaining.collect(),
                                    resume_at_resolution_depth,
                                });
                            return Ok(state.waiting_for.clone());
                        }
                        super::life_costs::PayLifeCostResult::InsufficientLife
                        | super::life_costs::PayLifeCostResult::Prohibited => {
                            return Err(EngineError::ActionNotAllowed(
                                "Cannot complete deferred Phyrexian life payment".to_string(),
                            ));
                        }
                    }
                }
                super::mana_abilities::finish_mana_root_after_deferred_life_payment(
                    state, player, *resume, events,
                )
            }
        }
    })();
    if result.is_err() && state.pending_deferred_life_cost_resume.is_none() {
        state.pending_deferred_life_cost_resume = Some(resume_for_restore);
    }
    result
}

/// CR 702.66a: Finish one Delve payment after its graveyard-to-exile cost move
/// was delivered or fully replaced. The move's `TrackBySource` delivery tail
/// records only cards actually delivered to exile; this typed root restores the
/// exact Delve payment prompt and its one-generic cost reduction without
/// finalizing the pending cast.
pub(super) fn resume_delve_mana_payment(state: &mut GameState) -> WaitingFor {
    let Some(PendingCostMoveResume::DelveManaPayment { player, fuel_id }) =
        state.pending_cost_move_resume.take()
    else {
        unreachable!("delve cost-move resume requires its typed continuation")
    };
    // CR 118.3a: The generic-only marker is consumed by the shared mana-payment
    // finalizer and cannot be pinned or spent on a colored cost.
    let _ = state.add_mana_to_pool(
        player,
        crate::types::mana::ManaUnit::convoke_payment(
            crate::types::mana::ManaType::Colorless,
            fuel_id,
        ),
    );
    WaitingFor::ManaPayment {
        player,
        convoke_mode: Some(ConvokeMode::Delve),
    }
}

/// Decision emitted by the auto-pass loop's per-iteration check.
enum AutoPassDecision {
    /// No active auto-pass — leave the loop and let the frontend take over.
    Exit,
    /// Auto-pass completed or was interrupted (opponent action, phase stop,
    /// stack terminator). Clear the flag and exit.
    Finish,
    /// Continue passing priority for this iteration.
    Pass,
}

/// Classify what the auto-pass loop should do for `player` at the current
/// priority window.
///
/// Interrupts (MTGA-style): `UntilStackEmpty` bails when the stack empties or
/// grows beyond the baseline (trigger or opponent spell); `UntilTurnBoundary`
/// bails when an opponent-controlled object is on top of the stack or when the
/// current phase is in the user-supplied `phase_stops` list. The per-window
/// interrupt logic is boundary-agnostic — both `EndOfCurrentTurn` and
/// `MyNextTurnStart` behave identically within a priority window.
fn priority_auto_pass_decision(state: &GameState, player: PlayerId) -> AutoPassDecision {
    let Some(mode) = state.auto_pass.get(&player) else {
        return AutoPassDecision::Exit;
    };
    match mode {
        AutoPassMode::UntilStackEmpty { initial_stack_len } => {
            if state.stack.is_empty() || state.stack.len() > *initial_stack_len {
                AutoPassDecision::Finish
            } else {
                AutoPassDecision::Pass
            }
        }
        AutoPassMode::UntilTurnBoundary { .. } => {
            // CR 117.3d: An opponent-controlled top-of-stack normally ends the
            // session so the player can respond — unless they have pre-committed
            // to yield priority for that exact triggered ability, in which case
            // the session keeps auto-passing through it.
            let opponent_on_stack = state.stack.last().is_some_and(|top| {
                top.controller != player && !state.is_priority_yielded(player, top)
            });
            if opponent_on_stack || state.phase_stop_hit(player) {
                AutoPassDecision::Finish
            } else {
                AutoPassDecision::Pass
            }
        }
    }
}

/// True when `player` has an active turn-boundary auto-pass session (either
/// boundary). Both `EndOfCurrentTurn` and `MyNextTurnStart` drive the
/// DeclareAttackers/DeclareBlockers empty auto-submit arms, since both
/// auto-submit empty attackers within the current turn.
fn end_of_turn_active(state: &GameState, player: PlayerId) -> bool {
    matches!(
        state.auto_pass.get(&player),
        Some(AutoPassMode::UntilTurnBoundary { .. })
    )
}

fn pass_priority_once_with_pipeline(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    stack_resolution_limit: Option<u32>,
) -> Result<WaitingFor, EngineError> {
    if let WaitingFor::Priority { player } = &state.waiting_for {
        if super::precast_copy_shortcut::blocks_pass(state, *player) {
            return Ok(state.waiting_for.clone());
        }
    }
    state.cancelled_casts.clear();
    // CR 117.4 + 608.1: When all players pass in succession the stack begins
    // resolving; at that moment the AI guard against re-activating pending
    // abilities is no longer needed.
    state.pending_activations.clear();

    let stack_was_empty = state.stack.is_empty();
    // PR-3 (Option C) Defect-1: capture the pre-pipeline stack frame for the §2
    // loop-shortcut window maintenance below. `stack_top_before` is the resolving
    // entry's id; a real resolution this beat replaces the top with a different id
    // (every refilled trigger gets a fresh monotonic ObjectId), whereas a bare
    // priority handoff leaves it unchanged.
    let stack_len_before = state.stack.len();
    let stack_top_before = state.stack.last().map(|e| e.id);
    // CR 117.4 + CR 723.5/723.8: pass the *seat* that holds priority, not
    // `priority_player` — under turn-control the latter is the authorized
    // submitter (the controller), which would mis-count consecutive passes and
    // soft-lock the game.
    let current_seat = turn_control::priority_seat(state);
    let wf = priority::handle_priority_pass_with_limit(
        current_seat,
        state,
        events,
        stack_resolution_limit,
    );
    sync_waiting_for(state, &wf);

    // CR 608.2 + CR 117.4: Drain any pending continuation queued during the
    // priority pass (e.g. effects that chain a sub-resolution after the parent
    // settles) while the stack is still in its post-resolution state. Without
    // this drain, a continuation queued after a no-choice effect would sit
    // until an unrelated action, by which point referenced stack objects may
    // have left the stack.
    resume_pending_continuation_if_priority(state, events)?;

    let skip_triggers =
        stack_was_empty && !state.stack.is_empty() && state.phase == Phase::CombatDamage;

    let wf = engine_priority::run_post_action_pipeline(
        state,
        events,
        &state.waiting_for.clone(),
        skip_triggers,
        false,
    )?;
    sync_waiting_for(state, &wf);

    // PR-3 (Option C) CR 732.2a loop-shortcut window accumulation — relocated here
    // (PR3 Defect-1 fix). The refilling trigger is placed by
    // `run_post_action_pipeline` (CR 603.3 / CR 704.3: triggered abilities waiting to
    // go on the stack are put there the next time a player would receive priority),
    // which runs above — AFTER the resolution seam in `handle_priority_pass_with_limit`.
    // Sampling here is the only frame where a self-refilling cascade is already
    // non-shrinking (the refilled trigger is on the stack).
    //
    // RESOLUTION-OCCURRED GATE. `resolved_this_beat` is true iff there WAS a top entry
    // at function entry and it is no longer the top — i.e. a stack entry was actually
    // resolved/consumed this beat. A bare priority handoff (the active player passes,
    // priority moves on, stack untouched) leaves the top unchanged ⇒
    // `resolved_this_beat == false` ⇒ the ring is LEFT INTACT so accumulation survives
    // across the handoff beats that separate resolutions under the per-beat drive. A
    // naive `len >= before` gate would false-positive on those handoffs; a strict
    // clear-on-handoff would destroy the accumulation — both are wrong. This gate
    // samples only on a real resolution and touches the ring only then.
    let resolved_this_beat =
        stack_top_before.is_some() && state.stack.last().map(|e| e.id) != stack_top_before;
    // CR 732.2a: sample the loop-detection ring ONLY when the user-controllable
    // combo-detector is enabled. With `loop_detection == Off` (the default) the ring
    // is never populated, so the engine pays none of the per-resolution
    // `normalize_for_loop` clone cost and the reconcile-seam shortcut (which guards on
    // a non-empty ring AND the same flag) can never fire — exact pre-detector behavior.
    // PR-7 Phase 3: `samples()` so `Interactive` populates the ring identically to `On`;
    // `Off` (false) and `On` (true) are byte-preserved (`samples() == is_on()` for both).
    if resolved_this_beat && !in_simulation_probe() && state.loop_detection.samples() {
        // REFILL gate: a self-refilling MANDATORY cascade holds the stack non-empty and
        // non-shrinking across the resolution, settling at a non-interactive priority
        // window reset to the active player (the canonical modulo-comparison point —
        // `project_out_resources` compares phase/priority exactly). A normal multi-spell
        // stack SHRINKS; an interactive effect opens a non-Priority window; a finite
        // chain drains to empty — all three fall to the clear arm.
        if !state.stack.is_empty()
            && state.stack.len() >= stack_len_before
            && matches!(wf, WaitingFor::Priority { player } if player == state.active_player)
        {
            state.record_loop_detect_sample();
        } else if !wf.is_forced_cascade_window() {
            state.loop_detect_ring.clear();
        }
        // CR 603.3b/603.3d/603.5/608.2/903.9a + CR 703.1/117.3a + CR 732.2a: leave the
        // ring intact on every FORCED PRE-PRIORITY window, not just trigger ordering.
        // `is_forced_cascade_window` is the single authority for that class (the other
        // clear site, `apply_action`, consults the same predicate); it holds exactly the
        // windows at which no player has priority — the forced steps of putting triggers
        // on the stack / finishing a resolution, plus the CR 703.1 turn-based actions
        // CR 117.3a places before the step's own grant of priority — so answering one is
        // never a settle or a deliberate break. The stack is momentarily shrunk or empty
        // at these windows (an ordering batch is staged in `pending_trigger_order`; a
        // mid-resolution "may" pause has already popped its entry; a turn-based window
        // opens between phases with the stack drained), so without this arm the
        // accumulated `Priority{active}` samples would be discarded and a self-refilling
        // multi-trigger loop could never reach CR 732.2a detection. The turn-based
        // members buy RING SURVIVAL across a turn boundary — necessary but not yet
        // sufficient for the cross-turn shortcut CR 732.2a contemplates ("may even cross
        // multiple turns"), because `loop_states_equal` still compares `turn_number`
        // (via `impl PartialEq for GameState`, un-neutralized by `normalize_for_loop` and
        // `project_out_resources`), so no cross-turn pair certifies today. The measured
        // justification is the wipe itself: without these members the Fantastic Four dump
        // force-clears the ring once per 99-beat turn period at declare-attackers,
        // capping it at 2 frames where the widened class reaches 13.
    }
    // No else-branch: a bare handoff or an empty-stack pass-to-advance-phase does NOT
    // touch the ring (leave-intact), so accumulation survives the inter-resolution beats.

    Ok(wf)
}

fn active_until_stack_empty_requester(state: &GameState) -> Option<PlayerId> {
    state.auto_pass.iter().find_map(|(player, mode)| {
        matches!(mode, AutoPassMode::UntilStackEmpty { .. }).then_some(*player)
    })
}

fn priority_player_has_meaningful_action(state: &GameState) -> bool {
    let mut probe_state = state.clone();
    probe_state.auto_pass.clear();
    super::layers::flush_layers(&mut probe_state);
    let player = match probe_state.waiting_for {
        WaitingFor::Priority { player } => player,
        _ => probe_state.priority_player,
    };
    let probe = super::casting::PriorityCastProbe::from_flushed_state(probe_state, player);
    // The probe always has `waiting_for == Priority` at both call sites, so the
    // flat priority-action path is byte-identical to what `legal_actions` yielded
    // — it drops only the unused spell-cost object-walk and grouped-map build.
    let actions = crate::ai_support::flat_priority_actions_with_probe(probe.state(), Some(&probe));
    crate::ai_support::has_meaningful_priority_action(probe.state(), &actions)
}

/// CR 732.5: no player can be forced to keep looping if ANY of them could take an
/// action that ends the loop. The cap-path [`priority_player_has_meaningful_action`]
/// checks only the CURRENT priority holder; the loop-shortcut WIN designates a
/// LOSER, so its gate must be stronger — the would-be loop-breaker (a victim whose
/// priority is auto-passed by a stale `UntilStackEmpty`/`UntilTurnBoundary` session,
/// which `priority_auto_pass_decision` Passes WITHOUT a meaningful check) need NOT
/// hold priority at the modulo-match iteration. Probe EVERY living player as the
/// priority holder (`legal_actions`/`has_meaningful_priority_action` key off
/// `waiting_for`). Conservative: if anyone has a meaningful action this returns
/// `false` and the cascade falls through to the existing halt (priority preserved) —
/// fail-safe toward the status quo, never a wrong win.
fn no_living_player_has_meaningful_priority_action(state: &GameState) -> bool {
    state.players.iter().filter(|p| !p.is_eliminated).all(|p| {
        let mut probe_state = state.clone();
        probe_state.auto_pass.clear();
        probe_state.priority_player = p.id;
        probe_state.waiting_for = WaitingFor::Priority { player: p.id };
        super::layers::flush_layers(&mut probe_state);
        let probe = super::casting::PriorityCastProbe::from_flushed_state(probe_state, p.id);
        let actions =
            crate::ai_support::flat_priority_actions_with_probe(probe.state(), Some(&probe));
        !crate::ai_support::has_meaningful_priority_action(probe.state(), &actions)
    })
}

fn finish_completed_or_interrupted_until_stack_empty_sessions(state: &mut GameState) -> bool {
    let finished: Vec<PlayerId> = state
        .auto_pass
        .iter()
        .filter_map(|(player, mode)| match mode {
            AutoPassMode::UntilStackEmpty { initial_stack_len }
                if state.stack.is_empty() || state.stack.len() > *initial_stack_len =>
            {
                Some(*player)
            }
            _ => None,
        })
        .collect();

    for player in &finished {
        state.auto_pass.remove(player);
    }

    !finished.is_empty()
}

// CR 732.2a SAFETY LIMIT: a shortcut is "a loop that repeats a specified number of times";
// the CR places NO board-relative upper bound, so this is an engine implementation cap
// against an absurd/hostile count — NOT a rules constraint. It bounds both a `Fixed(n)`
// cycle count (handle_declare_shortcut) and a template drive period (shortcut_drive_period).
// Motivating vector: a `u32` count scalar-encodes up to ~4.3e9 cycles in ~10 JSON bytes, so
// it sails through the 8 KB inbound WS frame cap (phase-server/src/main.rs:409/1420) yet
// would force ~4.3e9 GameState clones — a byte cap cannot see it, only this count cap can.
// 1_000 is generous vs any honest Fixed count (~10x KCI-style loops); worst-case bounded
// cost is 1_000 cycles x <=10_000 beats = 1e7.
// `pub(crate)`: also the CR 732.2a boundary-collapse `PayableResource::LoopCollapse`
// prompt max (turns.rs), reusing the one existing loop-count safety bound.
pub(crate) const MAX_SHORTCUT_CYCLES: u32 = 1_000;

fn auto_pass_loop_max_iterations(state: &GameState) -> usize {
    let living_players = state
        .players
        .iter()
        .filter(|player| !player.is_eliminated)
        .count()
        .max(1);
    state
        .stack
        .len()
        .saturating_mul(living_players)
        .saturating_mul(2)
        .saturating_add(16)
        .clamp(500, 10_000)
}

#[cfg(test)]
#[path = "engine_auto_pass_decision_tests.rs"]
mod auto_pass_decision_tests;

/// Auto-pass loop: when a player has an auto-pass flag and receives priority,
/// automatically pass for them until the goal condition is met or interrupted.
fn run_auto_pass_loop(state: &mut GameState, result: &mut ActionResult) -> bool {
    // CR 732.2: per-dispatch resource ceilings for a runaway mandatory cascade.
    // Sized above the largest legitimate single-dispatch burst (a Scute Swarm
    // landfall copies every Scute in one resolution — tested boards reach ~2,936
    // permanents) yet far below the WASM linear-memory exhaustion threshold
    // (hundreds of thousands of objects). The iteration cap below is the
    // sustained-growth backstop; these deltas catch heavy-per-iteration loops.
    const MAX_EVENT_GROWTH: usize = 50_000;
    const MAX_OBJECT_GROWTH: usize = 16_000;
    let events_baseline = result.events.len();
    let objects_baseline = state.objects.len();

    // CR 104.4b: bounded-state mandatory-loop detection. Fingerprinting starts
    // only after this many mandatory iterations (normal resolution settles far
    // sooner, so it pays nothing); stored normalized snapshots are capped so a
    // non-repeating mandatory sequence falls through to the Phase-1 backstop.
    const FINGERPRINT_AFTER_ITERS: usize = 32;
    const MAX_LOOP_WINDOW: usize = 128;
    let mut mandatory_iters = 0usize;
    let mut loop_window: VecDeque<(u64, GameState)> = VecDeque::new();

    let max_iterations = auto_pass_loop_max_iterations(state);
    let mut iteration = 0usize;
    let mut advanced = false;
    loop {
        // CR 732.2: the iteration cap was exhausted while a mandatory cascade is
        // still in flight (priority unsettled, non-empty stack, no meaningful
        // action) — halt gracefully, the same way the growth ceilings do, rather
        // than fall through and leave the game mid-cascade. Reached ONLY on true
        // exhaustion: every productive exit below uses `break`, leaving the loop
        // without passing this guard, so a normal short resolution never trips it.
        if iteration >= max_iterations {
            if matches!(result.waiting_for, WaitingFor::Priority { .. })
                && !state.stack.is_empty()
                && !priority_player_has_meaningful_action(state)
            {
                emit_resolution_halt(state, result);
            }
            break;
        }
        iteration += 1;

        match &result.waiting_for {
            WaitingFor::Priority { player } => {
                let player = *player;
                if super::precast_copy_shortcut::blocks_pass(state, player) {
                    break;
                }
                let decision = priority_auto_pass_decision(state, player);
                match decision {
                    AutoPassDecision::Exit => {
                        let Some(requester) = active_until_stack_empty_requester(state) else {
                            break;
                        };
                        if requester == player {
                            break;
                        }
                        if finish_completed_or_interrupted_until_stack_empty_sessions(state) {
                            break;
                        }
                        if priority_player_has_meaningful_action(state) {
                            break;
                        }
                    }
                    AutoPassDecision::Finish => {
                        state.auto_pass.remove(&player);
                        break;
                    }
                    AutoPassDecision::Pass => {}
                }

                let mut events = Vec::new();
                match pass_priority_once_with_pipeline(state, &mut events, None) {
                    Ok(wf) => {
                        advanced = true;
                        let stack_empty_or_grew =
                            finish_completed_or_interrupted_until_stack_empty_sessions(state);
                        result.events.extend(events);
                        result.waiting_for = wf;
                        // CR 732.2: a mandatory cascade growing the board or
                        // event stream past the resource ceiling cannot settle —
                        // halt gracefully rather than exhaust WASM memory.
                        if result.events.len().saturating_sub(events_baseline) > MAX_EVENT_GROWTH
                            || state.objects.len().saturating_sub(objects_baseline)
                                > MAX_OBJECT_GROWTH
                        {
                            emit_resolution_halt(state, result);
                            return advanced;
                        }

                        // CR 104.4b: detect a repeating mandatory loop. Every
                        // iteration here is mandatory by construction (a
                        // meaningful action would have broken the loop), so the
                        // window never spans an optional action. A cheap
                        // fingerprint pre-filters; a true repeat is CONFIRMED by
                        // deep state equality before any draw, so a fingerprint
                        // collision can never cause a wrongful draw.
                        mandatory_iters += 1;
                        if mandatory_iters >= FINGERPRINT_AFTER_ITERS
                            && matches!(result.waiting_for, WaitingFor::Priority { .. })
                        {
                            let fingerprint = state.loop_fingerprint();
                            let normalized = state.normalize_for_loop();
                            if loop_window.iter().any(|(fp, prior)| {
                                *fp == fingerprint
                                    && crate::types::game_state::loop_states_equal(
                                        &normalized,
                                        prior,
                                    )
                            }) {
                                // CR 104.4b + CR 732.4: a mandatory action
                                // repeated a prior state with no way to stop — a
                                // draw. CR 801.16: limited-range partial draw N/A
                                // while format_config.range_of_influence is None.
                                result.events.push(GameEvent::GameOver { winner: None });
                                result.waiting_for = WaitingFor::GameOver { winner: None };
                                state.waiting_for = WaitingFor::GameOver { winner: None };
                                match_flow::handle_game_over_transition(state);
                                return advanced;
                            }

                            // PR-3 (Option C): the NET-PROGRESS mandatory-loop WIN
                            // shortcut is NOT duplicated here. `run_auto_pass_loop`
                            // resolves via `pass_priority_once_with_pipeline` (:1339),
                            // whose §2 maintenance accumulates the persisted
                            // `loop_detect_ring` across these internal iterations, but
                            // `reconcile_terminal_result` (the §3 win site) is NOT called
                            // inside this loop — only at :200 AFTER it returns. So the §3
                            // shortcut does NOT accelerate this auto-pass grind: this loop
                            // runs its own net-progress drive to the natural CR 704.5a
                            // death (or the strict CR 104.4b DRAW block above) on its own.
                            // The accelerated path is the per-beat repeated
                            // `apply(PassPriority)` drive (the production frontend
                            // default), where §3 runs after every beat. Keeping a second
                            // win site here would create two divergent detectors.

                            // CR 104.4b: a sliding window of the most recent
                            // MAX_LOOP_WINDOW distinct states. A fill-once-and-stop
                            // buffer never records the cycle of a loop whose
                            // repeating phase begins after a long mandatory preamble
                            // (more than MAX_LOOP_WINDOW transient states), silently
                            // downgrading that bounded-state draw to a Phase-1 halt.
                            // Evicting the oldest keeps any period <= MAX_LOOP_WINDOW
                            // detectable regardless of when the cycle starts; the
                            // deep loop_states_equal confirmation above still gates
                            // every draw, so eviction never risks a wrongful draw.
                            if loop_window.len() == MAX_LOOP_WINDOW {
                                loop_window.pop_front();
                            }
                            loop_window.push_back((fingerprint, normalized));
                        }

                        if stack_empty_or_grew {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            // UntilTurnBoundary: auto-submit empty attackers unless the user
            // flagged this phase as a stop.
            WaitingFor::DeclareAttackers { player, .. }
                if end_of_turn_active(state, *player) && !state.phase_stop_hit(*player) =>
            {
                let mut events = Vec::new();
                match engine_combat::handle_empty_attackers(state, &mut events) {
                    Ok(wf) => {
                        advanced = true;
                        sync_waiting_for(state, &wf);
                        result.events.extend(events);
                        result.waiting_for = wf;
                    }
                    Err(_) => break,
                }
            }

            // Auto-submit empty blockers only when there's nothing to choose.
            // CR 509.1 says the turn-based action still runs when no legal blocks
            // are available, and CR 117.1c requires the active player to receive
            // priority during the step (instants and Ninjutsu-family activations
            // per CR 702.49 — notably Sneak, which is restricted to this step).
            // A phase stop on Declare Blockers overrides this even without an
            // auto-pass session: if the player explicitly asked to pause here,
            // honor it.
            WaitingFor::DeclareBlockers {
                player,
                valid_blocker_ids,
                ..
            } if !state.phase_stop_hit(*player)
                && (valid_blocker_ids.is_empty()
                    || !super::combat::has_attackers_in_play(state)) =>
            {
                let mut events = Vec::new();
                match engine_combat::handle_empty_blockers(state, *player, &mut events) {
                    Ok(wf) => {
                        advanced = true;
                        sync_waiting_for(state, &wf);
                        result.events.extend(events);
                        result.waiting_for = wf;
                    }
                    Err(_) => break,
                }
            }

            // Non-auto-passable WaitingFor (interactive choice, game over, etc.)
            _ => break,
        }
    }
    advanced
}

/// CR 732.2: settle a runaway mandatory cascade gracefully. Pauses resolution,
/// returns priority to the active player, and emits a non-fatal `ResolutionHalted`
/// log event so the UI/log explains why the cascade stopped. Reached three ways:
/// the event-growth ceiling, the object-growth ceiling, and iteration-cap
/// exhaustion. NOT a draw — a net-progress loop is a CR 732.2 shortcut the engine
/// cannot infer an iteration count for; a *repeating* state is a separate CR
/// 104.4b draw.
fn emit_resolution_halt(state: &mut GameState, result: &mut ActionResult) {
    // Diagnostic-only: the in-flight cascade's distinct stack-source ids.
    let mut involved: Vec<ObjectId> = state.stack.iter().map(|e| e.source_id).collect();
    involved.sort_unstable_by_key(|id| id.0);
    involved.dedup();
    result.events.push(GameEvent::ResolutionHalted { involved });

    priority::reset_priority(state);
    let wf = WaitingFor::Priority {
        player: state.active_player,
    };
    state.waiting_for = wf.clone();
    result.waiting_for = wf;
}

/// CR 707.10c: Finalize a `CopyRetarget` flow — write the slot-derived targets
/// back onto the copy's stack entry, emit `EffectResolved`, hand priority back
/// to the chooser, and drain any pending continuation queued during resolution.
fn finalize_copy_retarget(
    state: &mut GameState,
    player: PlayerId,
    copy_id: ObjectId,
    slots: &[crate::types::game_state::CopyTargetSlot],
    effect_kind: crate::types::ability::EffectKind,
    effect_source_id: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let paradigm_remaining_offers = match &state.waiting_for {
        WaitingFor::CopyRetarget {
            paradigm_remaining_offers,
            ..
        } => paradigm_remaining_offers.clone(),
        _ => None,
    };
    let targets: Vec<_> = slots
        .iter()
        .map(|slot| {
            slot.current.clone().ok_or_else(|| {
                EngineError::InvalidAction(
                    "Copy target selection has an unchosen target slot".to_string(),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(entry) = state.stack.iter_mut().find(|e| e.id == copy_id) {
        if let Some(ability) = entry.ability_mut() {
            ability.targets = targets;
        }
    }
    events.push(GameEvent::EffectResolved {
        kind: effect_kind,
        // Pre-metadata CopyRetarget saves omitted this field; those states were
        // generic copy-spell choices whose completion source is the copy.
        source_id: effect_source_id.unwrap_or(copy_id),
        subject: None,
    });
    // CR 707.10c + CR 603.2: Copy observers (Magecraft) must drain only after
    // the copy's targets are finalized, not while `CopyRetarget` is still open.
    if let Some(wf) =
        triggers::drain_deferred_triggers_after_stack_object_announcement(state, events)
    {
        if let Some(remaining) = paradigm_remaining_offers.filter(|offers| !offers.is_empty()) {
            effects::paradigm::stash_pending_remaining_offers(state, player, remaining);
        }
        state.waiting_for = wf;
        state.priority_player = player;
        resume_pending_continuation_if_priority(state, events)?;
        return Ok(());
    }
    state.waiting_for = if let Some(remaining) = paradigm_remaining_offers {
        effects::paradigm::waiting_after_remaining_offers(player, remaining)
    } else {
        WaitingFor::Priority { player }
    };
    state.priority_player = player;
    resume_pending_continuation_if_priority(state, events)?;
    Ok(())
}

fn apply_action(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
    stack_resolution_limit: Option<u32>,
) -> Result<ActionResult, EngineError> {
    // Clear stale revealed_cards from the previous action.
    // RevealTop reveals (e.g. Goblin Guide) are momentary — shown for one state update.
    // RevealHand reveals (e.g. Thoughtseize) persist through the RevealChoice interaction.
    // ManifestDread reveals persist through ManifestDreadChoice (cards come from WaitingFor).
    // CR 701.20b: DigChoice reveals (reveal-dig, e.g. Satyr Wayfinder) persist through
    // the selection — revealed cards remain public while the player chooses.
    if !matches!(
        state.waiting_for,
        WaitingFor::RevealChoice { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::DigChoice { .. }
            // CR 700.3 + CR 701.20a: Fact or Fiction reveals persist through
            // both the opponent's partition step and the controller's pile
            // choice — the cards remain public while both players interact.
            | WaitingFor::SeparatePilesChooseOpponent { .. }
            | WaitingFor::SeparatePilesPartition { .. }
            | WaitingFor::SeparatePilesChoice { .. }
    ) {
        state.revealed_cards.clear();
    }

    // CR 701.20e: A bare "look at the top card" peek is visible to the looker
    // only until they act on it. The peek window must survive the action that
    // serves the dependent "you may reveal that card" optional (the looked-at
    // card is shown while that `OptionalEffectChoice` is pending) and any
    // `RevealChoice` opened by a private look-at-hand, then clear on the next
    // action boundary — mirroring the momentary `revealed_cards` reveal.
    if !matches!(
        state.waiting_for,
        WaitingFor::OptionalEffectChoice { .. } | WaitingFor::RevealChoice { .. }
    ) {
        state.private_look_ids.clear();
        state.private_look_player = None;
    }

    let mut events = Vec::new();
    let mut triggers_processed_inline = false;
    let mut skip_deferred_trigger_drain = false;

    // CancelAutoPass works from any WaitingFor state (player may cancel during
    // interactive choices). Routed by `actor` — previously used
    // `authorized_submitter(state)`, which silently cancelled the wrong player's
    // session when fired while an opponent held the prompt.
    if matches!(action, GameAction::CancelAutoPass) {
        state.auto_pass.remove(&actor);
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // SetPhaseStops propagates the player's phase-stop preference. Pure preference
    // state — no game logic, no WaitingFor transition. Works from any state so
    // frontends can sync on preference changes regardless of the current prompt.
    // Routed by `actor` so the human can update their own stops while the AI
    // holds priority (the previous "authorized_submitter" lookup rejected this
    // outright via the WrongPlayer guard, surfacing as an in-game dispatch error).
    if let GameAction::SetPhaseStops { stops } = &action {
        if stops.is_empty() {
            state.phase_stops.remove(&actor);
        } else {
            state.phase_stops.insert(actor, stops.clone());
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // Priority-passing mode is a standing, actor-scoped UI preference. It may
    // be changed in any state and does not itself pass priority, advance the
    // game, emit events, clear yields, or disturb takeback/loop state.
    if let GameAction::SetPriorityPassingMode { mode } = &action {
        if *mode == crate::types::game_state::PriorityPassingMode::Standard {
            state.priority_passing_modes.remove(&actor);
        } else {
            state.priority_passing_modes.insert(actor, *mode);
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 117.3d: SetPriorityYield propagates the actor's standing priority-yield
    // preference — a pre-committed decision to pass priority while a class of
    // triggered ability resolves. Pure preference state, routed by `actor`, and
    // handled BEFORE the loop-ring clear and auto-pass session clearing below so
    // yields are exempt from that per-session teardown (CR 400.7: an `Add`
    // snapshots the source's latched identity from the on-stack trigger).
    if let GameAction::SetPriorityYield { op } = &action {
        match op {
            PriorityYieldOp::Add { source_id, scope } => {
                if let Some(target) = state.resolve_yield_target_from_stack(*source_id, *scope) {
                    state.add_priority_yield(actor, target);
                }
            }
            PriorityYieldOp::Remove { target } => {
                state.remove_priority_yield(actor, target);
            }
            PriorityYieldOp::ClearAll => {
                state.clear_priority_yields(actor);
            }
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 603.5: SetMayTriggerAutoChoice propagates the actor's stored "don't ask
    // again" auto-choices for optional ("may") triggers. Pure preference state,
    // routed by `actor`, and — like SetPriorityYield — handled before the
    // loop-ring / auto-pass teardown so it is a legal any-state mutation. Actor
    // scoping is enforced by overriding the key's player with `actor`, so a
    // player can only mutate their own preferences regardless of the payload.
    if let GameAction::SetMayTriggerAutoChoice { op } = &action {
        match op {
            MayTriggerAutoChoiceOp::Remove { key } => {
                let actor_key = MayTriggerAutoChoiceKey {
                    player: actor,
                    ..key.clone()
                };
                state.remove_may_trigger_auto_choice(&actor_key);
            }
            MayTriggerAutoChoiceOp::ClearAll => {
                state.clear_may_trigger_auto_choices(actor);
            }
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 603.3b: Preferences are written only by a live `OrderTriggers` response.
    // This public action can only forget the actor's saved preferences and remains a
    // legal any-state, actor-scoped preference action.
    if let GameAction::SetTriggerOrderTemplate { op } = &action {
        match op {
            TriggerOrderTemplateOp::ClearAll => {
                state.clear_trigger_order_templates(actor);
            }
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 402.3: Hand order has no game-rules significance — ReorderHand is a
    // display-preference update on the actor's own hand. Validated as a strict
    // permutation of the current hand and applied with no event emission, no
    // WaitingFor transition, and no auto-pass / lands-tapped clearing. Mirrors
    // the SetPhaseStops / CancelAutoPass pattern: any-state, routed by `actor`.
    if let GameAction::ReorderHand { order } = &action {
        // Canonical accessor in this crate is direct indexing — see
        // `state.players[player.0 as usize]` throughout `ai_support/candidates.rs`,
        // `game/companion.rs`, and the existing test module. Bounds-check via
        // `len()` rather than swapping to `.get_mut()`, to stay idiomatic with
        // the rest of the file.
        if (actor.0 as usize) >= state.players.len() {
            return Err(EngineError::InvalidAction(format!(
                "ReorderHand: actor {:?} is not a valid player index",
                actor
            )));
        }
        let player = &mut state.players[actor.0 as usize];

        if order.len() != player.hand.len() {
            return Err(EngineError::InvalidAction(format!(
                "ReorderHand: expected {} ids, got {}",
                player.hand.len(),
                order.len()
            )));
        }

        // Permutation check: same multiset. Sort copies and compare — O(n log n)
        // is fine for hand sizes (typically <= 7, capped well under any realistic
        // limit by CR 402.2 and our zone semantics). ObjectId is not Ord, so
        // sort by the inner u64 key directly.
        let mut current: Vec<ObjectId> = player.hand.iter().copied().collect();
        let mut requested = order.clone();
        current.sort_unstable_by_key(|id| id.0);
        requested.sort_unstable_by_key(|id| id.0);
        if current != requested {
            return Err(EngineError::InvalidAction(
                "ReorderHand: order is not a permutation of the current hand".into(),
            ));
        }

        player.hand = order.iter().copied().collect();

        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 104.3a: A player may concede at any time. Concede bypasses the WaitingFor
    // dispatch entirely — there is no priority/state check. Eliminating the player
    // performs CR 800.4a object cleanup and advances `waiting_for` if the conceder
    // owned it (see `eliminate_player`).
    if let GameAction::Concede { player_id } = action {
        let mut events = Vec::new();
        super::elimination::eliminate_player(state, player_id, &mut events);
        return Ok(ActionResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // Debug actions bypass WaitingFor dispatch — gated on debug_mode flag
    // (engine-level: the action runs) and `debug_permitted` (transport-level:
    // the player may submit). The transport layer (server-core / WASM) is
    // responsible for enforcing per-player permission; this engine check is
    // a defense-in-depth invariant — a player not in `debug_permitted` should
    // never have reached `apply`.
    if let GameAction::Debug(debug_action) = action {
        if !state.debug_mode {
            return Err(EngineError::InvalidAction(
                "Debug actions require debug_mode to be enabled".into(),
            ));
        }
        if !state.debug_permitted.is_empty() && !state.debug_permitted.contains(&actor) {
            return Err(EngineError::InvalidAction(
                "Debug actions require debug permission".into(),
            ));
        }
        let description = debug_action.describe(state);
        let mut result =
            super::engine_debug::apply_debug_action(state, actor, debug_action, &mut events)?;
        result
            .events
            .push(crate::types::events::GameEvent::DebugActionUsed {
                player_id: actor,
                description,
            });
        return Ok(result);
    }

    // Sandbox host-only grant/revoke of debug permission. server-core also
    // checks this at the transport boundary; the engine repeats the check as
    // defense-in-depth so WASM and P2P-host paths cannot be bypassed by a
    // malicious actor crafting the action shape directly. The host convention
    // (PlayerId(0)) is fixed across every transport — see
    // `crates/server-core/src/session.rs` `HOST_PLAYER`. Emits a public audit
    // event on success.
    const HOST_PLAYER: PlayerId = PlayerId(0);
    if matches!(
        action,
        GameAction::GrantDebugPermission { .. } | GameAction::RevokeDebugPermission { .. }
    ) {
        if !state.format_config.allow_debug_actions {
            return Err(EngineError::ActionNotAllowed(
                "Sandbox mode is not enabled for this game".to_string(),
            ));
        }
        if actor != HOST_PLAYER {
            return Err(EngineError::ActionNotAllowed(
                "Only the host can grant or revoke debug permission".to_string(),
            ));
        }
        if let GameAction::RevokeDebugPermission { player_id } = action {
            if player_id == HOST_PLAYER {
                return Err(EngineError::ActionNotAllowed(
                    "The host cannot revoke their own debug permission".to_string(),
                ));
            }
        }
    }
    if let GameAction::GrantDebugPermission { player_id } = action {
        state.debug_permitted.insert(player_id);
        events.push(crate::types::events::GameEvent::DebugPermissionGranted {
            host: actor,
            player_id,
        });
        return Ok(ActionResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }
    if let GameAction::RevokeDebugPermission { player_id } = action {
        state.debug_permitted.remove(&player_id);
        events.push(crate::types::events::GameEvent::DebugPermissionRevoked {
            host: actor,
            player_id,
        });
        return Ok(ActionResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // PR-3 (Option C): CR 732.2a loop-detection ring invalidation. Any deliberate
    // non-pass action (cast / activate / play-land) breaks a self-refilling mandatory
    // cascade, so the accumulated detection window is stale and must be dropped.
    // Placed AFTER every preference early-return (CancelAutoPass / SetPhaseStops /
    // SetPriorityPassingMode / ReorderHand / Debug / Grant- & RevokeDebugPermission)
    // so a no-op preference
    // toggle never reaches here; PassPriority and OrderTriggers are the only actions
    // that CONTINUE a cascade and so must NOT clear (see the CR 603.3b note below).
    // `run_auto_pass_loop` and `resolve_all_fast_forward`
    // call the resolution seam directly (not via `apply_action`), so this clear does
    // not fire during their internal iterations — the ring accumulates correctly there.
    //
    // CR 603.3b + CR 732.2a: PassPriority AND OrderTriggers both CONTINUE a mandatory
    // cascade (OrderTriggers is the forced CR 603.3b placement of simultaneous triggers,
    // not a deliberate action). Every other action (cast/activate/play-land) is a
    // deliberate break and still invalidates the ring.
    //
    // CR 603.3d / CR 603.5 + CR 608.2 / CR 903.9a / CR 703.1 + CR 117.3a: the second
    // conjunct keys on the
    // WINDOW BEING ANSWERED, not on the action, because `state.waiting_for` has not been
    // reduced yet here — the very next statement reads `state.waiting_for.acting_player()`
    // for `semantic_actor`. Answering a forced pre-priority window is not a deliberate
    // break of the cascade (no player had priority to break it with), so the ring must
    // survive the answer as well as the prompt; the sampler at the other clear site
    // consults the same `is_forced_cascade_window` authority. Keying on the window rather
    // than the action also covers every answering variant at once — an action-keyed list
    // would need `ChooseTarget`, `SelectTargets`, `DecideOptionalEffect` AND
    // `DecideOptionalEffectAndRemember`, and would silently miss the next one added.
    // Widening the class to the CR 703.1 turn-based windows makes that the decisive
    // argument rather than a convenience one: the same conjunct picked up
    // `DeclareAttackers`, `DeclareBlockers`, `ChooseUntap`, `ChooseExert`, `ChooseEnlist`
    // and the `SelectCards` that answers `DiscardToHandSize` with no edit here — and
    // `SelectCards` in particular is answer-overloaded across a dozen unrelated windows,
    // so an action-keyed list could not have expressed the class correctly at all.
    // `PassPriority` keeps its own action-side exemption because it is answered at a
    // `Priority` window, which is deliberately NOT in the forced class.
    if !matches!(
        action,
        GameAction::PassPriority | GameAction::OrderTriggers { .. }
    ) && !state.waiting_for.is_forced_cascade_window()
    {
        state.loop_detect_ring.clear();
    }

    // Keep the semantic owner of the prompt before reducing it. Under turn
    // control this can differ from the authenticated submitter; a successful
    // action discharges a shortened shortcut only for that owner.
    let semantic_actor = state.waiting_for.acting_player().unwrap_or(actor);
    let action_for_divergence = action.clone();

    // Any deliberate player action (not auto-pass-related or a simple pass) cancels their auto-pass.
    // CR 103.5: Use the authenticated `actor` directly so the simultaneous mulligan
    // variants (where `authorized_submitter` is None when multiple players are pending)
    // still clear per-actor side-effect state correctly.
    match &action {
        GameAction::SetAutoPass { .. }
        | GameAction::PassPriority
        | GameAction::ReorderHand { .. } => {}
        _ => {
            state.auto_pass.remove(&actor);
        }
    }

    // Clear manual mana-tap tracking when the player commits to a non-mana action.
    // ActivateAbility is handled per-arm (only non-mana abilities clear tracking).
    match &action {
        GameAction::PassPriority
        | GameAction::PlayLand { .. }
        | GameAction::CastSpell { .. }
        | GameAction::Foretell { .. }
        | GameAction::CastSpellAsSneak { .. }
        | GameAction::CastSpellAsWebSlinging { .. }
        | GameAction::CastSpellForFree { .. }
        | GameAction::CastSpellAsMiracle { .. }
        | GameAction::CastSpellAsMadness { .. }
        | GameAction::CancelCast
        | GameAction::UnlockRoomDoor { .. }
        | GameAction::RollPlanarDie
        | GameAction::PayUnlessCost { .. }
        | GameAction::PayCombatTax { .. } => {
            state.lands_tapped_for_mana.remove(&actor);
        }
        _ => {}
    }

    // Validate and process action against current WaitingFor
    let waiting_for = match (&state.waiting_for.clone(), action) {
        (WaitingFor::Priority { player }, GameAction::PassPriority) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if super::precast_copy_shortcut::blocks_pass(state, *player) {
                return Err(EngineError::ActionNotAllowed(
                    "A shortened pre-cast shortcut requires a different meaningful action before passing"
                        .to_string(),
                ));
            }
            let wf = pass_priority_once_with_pipeline(state, &mut events, stack_resolution_limit)?;
            return Ok(ActionResult {
                events,
                waiting_for: wf,
                log_entries: vec![],
            });
        }
        (WaitingFor::Priority { player }, GameAction::PlayLand { object_id, card_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            state.cancelled_casts.clear();
            // CR 116.2a: Playing a land is a special action — sorcery-speed, once per turn, stack must be empty.
            // CR 305.2: Playing a land is a special action, not a spell.
            handle_play_land(state, *player, object_id, card_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::TapLandForMana { selection }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            let events_before = events.len();
            let waiting_for = handle_tap_land_for_mana(
                state,
                *player,
                &selection,
                ManaAbilityResume::Priority,
                &mut events,
            )?;
            // CR 605.4a: Triggered mana abilities coupled to this semantic
            // land activation resolve immediately. This also consumes any
            // engine-authored Aura color override before the public boundary.
            triggers::resolve_tap_mana_triggers_inline(state, &mut events, events_before);
            record_mana_loop_action_step(
                state,
                *player,
                selection.source.object_id,
                crate::types::game_state::LoopAction::TapLandForMana { selection },
            );
            waiting_for
        }
        (WaitingFor::Priority { player }, GameAction::ActivateManaSource { selection }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if matches!(
                mana_sources::priority_mana_route(state, &selection),
                Some(mana_sources::PriorityManaRoute::LandTap)
            ) {
                return Err(EngineError::ActionNotAllowed(
                    "Land mana abilities use TapLandForMana".to_string(),
                ));
            }
            let events_before = events.len();
            let waiting_for = mana_sources::activate_mana_source_selection(
                state,
                *player,
                &selection,
                &mut events,
                ManaAbilityResume::Priority,
            )?;
            triggers::resolve_tap_mana_triggers_inline(state, &mut events, events_before);
            if let Some(ability_index) = selection.ability_index {
                record_mana_loop_action_step(
                    state,
                    *player,
                    selection.source.object_id,
                    crate::types::game_state::LoopAction::Activate {
                        source_id: selection.source.object_id,
                        ability_index,
                    },
                );
            }
            waiting_for
        }
        (WaitingFor::Priority { player }, GameAction::UntapLandForMana { object_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            handle_untap_land_for_mana(state, state.priority_player, object_id, &mut events)?;
            WaitingFor::Priority { player: *player }
        }
        (
            WaitingFor::Priority { player },
            GameAction::CastSpell {
                object_id,
                card_id,
                payment_mode,
                ..
            },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            casting::handle_cast_spell_with_payment_mode(
                state,
                *player,
                object_id,
                card_id,
                payment_mode,
                &mut events,
            )?
        }
        (WaitingFor::Priority { player }, GameAction::Foretell { object_id, card_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            casting::handle_foretell(state, *player, object_id, card_id, &mut events)?
        }
        // CR 602.1: Activated abilities have a cost and an effect, written as "[Cost]: [Effect.]"
        (
            WaitingFor::Priority { player },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            // Check if this is a mana ability -- resolve instantly without the stack
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                // CR 605.3b: Mana abilities resolve immediately without using the stack.
                let ability_def = obj.abilities[ability_index].clone();
                let is_land = obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Land);
                let wf = mana_abilities::activate_mana_ability(
                    state,
                    source_id,
                    *player,
                    ability_index,
                    &ability_def,
                    &mut events,
                    crate::types::game_state::ManaAbilityResume::Priority,
                    None,
                )?;
                // CR 605.3b: Track land mana taps for undo (UntapLandForMana),
                // matching the TapLandForMana path so dual lands are undoable
                // too. `ManaSourcePenalty::None` is the only variant that
                // allows undo — painlands (damage on resolution), pay-life
                // sources, and sacrifice sources all commit irreversible
                // state atomically with CR 605.3b resolution.
                if is_land
                    && mana_sources::object_mana_ability_penalty(state, source_id, &ability_def)
                        .is_undoable()
                {
                    state
                        .lands_tapped_for_mana
                        .entry(state.priority_player)
                        .or_default()
                        .push(source_id);
                }
                // P7 v3 (CR 605.3b + CR 732.2a): this off-stack activation is the opener of a
                // multi-activation loop period. The shared recorder also owns semantic
                // `TapLandForMana` actions so the two public mana surfaces cannot drift.
                record_mana_loop_action_step(
                    state,
                    *player,
                    source_id,
                    crate::types::game_state::LoopAction::Activate {
                        source_id,
                        ability_index,
                    },
                );
                wf
            } else if obj.loyalty.is_some()
                && ability_index < obj.abilities.len()
                && matches!(
                    obj.abilities[ability_index].cost,
                    Some(crate::types::ability::AbilityCost::Loyalty { .. })
                )
            {
                // CR 606.3: Loyalty abilities activate once per turn at sorcery speed.
                state.lands_tapped_for_mana.remove(player);
                planeswalker::handle_activate_loyalty(
                    state,
                    *player,
                    source_id,
                    ability_index,
                    &mut events,
                )?
            } else {
                // Non-mana activated ability — clear tracking
                state.lands_tapped_for_mana.remove(player);
                let wf = casting::handle_activate_ability(
                    state,
                    *player,
                    source_id,
                    ability_index,
                    &mut events,
                )?;
                // P7 v3 (CR 602.2a + CR 732.2a): accumulate this on-stack activation into the
                // current loop period. (1) if a period is already accumulating for THIS controller
                // → APPEND (the multi-activation engine's continuation beat, e.g. Basalt's
                // `{3}: Untap` after its mana beat); (2) else if this activation CREATES A TOKEN →
                // SEED a fresh 1-step period (the P3 object-growth path — the activation-shaped dual
                // of the recast capture's STATIC `is_token_creating` predicate); (3) else → CLEAR (a
                // lone non-token, non-continuing activation seeds nothing). ⛔ A `battlefield.len() >
                // before` gate is STRUCTURALLY DEAD (B1): the ability only goes on the STACK at this
                // beat; its token appears on RESOLUTION. The clone-drive is the oracle (M8): an
                // illegal 2nd activation returns `Err(RecastAbort)`, no offer. Gated by `samples()`
                // (#4603 Off never writes) + `!in_simulation_probe()` (the drive must NOT grow the
                // seq — it is COMPARED across the cover frames); Off clears (byte-identical to
                // pre-PR-7's `= None`), a probe leaves the field untouched.
                if in_simulation_probe() {
                    // Detection/materialize drive: leave the sequence byte-stable.
                } else if !state.loop_detection.samples() {
                    // Off (#4603): a non-mana activation clears the field (was `= None` pre-PR-7).
                    state.last_loop_action_sequence.clear();
                } else {
                    match state
                        .objects
                        .get(&source_id)
                        // Capture guard: only a live battlefield permanent is a valid source.
                        .filter(|o| o.zone == Zone::Battlefield)
                    {
                        Some(o) => {
                            let card_id = o.card_id;
                            let creates_token =
                                o.abilities.get(ability_index).is_some_and(|def| {
                                    let mut es = Vec::new();
                                    crate::analysis::ability_graph::collect_effects(def, &mut es);
                                    es.iter().any(|e| {
                                        matches!(e, crate::types::ability::Effect::Token { .. })
                                    })
                                });
                            let continuing = state
                                .last_loop_action_sequence
                                .first()
                                .is_some_and(|s| s.controller == *player);
                            let step = crate::types::game_state::LoopActionContext {
                                card_id,
                                controller: *player,
                                action: crate::types::game_state::LoopAction::Activate {
                                    source_id,
                                    ability_index,
                                },
                                convoke: None,
                                // FIX-1: pinless at capture; fixed choices appended at their apply arms.
                                pins: Vec::new(),
                            };
                            if continuing {
                                accumulate_loop_action_step(state, step);
                            } else if creates_token {
                                state.last_loop_action_sequence = vec![step];
                            } else {
                                state.last_loop_action_sequence.clear();
                            }
                        }
                        None => state.last_loop_action_sequence.clear(),
                    }
                }
                wf
            }
        }
        (WaitingFor::Priority { player }, GameAction::UnlockRoomDoor { object_id, door }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            handle_unlock_room_door(state, *player, object_id, door, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::RollPlanarDie) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            // CR 901.9 / CR 116.2i: Rolling the planar die as a special action
            // does not use the stack; the escalating cost is charged before the
            // roll and effect-caused rolls do not increment the counter.
            crate::game::planechase::take_paid_planar_die_action(state, *player, &mut events)?;
            WaitingFor::Priority { player: *player }
        }
        // CR 715.3a: Player chooses creature or Adventure face.
        (
            WaitingFor::CastOffer {
                player,
                kind:
                    CastOfferKind::Adventure {
                        object_id,
                        card_id,
                        payment_mode,
                    },
            },
            GameAction::ChooseAdventureFace { creature },
        ) => casting::handle_adventure_choice_with_payment_mode(
            state,
            *player,
            *object_id,
            *card_id,
            creature,
            *payment_mode,
            &mut events,
        )?,
        // CR 712.12 (land face) / CR 712.11b (spell face): Player chooses which
        // face of an MDFC to play (land) or cast (spell).
        (
            WaitingFor::ModalFaceChoice {
                player,
                object_id,
                card_id,
                payment_mode,
            },
            GameAction::ChooseModalFace { back_face },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if let Some(obj) = state.objects.get_mut(object_id) {
                if back_face {
                    // Swap to back face using existing primitives
                    let back = obj.back_face.take().expect("dual-faced card has back face");
                    let front_snapshot = super::printed_cards::snapshot_object_face(obj);
                    super::printed_cards::apply_back_face_to_object(obj, back);
                    obj.back_face = Some(front_snapshot);
                    // CR 712.8a (MDFC) / CR 709.3 (split): non-front face showing;
                    // `apply_zone_exit_cleanup` reverts when leaving the stack.
                    obj.modal_back_face = true;
                } else {
                    // Front face chosen — clear layout_kind so the intercept
                    // won't re-fire on re-entry into handle_play_land / handle_cast_spell.
                    if let Some(ref mut bf) = obj.back_face {
                        bf.layout_kind = None;
                    }
                }
                // After choosing either face, clear layout on the stashed other
                // half so cast/play re-entry does not re-prompt.
                if back_face {
                    if let Some(ref mut bf) = obj.back_face {
                        bf.layout_kind = None;
                    }
                }
            }
            // CR 712.12 / CR 712.11b: Route the re-entry by the now-active face's
            // type. A land face is put onto the battlefield via the play-land
            // special action (CR 712.12); a spell face is cast (CR 712.11b — Esika
            // // The Prismatic Bridge). After a swap
            // the new back_face (from snapshot_object_face) has layout_kind: None,
            // and a front-face choice clears it explicitly — so neither the
            // both-faces-land intercept nor the spell-face intercept re-fires.
            let active_is_land = state.objects.get(object_id).is_some_and(|obj| {
                obj.card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Land)
            });
            if active_is_land {
                handle_play_land(state, *player, *object_id, *card_id, &mut events)?
            } else {
                casting::handle_cast_spell_with_payment_mode(
                    state,
                    *player,
                    *object_id,
                    *card_id,
                    *payment_mode,
                    &mut events,
                )?
            }
        }
        // CR 118.9: Player chooses between the printed mana cost and the
        // keyword-granted alternative cost. The `keyword` axis on the waiting
        // state drives dispatch to the per-keyword post-payment handler
        // (CR 702.74a Evoke, CR 702.96a Overload, CR 702.103a Bestow,
        // CR 702.148a Cleave, custom Warp). Each keyword retains its own
        // resolver because post-payment semantics genuinely diverge — the
        // unification is purely at the player-decision layer.
        (
            WaitingFor::AlternativeCastChoice {
                player,
                object_id,
                card_id,
                payment_mode,
                keyword,
                ..
            },
            GameAction::ChooseAlternativeCast { choice },
        ) => {
            use crate::types::game_state::AlternativeCastKeyword;
            match keyword {
                AlternativeCastKeyword::Warp => casting::handle_warp_cost_choice_with_payment_mode(
                    state,
                    *player,
                    *object_id,
                    *card_id,
                    choice,
                    *payment_mode,
                    &mut events,
                )?,
                AlternativeCastKeyword::Evoke => {
                    casting::handle_evoke_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Emerge => {
                    casting::handle_emerge_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Dash => {
                    casting::handle_dash_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Blitz => {
                    casting::handle_blitz_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Spectacle => {
                    casting::handle_spectacle_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Prowl => {
                    casting::handle_prowl_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Overload => {
                    casting::handle_overload_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Bestow => {
                    casting::handle_bestow_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Awaken => {
                    casting::handle_awaken_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Mutate => {
                    // CR 702.140a: Handle the mutate alternative cost choice.
                    casting::handle_mutate_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Cleave => {
                    casting::handle_cleave_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::MoreThanMeetsTheEye => {
                    casting::handle_mtmte_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Impending => {
                    // CR 702.176a: Handle the impending alternative cost choice during casting.
                    casting::handle_impending_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Prototype => {
                    // CR 702.160a: Handle the prototype alternative cost choice during casting.
                    casting::handle_prototype_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::FaceDown => {
                    // CR 702.37c / CR 702.168b: Handle the "cast normally vs cast
                    // face down for {3}" choice for a Morph/Megamorph/Disguise card.
                    casting::handle_face_down_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
            }
        }
        (
            WaitingFor::CastingVariantChoice {
                player,
                object_id,
                card_id,
                payment_mode,
                options,
            },
            GameAction::ChooseCastingVariant { index },
        ) => casting::handle_casting_variant_choice_with_payment_mode(
            state,
            *player,
            *object_id,
            *card_id,
            options,
            index,
            *payment_mode,
            &mut events,
        )?,
        // CR 110.4: Player chose which permanent type slot to consume for a
        // multi-type graveyard cast via OncePerTurnPerPermanentType (Muldrotha).
        (
            WaitingFor::ChoosePermanentTypeSlot {
                player,
                object_id,
                card_id,
                source,
                payment_mode,
                available_slots,
            },
            GameAction::ChoosePermanentTypeSlot { slot },
        ) => {
            if !available_slots.contains(&slot) {
                return Err(EngineError::InvalidAction(
                    "Selected permanent type is not available for this cast".to_string(),
                ));
            }
            let is_land_play = slot == crate::types::card_type::CoreType::Land;
            if is_land_play {
                state.pending_permanent_type_slot = Some((*source, slot));
                handle_play_land(state, *player, *object_id, *card_id, &mut events)?
            } else {
                casting::handle_permanent_type_slot_choice_with_payment_mode(
                    state,
                    *player,
                    *object_id,
                    *card_id,
                    *source,
                    slot,
                    *payment_mode,
                    &mut events,
                )?
            }
        }
        // CR 110.4: Cancel during slot choice — return to priority.
        (WaitingFor::ChoosePermanentTypeSlot { player, .. }, GameAction::CancelCast) => {
            WaitingFor::Priority { player: *player }
        }
        (WaitingFor::ModeChoice { player, .. }, GameAction::SelectModes { indices }) => {
            casting::handle_select_modes(state, *player, indices, &mut events)?
        }
        (
            WaitingFor::ModeChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        (WaitingFor::TargetSelection { player, .. }, GameAction::SelectTargets { targets }) => {
            engine_casting::handle_target_selection_select_targets(
                state,
                *player,
                targets,
                &mut events,
            )?
        }
        (WaitingFor::TargetSelection { player, .. }, GameAction::ChooseTarget { target }) => {
            engine_casting::handle_target_selection_choose_target(
                state,
                *player,
                target,
                &mut events,
            )?
        }
        (
            WaitingFor::TargetSelection {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        (
            WaitingFor::OptionalCostChoice {
                player,
                cost,
                pending_cast,
                ..
            },
            GameAction::DecideOptionalCost { pay },
        ) => engine_casting::handle_optional_cost_choice(
            state,
            *player,
            *pending_cast.clone(),
            cost,
            pay,
            &mut events,
        )?,
        (
            WaitingFor::OptionalCostChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        (
            WaitingFor::ChooseGiftRecipient {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        // CR 702.47a–e: Splice — caster reveals a card to splice onto the spell
        // (re-offering for the rest), or declines to finish and proceed to targets.
        (
            WaitingFor::SpliceOffer {
                player,
                pending_cast,
                eligible,
            },
            GameAction::RespondToSpliceOffer { card },
        ) => splice::resolve_offer(
            state,
            *player,
            *pending_cast.clone(),
            eligible.clone(),
            card,
            &mut events,
        )?,
        (
            WaitingFor::SpliceOffer {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        // CR 601.2b: Defiler cycle — player decides whether to pay life for mana reduction.
        (
            WaitingFor::DefilerPayment {
                player,
                life_cost,
                mana_reduction,
                pending_cast,
            },
            GameAction::DecideOptionalCost { pay },
        ) => engine_casting::handle_defiler_payment(
            state,
            *player,
            *pending_cast.clone(),
            *life_cost,
            mana_reduction,
            pay,
            &mut events,
        )?,
        (
            WaitingFor::DefilerPayment {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        // CR 118.3 + CR 601.2b + CR 605.3b: Player selected objects to pay a
        // cost. The single `PayCost` state dispatches on `kind` (which action)
        // and `resume` (spell-cast vs mana-ability pipeline) to the
        // appropriate authoritative handler.
        (
            WaitingFor::PayCost {
                player,
                kind:
                    PayCostKind::RemoveCounter {
                        counter_type,
                        count: counter_count,
                        selection,
                    },
                choices,
                resume,
                ..
            },
            GameAction::ChooseRemoveCounterCostDistribution { distribution },
        ) => match resume {
            CostResume::Spell {
                spell: pending_cast,
            }
            | CostResume::SpellCost {
                spell: pending_cast,
                ..
            } => {
                casting_costs::handle_remove_counter_distribution_for_cost(
                    state,
                    *player,
                    *pending_cast.clone(),
                    *counter_count,
                    counter_type.clone(),
                    *selection,
                    choices,
                    &distribution,
                    &mut events,
                )?
            }
            CostResume::ManaAbility {
                ..
            } => {
                return Err(EngineError::InvalidAction(
                    "Counter-cost distribution is not valid for mana abilities".to_string(),
                ));
            }
            CostResume::Resolution => {
                return Err(EngineError::InvalidAction(
                    "Counter-cost distribution is not valid for resolution costs".to_string(),
                ));
            }
        },
        (
            WaitingFor::PayCost {
                player,
                kind,
                choices,
                count,
                min_count,
                resume,
            },
            GameAction::SelectCards { cards: chosen },
        ) => match resume {
            CostResume::Spell {
                spell: pending_cast,
            }
            | CostResume::SpellCost {
                spell: pending_cast,
                ..
            } => {
                let paid_cost = match resume {
                    CostResume::SpellCost { cost, source, .. } => {
                        Some(casting_costs::SpellCostPayment {
                            cost: cost.as_ref(),
                            source: *source,
                        })
                    }
                    _ => None,
                };
                match kind {
                PayCostKind::Discard => engine_casting::handle_discard_for_cost(
                    state,
                    *player,
                    *pending_cast.clone(),
                    *count,
                    choices,
                    &chosen,
                    &mut events,
                )?,
                PayCostKind::Reveal => engine_casting::handle_reveal_for_cost(
                    state,
                    *player,
                    *pending_cast.clone(),
                    *count,
                    choices,
                    &chosen,
                    &mut events,
                )?,
	                PayCostKind::Sacrifice => engine_casting::handle_sacrifice_for_cost(
	                    state,
	                    *player,
	                    *pending_cast.clone(),
	                    paid_cost,
	                    casting_costs::CostSelection {
	                        min_count: *min_count,
	                        count: *count,
	                        legal_permanents: choices,
	                        chosen: &chosen,
	                    },
	                    &mut events,
	                )?,
                PayCostKind::ReturnToHand => engine_casting::handle_return_to_hand_for_cost(
                    state,
                    *player,
                    *pending_cast.clone(),
                    *count,
                    choices,
                    &chosen,
                    &mut events,
                )?,
                // CR 601.2h: A ChangeZone effect-as-cost carries the optional
                // any-number exile selection and its cast-time reduction.
                PayCostKind::ExileFromZone { zone }
                    if paid_cost.as_ref().is_some_and(|payment| {
                        casting_costs::is_exile_any_number_effect_cost(payment.cost)
                    }) =>
                {
                    casting_costs::handle_exile_any_number_for_cost(
                        state,
                        *player,
                        *zone,
                        *pending_cast.clone(),
                        *count,
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                PayCostKind::ExileFromZone { zone } => engine_casting::handle_exile_for_cost(
                    state,
                    *player,
                    *zone,
                    *pending_cast.clone(),
                    *count,
                    choices,
                    &chosen,
                    &mut events,
                )?,
                // CR 601.2h + CR 701.13: Exile a battlefield permanent the player
                // controls as an additional/alternative cost (Food Chain class).
                PayCostKind::ExilePermanent { filter } => {
                    engine_casting::handle_exile_permanent_for_cost(
                        state,
                        *player,
                        filter.clone(),
                        *pending_cast.clone(),
                        *count,
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                // CR 701.3d + CR 608.2k: Unattach a matching attachment from the
                // source as an activation cost (Captain America's Throw). The
                // handler snapshots the detached Equipment as the cost-referent,
                // then re-surfaces the deferred damage division.
                PayCostKind::UnattachFrom { filter } => {
                    casting_costs::handle_unattach_for_cost(
                        state,
                        *player,
                        filter,
                        *pending_cast.clone(),
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                // CR 702.167a/b: Craft materials exile across the
                // battlefield/graveyard union.
                PayCostKind::ExileMaterials { materials } => {
                    engine_casting::handle_exile_materials_for_cost(
                        state,
                        *player,
                        materials.clone(),
                        *pending_cast.clone(),
                        (*min_count, *count),
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                // CR 117.1 + CR 601.2b + CR 608.2c: Aggregate-threshold "exile
                // any number" cost (Baron Helmut Zemo's Boast); the handler
                // validates the threshold, exiles, publishes the tracked set, and
                // binds the resolving ability's tracked-set sentinel to it.
                PayCostKind::ExileAggregate {
                    zone,
                    function,
                    property,
                    comparator,
                    value,
                    filter,
                } => engine_casting::handle_exile_aggregate_for_cost(
                    state,
                    *player,
                    *zone,
                    *function,
                    *property,
                    *comparator,
                    *value,
                    filter,
                    *pending_cast.clone(),
                    choices,
                    &chosen,
                    &mut events,
                )?,
                PayCostKind::RemoveCounter {
                    counter_type,
                    count: counter_count,
                    selection,
                } => {
                    casting_costs::handle_remove_counter_for_cost(
                        state,
                        *player,
                        *pending_cast.clone(),
                        *counter_count,
                        counter_type.clone(),
                        *selection,
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                PayCostKind::TapCreatures { aggregate } => {
                    engine_casting::handle_tap_creatures_for_spell_cost(
                        state,
                        *player,
                        *pending_cast.clone(),
                        *count,
                        *aggregate,
                        choices,
                        &chosen,
                        &mut events,
                    )?
                }
                PayCostKind::Behold { action } => engine_casting::handle_behold_for_cost(
                    state,
                    *player,
                    *pending_cast.clone(),
                    *count,
                    choices,
                    *action,
                    &chosen,
                    &mut events,
                )?,
                // ExileFromManaZone is mana-ability-only; never appears with a
                // spell-cast resume.
                PayCostKind::ExileFromManaZone { .. } => {
                    return Err(EngineError::InvalidAction(
                        "ExileFromManaZone cost cannot resume a spell cast".into(),
                    ));
                }
                }
            }
            CostResume::ManaAbility {
                mana_ability: pending_mana_ability,
            } => match kind {
                // CR 605.1a: mana-ability tap costs are always fixed-count; the
                // aggregate form never resumes a mana ability.
                PayCostKind::TapCreatures { .. } => {
                    let wf = engine_casting::handle_tap_creatures_for_mana_ability(
                        state,
                        *count,
                        choices,
                        pending_mana_ability,
                        &chosen,
                        &mut events,
                    )?;
                    // FIX-1 (CR 605.1a + CR 608.2b): record the tap-cost target choice on the
                    // current loop-period step so the object-growth detection drive can replay
                    // "tap this legendary (Kilo) for the Relic mana ability". Slot source = the
                    // mana-ability cost source (distinct from the proliferate pin's Kilo source);
                    // `index: 0` (the color pin on the same source takes `index: 1`).
                    if let Some(source) =
                        object_decision_source(state, pending_mana_ability.source_id)
                    {
                        let targets: Vec<crate::analysis::decision_template::TargetPin> = chosen
                            .iter()
                            .filter_map(|&id| {
                                object_decision_source(state, id)
                                    .map(crate::analysis::decision_template::TargetPin::ByIdentity)
                            })
                            .collect();
                        if !targets.is_empty() {
                            record_loop_pin(
                                state,
                                *player,
                                crate::analysis::decision_template::PinnedDecision::Targets {
                                    slot: crate::analysis::decision_template::DecisionSlot {
                                        source,
                                        index: 0,
                                    },
                                    targets,
                                },
                            );
                        }
                    }
                    wf
                }
                PayCostKind::Discard => engine_casting::handle_discard_for_mana_ability(
                    state,
                    *count,
                    choices,
                    pending_mana_ability,
                    &chosen,
                    &mut events,
                )?,
                PayCostKind::ExileFromManaZone { .. } => {
                    super::mana_abilities::handle_exile_for_mana_ability(
                        state,
                        *count,
                        choices,
                        pending_mana_ability,
                        &chosen,
                        &mut events,
                    )?
                }
                PayCostKind::Sacrifice => super::mana_abilities::handle_sacrifice_for_mana_ability(
                    state,
                    *count,
                    choices,
                    pending_mana_ability,
                    &chosen,
                    &mut events,
                )?,
                // ReturnToHand, Reveal, ExileFromZone, RemoveCounter, and Behold
                // do not have mana-ability cost handlers wired today. If a
                // future mana ability uses one of these CR-valid cost shapes,
                // add the corresponding mana-ability handler instead of
                // routing it through the spell pipeline.
                PayCostKind::ReturnToHand
                | PayCostKind::Reveal
                | PayCostKind::ExileFromZone { .. }
                | PayCostKind::ExileMaterials { .. }
                | PayCostKind::ExilePermanent { .. }
                | PayCostKind::ExileAggregate { .. }
                | PayCostKind::RemoveCounter { .. }
                // CR 701.3d: an unattach-from cost is only ever surfaced via
                // `CostResume::Spell` (targeted activation), never as a mana
                // ability — unreachable here.
                | PayCostKind::UnattachFrom { .. }
                | PayCostKind::Behold { .. } => {
                    debug_assert!(
                        !matches!(kind, PayCostKind::UnattachFrom { .. }),
                        "UnattachFrom cost cannot resume a mana ability",
                    );
                    return Err(EngineError::InvalidAction(
                        "Cost kind cannot resume a mana ability".into(),
                    ));
                }
            },
            CostResume::Resolution => match kind {
                PayCostKind::TapCreatures { aggregate } => {
                    casting_costs::pay_tap_creatures_selection(
                        state,
                        *count,
                        *aggregate,
                        choices,
                        &chosen,
                        &mut events,
                    )?;
                    state.last_effect_count = Some(chosen.len() as i32);
                    if matches!(state.waiting_for, WaitingFor::PayCost { .. }) {
                        state.waiting_for = WaitingFor::Priority { player: *player };
                    }
                    effects::drain_pending_continuation(state, &mut events);
                    state.waiting_for.clone()
                }
                PayCostKind::Discard
                | PayCostKind::Reveal
                | PayCostKind::Sacrifice
                | PayCostKind::ReturnToHand
                | PayCostKind::ExileFromZone { .. }
                | PayCostKind::ExilePermanent { .. }
                | PayCostKind::UnattachFrom { .. }
                | PayCostKind::ExileMaterials { .. }
                | PayCostKind::ExileAggregate { .. }
                | PayCostKind::RemoveCounter { .. }
                | PayCostKind::Behold { .. }
                | PayCostKind::ExileFromManaZone { .. } => {
                    return Err(EngineError::InvalidAction(
                        "Cost kind cannot resume a resolution PayCost".into(),
                    ));
                }
            },
        },
        // CR 601.2: Player backed out of a cost-payment choice. Only spell
        // casts can be cancelled; mana-ability cost payment has no cancel path.
        (
            WaitingFor::PayCost {
                player,
                resume:
                    CostResume::Spell {
                        spell: pending_cast,
                    }
                    | CostResume::SpellCost {
                        spell: pending_cast,
                        ..
                    },
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        // CR 118.3: Player selected permanents to sacrifice as cost.
        (
            WaitingFor::ActivationCostOneOfChoice {
                player,
                costs,
                pending_cast,
            },
            GameAction::ChooseActivationCostBranch { index },
        ) => engine_casting::handle_activation_cost_one_of_choice(
            state,
            *player,
            *pending_cast.clone(),
            costs,
            index,
            &mut events,
        )?,
        (
            WaitingFor::ActivationCostOneOfChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        // CR 601.2b + CR 701.4a: player chose the creature type for a pre-choice
        // behold cost; record it and resume behold payment.
        (
            WaitingFor::CostTypeChoice {
                player,
                options,
                pending_cast,
                ..
            },
            GameAction::ChooseOption { choice },
        ) => casting_costs::handle_cost_type_choice(
            state,
            *player,
            *pending_cast.clone(),
            options,
            &choice,
            &mut events,
        )?,
        (
            WaitingFor::CostTypeChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        // Blight: player selected creature(s) to put -1/-1 counters on as cost.
        (
            WaitingFor::BlightChoice {
                player,
                counters,
                creatures,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => casting_costs::handle_blight_choice(
            state,
            *player,
            *pending_cast.clone(),
            *counters,
            creatures,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::BlightChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        (
            WaitingFor::ChooseManaColor {
                choice, context, ..
            },
            GameAction::ChooseManaColor {
                choice: chosen,
                count,
            },
        ) => {
            let events_before = events.len();
            let wf = match context {
                crate::types::game_state::ManaChoiceContext::ManaAbility(pending_mana_ability) => {
                    // CR 605.3a: validate the requested batch size BEFORE any mana
                    // is produced, so an out-of-range count rejects cleanly with
                    // no partial application. The cap is the just-activated source
                    // plus its choice-free identical twins.
                    if count as usize > pending_mana_ability.batch_siblings.len() + 1 {
                        return Err(EngineError::InvalidAction(format!(
                            "ChooseManaColor count {count} exceeds the {} batchable sources",
                            pending_mana_ability.batch_siblings.len() + 1
                        )));
                    }
                    let wf = engine_casting::handle_choose_mana_color(
                        state,
                        pending_mana_ability,
                        choice,
                        chosen.clone(),
                        &mut events,
                    )?;
                    // FIX-1 (CR 608.2d): record the fixed mana-color choice on the current
                    // loop-period step (slot `index: 1` — distinct from the tap-cost `Targets`
                    // pin at `index: 0` on the SAME mana-ability source) so the object-growth
                    // detection drive replays the exact color that keeps the loop mana-neutral
                    // (Blue → Freed's `{U}`). Only a WUBRG `SingleColor` choice is pinnable.
                    if let Some(color) = pinnable_mana_color(&chosen) {
                        if let Some(source) =
                            object_decision_source(state, pending_mana_ability.source_id)
                        {
                            record_loop_pin(
                                state,
                                pending_mana_ability.player,
                                crate::analysis::decision_template::PinnedDecision::ManaColor {
                                    slot: crate::analysis::decision_template::DecisionSlot {
                                        source,
                                        index: 1,
                                    },
                                    color,
                                },
                            );
                        }
                    }
                    // CR 605.3a: one color choice may bulk-activate the player's
                    // other identical, choice-free mana sources (their remaining
                    // Treasures, etc.) with the same color. Sibling cost/mana
                    // events append before the shared trigger scan below, so each
                    // sacrifice's observers fire exactly once.
                    if count > 1 {
                        engine_casting::batch_activate_mana_siblings(
                            state,
                            pending_mana_ability,
                            &chosen,
                            count,
                            &mut events,
                        )?;
                    }
                    wf
                }
                crate::types::game_state::ManaChoiceContext::ResolvingEffect(pending_effect) => {
                    effects::mana::handle_choose_mana_effect(
                        state,
                        pending_effect,
                        choice,
                        chosen.clone(),
                        &mut events,
                    )?
                }
            };
            // CR 603.2c + CR 605.4a: A mana color choice produces mana inline.
            // Scan its events for TapsForMana mana multipliers and for
            // cost-payment triggers HERE, because for `ManaPayment` /
            // `UnlessPayment` resumes the post-action pipeline is skipped
            // (it is guarded by `matches!(waiting_for, WaitingFor::Priority)`),
            // so this is the only scan site — and CR 605.4a requires the bonus
            // mana to enter the pool before the spell's payment step continues.
            // Do NOT "simplify" this scan away for non-Priority resumes.
            if events.len() > events_before {
                let mana_events: Vec<_> = events[events_before..].to_vec();
                super::triggers::process_triggers(state, &mana_events);
            }
            // CR 603.3b (#531): if the inline trigger scan paused on an
            // OrderTriggers prompt (controller has 2+ simultaneous TapsForMana
            // multipliers, etc.), surface that prompt instead of overwriting
            // it with the resume `wf` (Priority/ManaPayment). Preserve `wf`
            // so `handle_order_triggers` can resume the interrupted chain
            // after the ordered triggered mana abilities dispatch.
            if let Some(order_wf) =
                super::triggers::preserve_order_triggers_resume(state, wf.clone())
            {
                return Ok(ActionResult {
                    events,
                    waiting_for: order_wf,
                    log_entries: vec![],
                });
            }
            // CR 603.2c: For a `Priority` resume the post-action pipeline WOULD
            // re-scan these same events, double-firing the multiplier (issue
            // #443: Delighted Halfling under a mana multiplier yields 5 not 3).
            // Claim the scan via `triggers_processed_inline` — the same
            // mechanism `DeclareAttackers` uses — so the pipeline runs SBAs,
            // delayed/state triggers, and layers but skips the trigger re-scan.
            if matches!(wf, WaitingFor::Priority { .. }) {
                triggers_processed_inline = true;
            }
            wf
        }
        // CR 605.3a + CR 601.2h + CR 107.4e: Player submits the per-hybrid-shard
        // color vector for a mana-ability mana sub-cost (filter lands, etc.).
        (
            WaitingFor::PayManaAbilityMana {
                options,
                pending_mana_ability,
                ..
            },
            GameAction::PayManaAbilityMana { payment },
        ) => engine_casting::handle_pay_mana_ability_mana(
            state,
            options,
            pending_mana_ability,
            &payment,
            &mut events,
        )?,
        (
            WaitingFor::CollectEvidenceChoice {
                player,
                minimum_mana_value,
                cards: legal_cards,
                resume,
            },
            GameAction::SelectCards { cards: chosen },
        ) => super::effects::collect_evidence::handle_choice(
            state,
            *player,
            *minimum_mana_value,
            legal_cards,
            resume,
            &chosen,
            &mut events,
        )?,
        (WaitingFor::CollectEvidenceChoice { player, resume, .. }, GameAction::CancelCast) => {
            engine_casting::handle_collect_evidence_cancel(state, *player, resume, &mut events)
        }
        // CR 702.180b: Player chose which creature to tap for harmonize cost reduction.
        // CR 601.2b: Creature is tapped as part of paying the total cost.
        (
            WaitingFor::HarmonizeTapChoice {
                player,
                eligible_creatures,
                pending_cast,
            },
            GameAction::HarmonizeTap { creature_id },
        ) => engine_casting::handle_harmonize_tap_choice(
            state,
            *player,
            eligible_creatures,
            *pending_cast.clone(),
            creature_id,
            &mut events,
        )?,
        (
            WaitingFor::HarmonizeTapChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events)?,
        // CR 608.2d: Player decided whether to perform an optional effect ("You may X").
        (WaitingFor::OptionalEffectChoice { .. }, GameAction::DecideOptionalEffect { accept }) => {
            engine_payment_choices::handle_optional_effect_choice(state, accept, &mut events)?
        }
        (
            WaitingFor::PairChoice {
                player,
                source_id,
                choices,
            },
            GameAction::ChoosePair { partner },
        ) => {
            if let Some(partner_id) = partner {
                if !choices.contains(&partner_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected Soulbond partner is not legal".to_string(),
                    ));
                }
                if super::pairing::is_unpaired_creature_you_control(state, *source_id, *player)
                    && super::pairing::is_unpaired_creature_you_control(state, partner_id, *player)
                {
                    super::pairing::pair_objects(state, *source_id, partner_id, *player);
                }
            }
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::PairWith,
                source_id: *source_id,
            subject: None,});
            state.waiting_for = WaitingFor::Priority { player: *player };
            state.priority_player = *player;
            resume_pending_continuation_if_priority(state, &mut events)?;
            state.waiting_for.clone()
        }
        (
            waiting_for @ WaitingFor::OptionalEffectChoice { .. },
            GameAction::DecideOptionalEffectAndRemember { choice },
        ) => engine_payment_choices::handle_optional_effect_choice_and_remember(
            state,
            waiting_for.clone(),
            choice,
            &mut events,
        )?,
        // CR 608.2d: Opponent decided on "any opponent may" effect.
        (
            waiting_for @ WaitingFor::OpponentMayChoice { .. },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            return engine_payment_choices::handle_opponent_may_choice(
                state,
                waiting_for.clone(),
                accept,
                &mut events,
            );
        }
        // CR 732.2a: the proposer declares the loop shortcut. The offered `schema` (the
        // declared-choices contract the fail-closed firewall validates the pins against) is
        // threaded through — no longer dropped by `..`.
        (
            WaitingFor::LoopShortcut {
                proposer,
                predicted_winner,
                certificate,
                schema,
            },
            GameAction::DeclareShortcut { count, template },
        ) => {
            return handle_declare_shortcut(
                state,
                LoopShortcutOffer {
                    proposer: *proposer,
                    predicted_winner: *predicted_winner,
                    certificate,
                    schema,
                },
                count,
                template,
                &mut events,
            );
        }
        // CR 732.2a: the proposer DECLINES the offered shortcut (suggesting is optional).
        // Proposer-only authorization is enforced upstream by `check_actor_authorization`, so
        // `proposer`/`certificate`/`schema` are unused here (`..`).
        (WaitingFor::LoopShortcut { .. }, GameAction::DeclineShortcut) => {
            return handle_decline_shortcut(state, &mut events);
        }
        // The finite pre-cast protocol is intentionally isolated from the
        // legacy generic loop-shortcut handlers above.
        (
            WaitingFor::PrecastCopyShortcutOffer { .. }
            | WaitingFor::RespondToPrecastCopyShortcut { .. },
            GameAction::PrecastCopyShortcut { epoch, response },
        ) => super::precast_copy_shortcut::handle(state, actor, epoch, response, &mut events)?,
        // CR 732.2b/c: an opponent answers the loop-shortcut offer.
        (
            WaitingFor::RespondToShortcut {
                player,
                remaining_players,
                proposal,
            },
            GameAction::RespondToShortcut { response },
        ) => {
            return handle_respond_to_shortcut(
                state,
                *player,
                remaining_players.clone(),
                proposal.clone(),
                response,
                &mut events,
            );
        }
        // CR 702.104a: The chosen opponent for a Tribute creature decided pay/decline.
        (
            waiting_for @ WaitingFor::TributeChoice { .. },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            return engine_payment_choices::handle_tribute_choice(
                state,
                waiting_for.clone(),
                accept,
                &mut events,
            );
        }
        // CR 118.12: Player decided whether to pay an "unless pays" cost.
        (waiting_for @ WaitingFor::UnlessPayment { .. }, GameAction::PayUnlessCost { pay }) => {
            return engine_payment_choices::handle_unless_payment(
                state,
                waiting_for.clone(),
                pay,
                &mut events,
            );
        }
        // CR 118.12a: Player chose **which** sub-cost of a disjunctive
        // unless-cost to pay (or declined to pay any). On a `Some(idx)`
        // choice, the handler swaps the multi-cost prompt for a single-cost
        // `WaitingFor::UnlessPayment` carrying the chosen branch. On `None`
        // it falls through to the effect-happens path the same way a `pay:
        // false` answer to `PayUnlessCost` would.
        (
            waiting_for @ WaitingFor::UnlessPaymentChooseCost { .. },
            GameAction::ChooseUnlessCostBranch { choice },
        ) => {
            return engine_payment_choices::handle_unless_payment_choose_cost(
                state,
                waiting_for.clone(),
                choice,
                &mut events,
            );
        }
        // CR 508.1d + CR 508.1h + CR 509.1c + CR 509.1d: Player decided whether to
        // pay the locked-in combat tax. Resumes the paused attack/block declaration
        // with the matching sanitization per the accept/decline branch.
        (
            waiting_for @ WaitingFor::CombatTaxPayment { .. },
            GameAction::PayCombatTax { accept },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_pay_combat_tax(state, waiting_for.clone(), accept, &mut events)?
        }
        // Allow mana abilities during unless-payment choice (CR 118.12)
        (
            waiting_for @ WaitingFor::UnlessPayment { .. },
            GameAction::TapLandForMana { selection },
        ) => engine_payment_choices::handle_unless_payment_tap_land_for_mana(
            state,
            waiting_for.clone(),
            &selection,
            &mut events,
        )?,
        (
            waiting_for @ WaitingFor::UnlessPayment { .. },
            GameAction::UntapLandForMana { object_id },
        ) => engine_payment_choices::handle_unless_payment_untap_land_for_mana(
            state,
            waiting_for.clone(),
            object_id,
            &mut events,
        )?,
        // Allow mana abilities during unless-payment choice (CR 118.12)
        (
            waiting_for @ WaitingFor::UnlessPayment { .. },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => engine_payment_choices::handle_unless_payment_activate_ability(
            state,
            waiting_for.clone(),
            source_id,
            ability_index,
            &mut events,
        )?,
        // CR 702.21a: Player selected a card to discard as ward cost payment.
        (
            waiting_for @ WaitingFor::WardDiscardChoice { .. },
            GameAction::SelectCards { cards: chosen },
        ) => engine_payment_choices::handle_ward_discard_choice(
            state,
            waiting_for.clone(),
            chosen,
            &mut events,
        )?,
        // CR 702.21a: Player selected a permanent to sacrifice as ward cost payment.
        (
            waiting_for @ WaitingFor::WardSacrificeChoice { .. },
            GameAction::SelectCards { cards: chosen },
        ) => engine_payment_choices::handle_ward_sacrifice_choice(
            state,
            waiting_for.clone(),
            chosen,
            &mut events,
        )?,
        // CR 118.12: Player selected a permanent to return to hand as unless cost.
        (
            waiting_for @ WaitingFor::UnlessBounceChoice { .. },
            GameAction::SelectCards { cards: chosen },
        ) => engine_payment_choices::handle_unless_bounce_choice(
            state,
            waiting_for.clone(),
            chosen,
            &mut events,
        )?,
        (WaitingFor::ManaPayment { player, .. }, GameAction::CancelCast) => {
            // CR 601.2i: Cancelling at mana payment rolls back the cast — pop
            // the stack entry placed at announcement and return the object to
            // its origin zone via `cancel_pending_cast`.
            ensure_assist_cancellation_is_allowed(state)?;
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)?
                }
                None => WaitingFor::Priority { player },
            }
        }
        (
            WaitingFor::ManaSourceSelection {
                player,
                options,
                convoke_mode,
            },
            GameAction::BackToManaPayment,
        ) => {
            // The selection window never consumes mana or changes pins. Restore
            // the exact payment state rather than re-running the planner.
            let _ = options;
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: *convoke_mode,
            }
        }
        (
            WaitingFor::ManaSourceSelection {
                player,
                options,
                convoke_mode,
            },
            GameAction::ActivateManaSource { selection },
        ) => {
            if !options.contains(&selection) {
                return Err(EngineError::ActionNotAllowed(
                    "Mana source was not offered for this payment".to_string(),
                ));
            }
            let events_before = events.len();
            let waiting_for = mana_sources::activate_mana_source_selection(
                state,
                *player,
                &selection,
                &mut events,
                ManaAbilityResume::ManaPayment {
                    outer_player: Some(*player),
                    convoke_mode: *convoke_mode,
                },
            )?;
            triggers::resolve_tap_mana_triggers_inline(state, &mut events, events_before);
            waiting_for
        }
        (WaitingFor::ChooseXValue { player, .. }, GameAction::CancelCast) => {
            // CR 601.2f + CR 601.2i: Caster may back out before committing to an
            // X value. Pop the stack entry placed at announcement and restore.
            ensure_assist_cancellation_is_allowed(state)?;
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)?
                }
                None => WaitingFor::Priority { player },
            }
        }
        (WaitingFor::ChooseXValue { .. }, GameAction::PassPriority) => {
            // CR 601.2f: X must be chosen before the cast can proceed; passing priority
            // is not a legal way to skip this step.
            return Err(EngineError::ActionNotAllowed(
                "Cannot pass priority while choosing a value for X — commit with ChooseX or CancelCast."
                    .to_string(),
            ));
        }
        // CR 107.1b + CR 601.2f: Commit the chosen X value, then advance to mana payment.
        (
            WaitingFor::ChooseXValue {
                player,
                min,
                max,
                convoke_mode,
                ..
            },
            GameAction::ChooseX { value },
        ) => {
            if value < *min {
                return Err(EngineError::InvalidAction(format!(
                    "X={value} is below the minimum legal value of {min}",
                    min = *min,
                )));
            }
            if value > *max {
                return Err(EngineError::InvalidAction(format!(
                    "X={value} exceeds the maximum legal value of {max}",
                    max = *max,
                )));
            }
            let player = *player;
            let convoke_mode = *convoke_mode;
            if let Some(pending) = state.pending_cast.as_ref() {
                if pending.deferred_target_selection {
                    // CR 601.2c: A chosen X that determines target count must
                    // have a legal target assignment before it is locked into
                    // the pending cast.
                    // CR 601.2f: The same X value then determines the total cost.
                    let mut trial = pending.as_ref().clone();
                    trial.ability.set_chosen_x_recursive(value);
                    trial.cost.concretize_x(value);
                    let mut target_slots = build_target_slots(state, &trial.ability)?;
                    // CR 601.2c + CR 601.2d: clamp a divided spell's slots to the
                    // (now-known) pool so the legal-assignment probe matches what
                    // the controller will actually be offered (issue #2856).
                    cap_distribution_target_slots(
                        state,
                        &trial.ability,
                        trial.distribute.as_ref(),
                        &mut target_slots,
                    );
                    if !target_slots.is_empty()
                        && !has_legal_target_assignment_for_ability(
                            state,
                            &trial.ability,
                            &target_slots,
                            &trial.target_constraints,
                        )
                    {
                        return Err(EngineError::InvalidAction(format!(
                            "X={value} has no legal target assignment"
                        )));
                    }
                }
            }
            let pending = state.pending_cast.as_mut().ok_or_else(|| {
                EngineError::InvalidAction("No pending cast awaiting X".to_string())
            })?;
            pending.ability.set_chosen_x_recursive(value);
            pending.cost.concretize_x(value);
            let object_id = pending.object_id;
            events.push(GameEvent::XValueChosen {
                player,
                object_id,
                value,
            });
            // CR 601.2b + CR 601.2f: X is now locked in. Re-derive the full
            // concrete cost from the captured base — all reductions, target-
            // dependent modifiers, and Strive re-applied, with floors (Trinisphere
            // class) run LAST — against the now-concrete total, before payment is
            // determined. (Legacy/in-flight pending casts without a captured base
            // fall back to flooring the already-concretized cost.)
            casting::apply_post_x_cost_modifiers(state, player, object_id);
            casting_costs::enter_payment_step(state, player, convoke_mode, &mut events)?
        }
        // CR 601.2c + CR 115.1: The spell controller chose which opponent announces
        // an "of an opponent's choice" target slot. Record it on the in-flight cast
        // and resume the (deferred) target declaration; `resolve_effect_player_ref`
        // now routes that slot's chooser to the controller-selected opponent.
        (
            WaitingFor::ChooseAnnouncingOpponent {
                player,
                candidates,
                pending_cast,
                ..
            },
            GameAction::ChooseAnnouncingOpponent { opponent },
        ) => {
            if !candidates.contains(&opponent) {
                return Err(EngineError::InvalidAction(format!(
                    "Player {opponent:?} is not an eligible announcing opponent"
                )));
            }
            let caster = *player;
            let chosen = opponent;
            let mut pending = (**pending_cast).clone();
            // CR 601.2c + CR 115.1: Record the announcer for the FIRST still-
            // unassigned "of an opponent's choice" slot group only. Each such
            // effect is decided independently; `begin_deferred_target_selection`
            // re-prompts for any remaining groups, so the controller may pick the
            // same or different opponents per effect (Volcanic Offering).
            if !casting_costs::assign_next_announcing_opponent(&mut pending.ability, chosen) {
                return Err(EngineError::InvalidAction(
                    "No opponent-choice effect is awaiting an announcing opponent".to_string(),
                ));
            }
            casting_costs::begin_deferred_target_selection(state, caster, pending, &mut events)?
        }
        // CR 702.174a: Caster chose which opponent receives the promised Gift.
        (
            WaitingFor::ChooseGiftRecipient {
                player,
                candidates,
                pending_cast,
                ..
            },
            GameAction::ChooseGiftRecipient { opponent },
        ) => {
            let caster = *player;
            casting_costs::handle_choose_gift_recipient(
                state,
                caster,
                (**pending_cast).clone(),
                opponent,
                candidates,
                &mut events,
            )?
        }
        // CR 702.132a: Assist — caster chooses another player to help pay generic,
        // or declines. `assist_state` was set to `Offered` when the offer was made,
        // so both branches simply (re)enter the payment step from where they resume.
        (
            WaitingFor::AssistChoosePlayer {
                player,
                candidates,
                max_generic,
                convoke_mode,
            },
            GameAction::ChooseAssistPlayer { player: chosen },
        ) => {
            let caster = *player;
            let convoke_mode = *convoke_mode;
            match chosen {
                None => {
                    // CR 702.132a: declining proceeds to normal payment by the caster.
                    casting_costs::enter_payment_step(state, caster, convoke_mode, &mut events)?
                }
                Some(p) => {
                    if !candidates.contains(&p) {
                        return Err(EngineError::InvalidAction(format!(
                            "Player {p:?} is not an eligible assist helper"
                        )));
                    }
                    WaitingFor::AssistPayment {
                        caster,
                        chosen: p,
                        max_generic: *max_generic,
                        convoke_mode,
                    }
                }
            }
        }
        (WaitingFor::AssistChoosePlayer { player, .. }, GameAction::CancelCast) => {
            ensure_assist_cancellation_is_allowed(state)?;
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)?
                }
                None => WaitingFor::Priority { player },
            }
        }
        (WaitingFor::AssistChoosePlayer { .. }, GameAction::PassPriority) => {
            return Err(EngineError::ActionNotAllowed(
                "Must choose an assisting player or decline with ChooseAssistPlayer { player: None }, or CancelCast."
                    .to_string(),
            ));
        }
        // CR 702.132a + CR 601.2h: Assist records the selected generic
        // contribution and reduces the caster's owed generic now, but helper
        // resources stay untouched until final payment begins. The typed
        // PaymentStarted boundary, not this deferred intent, makes cancellation
        // unavailable once a helper source can have changed state.
        (
            WaitingFor::AssistPayment {
                caster,
                chosen,
                max_generic,
                convoke_mode,
            },
            GameAction::CommitAssistPayment { generic },
        ) => {
            let caster = *caster;
            let chosen = *chosen;
            let max_generic = *max_generic;
            let convoke_mode = *convoke_mode;
            if generic > max_generic {
                return Err(EngineError::InvalidAction(format!(
                    "Assist contribution {generic} exceeds the maximum {max_generic}"
                )));
            }
            if generic > 0 {
                use crate::types::mana::ManaCost;
                // CR 702.132a: validate the helper can actually produce the committed
                // generic (simulated auto-tap on a clone) before reducing the
                // caster's cost. No real taps happen here — see `apply_committed_assist`.
                let probe = ManaCost::Cost {
                    shards: Vec::new(),
                    generic,
                };
                let mut sim = state.clone();
                let mut sink = Vec::new();
                casting_costs::auto_tap_mana_sources(&mut sim, chosen, &probe, &mut sink, None);
                let feasible = casting::mana_ability_cost_payment_is_paused(&sim)
                    || sim
                        .players
                        .iter()
                        .find(|p| p.id == chosen)
                        .is_some_and(|p| mana_payment::can_pay(&p.mana_pool, &probe));
                if !feasible {
                    return Err(EngineError::InvalidAction(format!(
                        "Assisting player cannot produce {generic} generic mana"
                    )));
                }
                // Reduce the caster's owed generic and record the commitment; the
                // helper actually taps/spends at finalize.
                let pending = state.pending_cast.as_mut().ok_or_else(|| {
                    EngineError::InvalidAction("No pending cast for assist".to_string())
                })?;
                if let ManaCost::Cost { generic: owed, .. } = &mut pending.cost {
                    *owed = owed.saturating_sub(generic);
                }
                pending.assist_state = AssistState::Committed {
                    helper: chosen,
                    generic,
                };
            }
            casting_costs::enter_payment_step(state, caster, convoke_mode, &mut events)?
        }
        // CR 601.2h: Player has confirmed payment — delegate to the shared finalizer
        // that both this branch and the auto-pay path in `enter_payment_step` share.
        (WaitingFor::ManaPayment { player, .. }, GameAction::PassPriority) => {
            // CR 118.3a: `finalize_mana_payment` clears `active_payment_pins`
            // itself on every Ok/Err path, so no caller clear is needed.
            casting_costs::finalize_mana_payment(state, *player, &mut events)?
        }
        // CR 107.4f + CR 601.2f + CR 601.2h: Caster submitted per-shard Phyrexian
        // choices. Validate choice count + current affordability, then resume the
        // cast via `finalize_mana_payment_with_phyrexian_choices`.
        (
            WaitingFor::PhyrexianPayment {
                player,
                spell_object,
                shards,
            },
            GameAction::SubmitPhyrexianChoices { choices },
        ) => {
            let player = *player;
            let spell_object = *spell_object;
            let expected_len = shards.len();
            if choices.len() != expected_len {
                return Err(EngineError::InvalidAction(format!(
                    "Phyrexian choice count mismatch: expected {expected_len}, got {}",
                    choices.len()
                )));
            }
            // CR 118.3: Re-validate affordability against current state — life may have
            // dropped mid-cast (e.g., a life-loss replacement fired), so `PayLife` choices
            // on shards that now show `LifeOnly`/`ManaOrLife` must still have life available.
            {
                let pending_ref = state.pending_cast.as_ref().ok_or_else(|| {
                    EngineError::InvalidAction("No pending cast for Phyrexian payment".to_string())
                })?;
                let cost = pending_ref.cost.clone();
                let player_pool = state
                    .players
                    .iter()
                    .find(|p| p.id == player)
                    .map(|p| p.mana_pool.clone())
                    .ok_or_else(|| EngineError::InvalidAction("Player not found".to_string()))?;
                let activation_ability_index = pending_ref.activation_ability_index;
                let current_shards = if let Some(ability_index) = activation_ability_index {
                    let activation_context =
                        casting::activation_payment_context(
                            state,
                            spell_object,
                            Some(ability_index),
                        );
                    let activation_ctx = activation_context.as_payment_context();
                    let any_color = casting::player_can_spend_as_any_color_for_payment(
                        state,
                        player,
                        Some(spell_object),
                        Some(&activation_ctx),
                    );
                    let permissions = super::static_abilities::build_cost_permission_context(
                        state, player, any_color,
                    );
                    mana_payment::compute_phyrexian_shards(
                        &player_pool,
                        &cost,
                        Some(&activation_ctx),
                        permissions,
                    )
                } else {
                    let spell_meta = casting::build_spell_meta(state, player, spell_object);
                    let spell_ctx = spell_meta
                        .as_ref()
                        .map(crate::types::mana::PaymentContext::Spell);
                    let any_color = casting::player_can_spend_as_any_color_for_payment(
                        state,
                        player,
                        Some(spell_object),
                        spell_ctx.as_ref(),
                    );
                    let permissions = super::static_abilities::build_cost_permission_context(
                        state, player, any_color,
                    );
                    mana_payment::compute_phyrexian_shards(
                        &player_pool,
                        &cost,
                        spell_ctx.as_ref(),
                        permissions,
                    )
                };
                if current_shards.len() != expected_len {
                    return Err(EngineError::ActionNotAllowed(
                        "Phyrexian shard count changed during pause".to_string(),
                    ));
                }
                for (choice, shard) in choices.iter().zip(current_shards.iter()) {
                    if let (
                        crate::types::game_state::ShardChoice::PayLife,
                        crate::types::game_state::ShardOptions::ManaOnly,
                    ) = (choice, shard.options)
                    {
                        return Err(EngineError::ActionNotAllowed(
                            "Cannot pay life for shard — only mana available".to_string(),
                        ));
                    }
                }
                if !casting::pending_phyrexian_route_is_payable(
                    state,
                    player,
                    spell_object,
                    &choices,
                ) {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay mana cost with selected Phyrexian route".to_string(),
                    ));
                }
            }
            // CR 118.3a: `finalize_mana_payment_with_phyrexian_choices` clears
            // `active_payment_pins` itself on every Ok/Err path; no caller clear.
            casting_costs::finalize_mana_payment_with_phyrexian_choices(
                state,
                player,
                &choices,
                &mut events,
            )?
        }
        // CR 601.2i: CancelCast during Phyrexian payment rolls back the cast —
        // mirrors the ManaPayment CancelCast path.
        (WaitingFor::PhyrexianPayment { player, .. }, GameAction::CancelCast) => {
            ensure_assist_cancellation_is_allowed(state)?;
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)?
                }
                None => WaitingFor::Priority { player },
            }
        }
        // Allow mana abilities during mana payment (mid-cast)
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                let events_before = events.len();
                let ability_def = obj.abilities[ability_index].clone();
                let wf = mana_abilities::activate_mana_ability(
                    state,
                    source_id,
                    *player,
                    ability_index,
                    &ability_def,
                    &mut events,
                    crate::types::game_state::ManaAbilityResume::ManaPayment {
                        outer_player: Some(*player),
                        convoke_mode: *convoke_mode,
                    },
                    None,
                )?;
                // CR 605.1b: Process TapsForMana triggers inline during mana payment
                // (same rationale as the TapLandForMana arm below).
                // CR 605.3b + CR 616.1 + CR 603.3b: A paused costed mana
                // ability serializes its unscanned events in its typed cursor.
                // The cursor is their single settlement authority, so do not
                // scan them here and again when the replacement choice resumes.
                if events.len() > events_before
                    && !casting::mana_ability_cost_payment_is_paused(state)
                {
                    let mana_events: Vec<_> = events[events_before..].to_vec();
                    super::triggers::process_triggers(state, &mana_events);
                }
                if let Some(order_wf) =
                    super::triggers::preserve_order_triggers_resume(state, wf.clone())
                {
                    return Ok(ActionResult {
                        events,
                        waiting_for: order_wf,
                        log_entries: vec![],
                    });
                }
                wf
            } else {
                return Err(EngineError::ActionNotAllowed(
                    "Only mana abilities can be activated during mana payment".to_string(),
                ));
            }
        }
        // Allow basic land tapping during mana payment
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::TapLandForMana { selection },
        ) => {
            let events_before = events.len();
            let wf = handle_tap_land_for_mana(
                state,
                *player,
                &selection,
                ManaAbilityResume::ManaPayment {
                    outer_player: Some(*player),
                    convoke_mode: *convoke_mode,
                },
                &mut events,
            )?;
            super::triggers::resolve_tap_mana_triggers_inline(
                state,
                &mut events,
                events_before,
            );
            // CR 605.1b: TapsForMana triggered mana abilities (Wild Growth, Vorinclex,
            // Fertile Ground, Mana Flare class) must resolve inline when mana is
            // produced during cost payment. The ManaPayment path does not flow through
            // run_post_action_pipeline, so process triggers explicitly here so the
            // bonus mana reaches the pool before the payment check.
            if events.len() > events_before
                && !casting::mana_ability_cost_payment_is_paused(state)
            {
                let mana_events: Vec<_> = events[events_before..].to_vec();
                super::triggers::process_triggers(state, &mana_events);
            }
            if let Some(order_wf) =
                super::triggers::preserve_order_triggers_resume(state, wf.clone())
            {
                return Ok(ActionResult {
                    events,
                    waiting_for: order_wf,
                    log_entries: vec![],
                });
            }
            wf
        }
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::UntapLandForMana { object_id },
        ) => {
            handle_untap_land_for_mana(state, state.priority_player, object_id, &mut events)?;
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: *convoke_mode,
            }
        }
        // CR 118.3a: Pin a specific pool unit so the finalize spend prefers it.
        // Immediate-stage: records the hint on `pending_cast`, no stack push.
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::SpendPoolMana { pip_id },
        ) => {
            let (player, convoke_mode) = (*player, *convoke_mode);
            handle_spend_pool_mana(state, player, pip_id)?;
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            }
        }
        // CR 118.3a: Remove a previously-recorded pin (always legal).
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::UnspendPoolMana { pip_id },
        ) => {
            let (player, convoke_mode) = (*player, *convoke_mode);
            handle_unspend_pool_mana(state, pip_id);
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            }
        }
        // CR 702.51a / Waterbend: Tap a creature or artifact to pay mana.
        // CR 702.51a + CR 302.6: Convoke taps creatures to pay mana; summoning sickness
        // (CR 302.6) is not checked because convoke does not use the tap activated-ability mechanism.
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode:
                    Some(
                        mode @ (ConvokeMode::Convoke
                        | ConvokeMode::Waterbend
                        | ConvokeMode::Improvise),
                    ),
            },
            GameAction::TapForConvoke {
                object_id,
                mana_type,
            },
        ) => {
            let mode = *mode;
            let obj = state
                .objects
                .get(&object_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            let is_eligible = match mode {
                ConvokeMode::Convoke => obj.is_convoke_eligible(*player),
                ConvokeMode::Waterbend => obj.is_waterbend_eligible(*player),
                ConvokeMode::Improvise => obj.is_improvise_eligible(*player),
                // CR 702.66a: delve has a dedicated handler arm below (exile, not tap).
                ConvokeMode::Delve => unreachable!("delve uses its own ManaPayment arm"),
            };
            if !is_eligible {
                return Err(EngineError::ActionNotAllowed(
                    "Can only tap an eligible untapped permanent you control for convoke"
                        .to_string(),
                ));
            }
            let tapped_creature_for_convoke = mode == ConvokeMode::Convoke
                && obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Creature);
            // CR 702.51a: Validate color match for Convoke.
            let resolved_mana_type = match mode {
                ConvokeMode::Convoke => {
                    if let Some(color) = mana_sources::mana_type_to_color(mana_type) {
                        // Colored mana: creature must have that color
                        if !obj.color.contains(&color) {
                            return Err(EngineError::ActionNotAllowed(format!(
                                "Creature does not have color {:?} for convoke",
                                color
                            )));
                        }
                        mana_type
                    } else {
                        // Colorless: any creature can pay generic
                        crate::types::mana::ManaType::Colorless
                    }
                }
                // Waterbend always produces colorless
                ConvokeMode::Waterbend => crate::types::mana::ManaType::Colorless,
                // CR 702.126a: Improvise pays generic mana only — always colorless.
                ConvokeMode::Improvise => crate::types::mana::ManaType::Colorless,
                ConvokeMode::Delve => unreachable!("delve uses its own ManaPayment arm"),
            };
            // CR 701.26a + CR 508.1f: route the convoke tap through the single
            // authority so a "can't become tapped" creature is refused (no
            // summoning sickness check — CR 702.51a + CR 302.6).
            crate::game::restrictions::tap_permanent_for_cost(state, object_id, &mut events)?;
            let unit = match mode {
                ConvokeMode::Convoke => {
                    crate::types::mana::ManaUnit::convoke_payment(resolved_mana_type, object_id)
                }
                ConvokeMode::Waterbend => crate::types::mana::ManaUnit::new(
                    resolved_mana_type,
                    object_id,
                    false,
                    Vec::new(),
                ),
                // CR 702.126a/b: improvise mana exists only to pay this spell's
                // generic cost — `convoke_payment` carries the restriction that
                // keeps it from leaking into the pool as real mana.
                ConvokeMode::Improvise => {
                    crate::types::mana::ManaUnit::convoke_payment(resolved_mana_type, object_id)
                }
                ConvokeMode::Delve => unreachable!("delve uses its own ManaPayment arm"),
            };
            // CR 118.3a: stamp a pip id on pool entry. Convoke/improvise markers
            // are consumed by the shared algorithm and never pinned (the frontend
            // filters ConvokePayment units); Waterbend produces real pinnable mana.
            let _ = state.add_mana_to_pool(*player, unit);
            if mode == ConvokeMode::Waterbend {
                events.push(GameEvent::ManaAdded {
                    player_id: *player,
                    mana_type: resolved_mana_type,
                    source_id: object_id,
                    tap_state: ManaTapState::NotFromTap,
                });
            }
            if tapped_creature_for_convoke {
                let pending = state.pending_cast.as_mut().ok_or_else(|| {
                    EngineError::InvalidAction("No pending cast for convoke".to_string())
                })?;
                pending.convoked_creatures.push(object_id);
            }
            // Only emit waterbend event for Waterbend mode
            if mode == ConvokeMode::Waterbend {
                crate::game::bending::record_bending(
                    state,
                    &mut events,
                    BendingType::Water,
                    object_id,
                    *player,
                );
            }
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: Some(mode),
            }
        }
        // CR 702.66a: Delve — exile a card from the caster's graveyard to pay one
        // generic mana. Unlike convoke/improvise (which tap a permanent), the
        // source is a graveyard card that is exiled. The contribution is a
        // generic-only colorless marker (like Improvise) that can't leak into the
        // pool.
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode: Some(ConvokeMode::Delve),
            },
            GameAction::TapForConvoke {
                object_id,
                mana_type,
            },
        ) => {
            let player = *player;
            if mana_type != crate::types::mana::ManaType::Colorless {
                return Err(EngineError::ActionNotAllowed(
                    "Delve can only pay generic mana".to_string(),
                ));
            }
            let eligible = state
                .objects
                .get(&object_id)
                .is_some_and(|o| o.is_delve_eligible(player));
            if !eligible {
                return Err(EngineError::ActionNotAllowed(
                    "Can only delve a card from your own graveyard".to_string(),
                ));
            }
            let spell_id = state
                .pending_cast
                .as_ref()
                .map(|pending| pending.object_id)
                .ok_or_else(|| {
                    EngineError::InvalidAction("No pending cast for delve".to_string())
                })?;
            state.pending_cost_move_resume = Some(PendingCostMoveResume::DelveManaPayment {
                player,
                fuel_id: object_id,
            });
            // CR 702.66a + CR 614.1 + CR 616.1: The cost move must consult Moved
            // replacements. `track_exiled_by_source` carries
            // `ExileLinkSpec { duration: None, tracking: TrackBySource }`, so the
            // delivery tail links only fuel that actually reaches exile.
            match zone_pipeline::move_object(
                state,
                ZoneMoveRequest::cost(object_id, Zone::Exile, spell_id)
                    .track_exiled_by_source(),
                &mut events,
            ) {
                ZoneMoveResult::Done => resume_delve_mana_payment(state),
                ZoneMoveResult::NeedsChoice(_) => state.waiting_for.clone(),
                ZoneMoveResult::NeedsAuraAttachmentChoice => {
                    unreachable!("a delve cost move to exile cannot require an Aura attachment")
                }
            }
        }
        (WaitingFor::MulliganDecision { .. }, GameAction::MulliganDecision { choice }) => {
            // CR 103.5 + 103.5b: `actor` is already authorized as a member of
            // `pending` by `check_actor_authorization`. The mulligan module
            // resolves the per-player state update, transitioning the actor's
            // entry into `BottomCards` when a declare-point action still owes
            // bottoms, or advancing the flow when the pending set is empty.
            mulligan::handle_mulligan_decision(state, actor, choice, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (WaitingFor::MulliganDecision { .. }, GameAction::SelectCards { cards }) => {
            // CR 103.5: `actor` is already authorized as a member of `pending`.
            // A `SelectCards` submission resolves that player's owed
            // `BottomCards` sub-phase (rejected if their entry is in `Declare`).
            mulligan::handle_mulligan_bottom(state, actor, cards, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (WaitingFor::OpeningHandBottomCards { .. }, GameAction::SelectCards { cards }) => {
            // TL:R 906.6a/e: `actor` is already authorized as a member of
            // `pending`; no normal mulligan actions are available in this state.
            mulligan::handle_opening_hand_bottom(state, actor, cards, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (
            WaitingFor::DeclareAttackers { player, .. },
            GameAction::DeclareAttackers { attacks, bands },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_declare_attackers(state, *player, &attacks, &bands, &mut events)?
        }
        (
            WaitingFor::DeclareBlockers { player, .. },
            GameAction::DeclareBlockers { assignments },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_declare_blockers(state, *player, &assignments, &mut events)?
        }
        (
            WaitingFor::UntapChoice {
                player,
                candidates,
                chosen_not_to_untap,
            },
            GameAction::ChooseUntap { object_id, untap },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if !candidates.contains(&object_id) {
                return Err(EngineError::InvalidAction(
                    "Invalid untap choice object".to_string(),
                ));
            }

            let remaining: Vec<ObjectId> = candidates
                .iter()
                .copied()
                .filter(|candidate| candidate != &object_id)
                .collect();
            let mut declined = chosen_not_to_untap.clone();
            if !untap {
                declined.push(object_id);
            }

            if !remaining.is_empty() {
                WaitingFor::UntapChoice {
                    player: *player,
                    candidates: remaining,
                    chosen_not_to_untap: declined,
                }
            } else {
                // CR 502.3: Declines are recorded; now either surface the
                // required bounded `ChooseUntapSubset` prompt (a MaxUntapPerType
                // cap is over its limit after declines) or untap + advance. The
                // bridge advances the phase itself when it untaps, so only
                // resume `auto_advance` when no subset prompt was raised.
                let skipped: std::collections::HashSet<ObjectId> = declined.into_iter().collect();
                match turns::begin_untap_or_subset_prompt(state, &mut events, skipped) {
                    Some(prompt) => prompt,
                    None => turns::auto_advance(state, &mut events),
                }
            }
        }
        // CR 502.3: The active player directly determines which permanents untap
        // under a MaxUntapPerType cap (Smoke / Stoic Angel / Damping Field). The
        // chosen subset (`SelectCards`) must be a subset of the prompted `group`
        // and no larger than `max`; the unchosen complement is folded into the
        // declines and held tapped. Then the untap executes and the phase
        // advances. The enforcement clamp inside `execute_untap_with_choices`
        // remains as a safety net for any selection that slips past validation.
        (
            WaitingFor::ChooseUntapSubset { player, group, max },
            GameAction::SelectCards { cards: chosen },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if chosen.len() > *max {
                return Err(EngineError::InvalidAction(format!(
                    "Untap subset selects {} permanents but the cap allows {max}",
                    chosen.len()
                )));
            }
            let chosen_set: std::collections::HashSet<ObjectId> = chosen.iter().copied().collect();
            if chosen_set.len() != chosen.len() {
                return Err(EngineError::InvalidAction(
                    "Untap subset contains duplicate permanents".to_string(),
                ));
            }
            if let Some(bad) = chosen.iter().find(|id| !group.contains(id)) {
                return Err(EngineError::InvalidAction(format!(
                    "Untap subset object {bad:?} is not in the over-cap group"
                )));
            }
            // CR 502.3: the complement of the chosen set within the prompted
            // group stays tapped. Combine with the declines stashed from the
            // preceding optional-decline prompt.
            let mut skipped: std::collections::HashSet<ObjectId> =
                std::mem::take(&mut state.pending_untap_declines)
                    .into_iter()
                    .collect();
            for id in group {
                if !chosen_set.contains(id) {
                    skipped.insert(*id);
                }
            }
            match turns::begin_untap_or_subset_prompt(state, &mut events, skipped) {
                Some(prompt) => prompt,
                None => turns::auto_advance(state, &mut events),
            }
        }
        // CR 508.1g + CR 701.43d: the active player decides whether to pay the
        // optional "exert as it attacks" cost for the prompted attacker, one
        // attacker at a time. Triggers are deferred to `finish_declare_attackers`
        // (the buffered declaration + exert events fire together), so suppress
        // the epilogue's trigger pass for every step of the loop.
        (
            WaitingFor::ExertChoice {
                player,
                attacker,
                remaining,
            },
            GameAction::ChooseExert { exert },
        ) => {
            triggers_processed_inline = true;
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if exert {
                engine_combat::apply_attack_exert(state, *attacker, &mut events);
            }
            if let Some((next, rest)) = remaining.split_first() {
                WaitingFor::ExertChoice {
                    player: *player,
                    attacker: *next,
                    remaining: rest.to_vec(),
                }
            } else if let Some(waiting_for) =
                engine_combat::next_current_enlist_choice(state, *player)
            {
                waiting_for
            } else {
                engine_combat::finish_declare_attackers(state, &mut events, false)?
            }
        }
        // CR 508.1g + CR 702.154a: the active player may tap up to one eligible
        // creature for each Enlist instance as the source attacks. As with
        // exert, declaration/tap/enlist triggers are deferred until all optional
        // attack costs are decided.
        (
            WaitingFor::EnlistChoice {
                player,
                attacker,
                eligible,
                remaining,
            },
            GameAction::ChooseEnlist { target },
        ) => {
            triggers_processed_inline = true;
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if let Some(target) = target {
                if !eligible.contains(&target) {
                    return Err(EngineError::InvalidAction(format!(
                        "{target:?} is not an eligible Enlist target"
                    )));
                }
                engine_combat::apply_attack_enlist(state, *attacker, target, &mut events)?;
            }
            if let Some(waiting_for) =
                engine_combat::next_enlist_choice(state, *player, remaining.clone())
            {
                waiting_for
            } else {
                engine_combat::finish_declare_attackers(state, &mut events, false)?
            }
        }
        (WaitingFor::ReplacementChoice { .. }, GameAction::ChooseReplacement { index }) => {
            engine_replacement::handle_replacement_choice(state, index, &mut events)?
        }
        // CR 603.3b: Player submits the chosen order for their pending triggers.
        // `actor` is already authorized as the prompted player by
        // `check_actor_authorization` (via `WaitingFor::acting_player`).
        (WaitingFor::OrderTriggers { .. }, GameAction::OrderTriggers { order }) => {
            triggers::handle_order_triggers(state, order)?
        }
        // CR 707.9: Player chose a permanent to copy for "enter as a copy of" replacement.
        (
            waiting_for @ WaitingFor::CopyTargetChoice { .. },
            GameAction::ChooseTarget { target },
        ) => engine_replacement::handle_copy_target_choice(
            state,
            waiting_for.clone(),
            target,
            &mut events,
        )?,
        (
            WaitingFor::ExploreChoice {
                player,
                remaining,
                pending_effect,
                ..
            },
            GameAction::ChooseTarget { target },
        ) => {
            if turn_control::authorized_submitter(state) != Some(*player) {
                return Err(EngineError::WrongPlayer);
            }
            let chosen = match target {
                Some(TargetRef::Object(id)) => id,
                _ => {
                    return Err(EngineError::InvalidAction(
                        "Invalid explore choice".to_string(),
                    ));
                }
            };
            super::effects::explore::handle_choice(
                state,
                chosen,
                remaining,
                pending_effect.as_ref(),
                &mut events,
            )?
        }
        // CR 303.4 + CR 303.4f + CR 303.4g + CR 115.1: Player picked the
        // permanent to enchant for a return-as-Aura sub-effect or a non-spell
        // Aura battlefield entry. The picker is a CHOICE (not a target), so
        // the action shape mirrors
        // `WaitingFor::ExploreChoice` — `GameAction::ChooseTarget` with the
        // chosen `TargetRef` drawn from `legal_targets`.
        (
            WaitingFor::ReturnAsAuraTarget {
                player,
                source_id: _,
                returned_id,
                legal_targets,
                pending_effect,
            },
            GameAction::ChooseTarget { target },
        ) => {
            if turn_control::authorized_submitter(state) != Some(*player) {
                return Err(EngineError::WrongPlayer);
            }
            let chosen = match target {
                Some(target) if legal_targets.contains(&target) => target.clone(),
                _ => {
                    return Err(EngineError::InvalidAction(
                        "ReturnAsAuraTarget: invalid or missing legal target".to_string(),
                    ));
                }
            };
            let pending = pending_effect.clone();
            let returned = *returned_id;
            let active_player = *player;
            let (filter, grants) = match &pending.effect {
                crate::types::ability::Effect::ReturnAsAura {
                    enchant_filter,
                    grants,
                } => (enchant_filter.clone(), grants.clone()),
                _ => {
                    let old_target = match chosen {
                        TargetRef::Object(chosen_id) => {
                            super::effects::attach::attach_to(state, returned, chosen_id)
                        }
                        TargetRef::Player(chosen_player) => {
                            super::effects::attach::attach_to_player(state, returned, chosen_player)
                        }
                    };
                    if let Some(old_target) = old_target {
                        events.push(crate::types::events::GameEvent::Unattached {
                            attachment_id: returned,
                            old_target,
                        });
                    }
                    let resumes_change_zone_iteration = state
                        .active_change_zone_frame()
                        .is_some_and(|frame| frame.pending.is_some());
                    if !resumes_change_zone_iteration {
                        events.push(crate::types::events::GameEvent::EffectResolved {
                            kind: crate::types::ability::EffectKind::ChangeZone,
                            source_id: pending.source_id,
                        subject: None,});
                    }
                    state.waiting_for = WaitingFor::Priority {
                        player: active_player,
                    };
                    state.priority_player = active_player;
                    // CR 603.10a + CR 616.1: an aura-attachment pause can carry a
                    // deferred batch completion (a reveal-until / dig kept Aura
                    // whose entry paused before the rest pile was moved). Drain it
                    // here — the replacement-choice resume path drains it for the
                    // CR 616.1 case, but the aura-host resume is the ONLY drain
                    // site for an `NeedsAuraAttachmentChoice` pause.
                    if state.active_batch_delivery().is_some() {
                        super::zone_pipeline::drain_pending_batch_deliveries(state, &mut events);
                    }
                    resume_pending_continuation_if_priority(state, &mut events)?;
                    return Ok(ActionResult {
                        events,
                        waiting_for: state.waiting_for.clone(),
                        log_entries: vec![],
                    });
                }
            };
            let chosen = match chosen {
                TargetRef::Object(id) => id,
                TargetRef::Player(_) => {
                    return Err(EngineError::InvalidAction(
                        "ReturnAsAuraTarget: ReturnAsAura requires an object host".to_string(),
                    ));
                }
            };
            super::effects::return_as_aura::finalize_attach(
                state,
                pending.as_ref(),
                returned,
                chosen,
                &filter,
                grants,
                &mut events,
            )
            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            // After resolving the attach, return control to standard priority
            // flow under the picker's controller, then resume any chain that was
            // paused behind the picker.
            state.waiting_for = WaitingFor::Priority {
                player: active_player,
            };
            state.priority_player = active_player;
            // CR 603.10a + CR 616.1: drain a deferred batch completion parked
            // behind this aura-attachment pause (see the sibling path above).
            if state.active_batch_delivery().is_some() {
                super::zone_pipeline::drain_pending_batch_deliveries(state, &mut events);
            }
            resume_pending_continuation_if_priority(state, &mut events)?;
            state.waiting_for.clone()
        }
        (
            WaitingFor::EquipTarget {
                player,
                equipment_id,
                valid_targets,
            },
            GameAction::Equip {
                equipment_id: eq_id,
                target_id,
            },
        ) => {
            if eq_id != *equipment_id {
                return Err(EngineError::InvalidAction(
                    "Equipment ID mismatch".to_string(),
                ));
            }
            if !valid_targets.contains(&target_id) {
                return Err(EngineError::InvalidAction(
                    "Invalid equip target".to_string(),
                ));
            }
            let p = *player;
            push_keyword_action(
                state,
                p,
                eq_id,
                KeywordAction::Equip {
                    equipment_id: eq_id,
                    target_creature_id: target_id,
                },
                &mut events,
            )
        }
        (WaitingFor::Priority { player }, GameAction::Equip { equipment_id, .. }) => {
            let p = *player;
            handle_equip_activation(state, p, equipment_id, &mut events)?
        }
        // CR 702.122a: Crew activation from Priority
        (WaitingFor::Priority { player }, GameAction::CrewVehicle { vehicle_id, .. }) => {
            let p = *player;
            handle_crew_activation(state, p, vehicle_id, &mut events)?
        }
        // CR 702.122a: Crew creature selection from CrewVehicle state
        (
            WaitingFor::CrewVehicle {
                player,
                vehicle_id,
                crew_power,
                eligible_creatures,
                ..
            },
            GameAction::CrewVehicle {
                vehicle_id: _vid,
                creature_ids,
            },
        ) => handle_crew_announcement(
            state,
            *player,
            *vehicle_id,
            *crew_power,
            eligible_creatures,
            &creature_ids,
            &mut events,
        )?,
        // CR 602.2b + CR 601.2h: crew's tap cost is not paid until the
        // activation payment step, so backing out before creature selection is
        // complete restores priority with no state to undo.
        (WaitingFor::CrewVehicle { player, .. }, GameAction::CancelCast) => {
            WaitingFor::Priority { player: *player }
        }
        // CR 702.184a: Station activation from Priority — enters target-selection state.
        (
            WaitingFor::Priority { player },
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        ) => {
            let p = *player;
            handle_station_activation(state, p, spacecraft_id, &mut events)?
        }
        // CR 702.184a: Station creature selection — resolves the ability.
        (
            WaitingFor::StationTarget {
                player,
                spacecraft_id,
                eligible_creatures,
            },
            GameAction::ActivateStation {
                spacecraft_id: _sid,
                creature_id: Some(cid),
            },
        ) => handle_station_announcement(
            state,
            *player,
            *spacecraft_id,
            eligible_creatures,
            cid,
            &mut events,
        )?,
        // CR 702.171a: Saddle activation from Priority — enters target-selection state.
        (WaitingFor::Priority { player }, GameAction::SaddleMount { mount_id, .. }) => {
            let p = *player;
            handle_saddle_activation(state, p, mount_id, &mut events)?
        }
        // CR 702.171a: Saddle creature selection — announces, pays cost, pushes stack entry.
        (
            WaitingFor::SaddleMount {
                player,
                mount_id,
                saddle_power,
                eligible_creatures,
                ..
            },
            GameAction::SaddleMount {
                mount_id: _mid,
                creature_ids,
            },
        ) => handle_saddle_announcement(
            state,
            *player,
            *mount_id,
            *saddle_power,
            eligible_creatures,
            &creature_ids,
            &mut events,
        )?,
        // CR 601.2c: no cost is paid until the saddle announcement, so backing out
        // restores priority with no state to undo.
        (WaitingFor::SaddleMount { player, .. }, GameAction::CancelCast) => {
            WaitingFor::Priority { player: *player }
        }
        (WaitingFor::Priority { player }, GameAction::Transform { object_id }) => {
            let p = *player;
            let obj = state
                .objects
                .get(&object_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::InvalidAction(
                    "Object is not on the battlefield".to_string(),
                ));
            }
            if obj.controller != p {
                return Err(EngineError::InvalidAction(
                    "You don't control this permanent".to_string(),
                ));
            }
            if obj.back_face.is_none() {
                return Err(EngineError::InvalidAction(
                    "Card has no back face".to_string(),
                ));
            }
            super::transform::transform_permanent(state, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        // CR 702.49: Ninjutsu-family activation during combat
        (
            WaitingFor::Priority { player },
            GameAction::ActivateNinjutsu {
                ninjutsu_object_id,
                creature_to_return,
            },
        ) => {
            let p = *player;
            super::keywords::activate_ninjutsu(
                state,
                p,
                ninjutsu_object_id,
                creature_to_return,
                &mut events,
            )
            .map_err(EngineError::InvalidAction)?;
            // CR 707.9 + CR 614.12a: battlefield entry may park on
            // `CopyTargetChoice` (enter-as-copy) or `ReplacementChoice` (optional
            // copy / CR 616.1 ordering); preserve the surfaced prompt instead of
            // clobbering it with Priority.
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                WaitingFor::Priority { player: p }
            } else {
                state.waiting_for.clone()
            }
        }
        // CR 702.190a: Sneak — cast a spell from hand during declare blockers
        // by paying the Sneak cost and returning an unblocked attacker.
        // Applies to any card type; permanent-spell placement (CR 702.190b)
        // is handled at resolution based on the variant's `placement`.
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellAsSneak {
                hand_object,
                card_id,
                creature_to_return,
                payment_mode,
            },
        ) => super::casting::handle_cast_spell_as_sneak_with_payment_mode(
            state,
            *player,
            hand_object,
            card_id,
            creature_to_return,
            payment_mode,
            &mut events,
        )?,
        // CR 702.188a: Web-slinging — cast a spell from hand by paying the
        // Web-slinging cost and returning a tapped creature you control.
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellAsWebSlinging {
                hand_object,
                card_id,
                creature_to_return,
                payment_mode,
            },
        ) => super::casting::handle_cast_spell_as_web_slinging_with_payment_mode(
            state,
            *player,
            hand_object,
            card_id,
            creature_to_return,
            payment_mode,
            &mut events,
        )?,
        // CR 601.2b + CR 118.9a: CastFromHandFree opt-in path — cast a hand
        // spell for free via a once-per-turn permission source (Zaffai).
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellForFree {
                object_id,
                card_id,
                source_id,
                payment_mode,
            },
        ) => super::casting::handle_cast_spell_for_free_with_payment_mode(
            state,
            *player,
            object_id,
            card_id,
            source_id,
            payment_mode,
            &mut events,
        )?,
        // CR 702.94a: Miracle reveal — accept path. The player reveals the card;
        // this creates a triggered ability ("When you reveal this card this way,
        // you may cast it for [miracle cost]") that goes on the stack. Opponents
        // can respond before the cast offer resolves.
        (
            WaitingFor::MiracleReveal {
                player,
                object_id,
                cost,
            },
            GameAction::CastSpellAsMiracle {
                object_id: action_obj,
                ..
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMiracle object_id does not match the outstanding miracle reveal"
                        .to_string(),
                ));
            }
            let p = *player;
            let source = *object_id;
            let miracle_cost = cost.clone();

            // CR 702.94a: Emit the reveal event.
            // CR 702.94a: Emit the reveal event.
            let card_name = state
                .objects
                .get(&source)
                .map(|o| o.name.clone())
                .unwrap_or_default();
            events.push(crate::types::events::GameEvent::CardsRevealed {
                player: p,
                card_ids: vec![source],
                card_names: vec![card_name],
            });

            // CR 702.94a: Push the miracle triggered ability onto the stack.
            // "When you reveal this card this way, you may cast it by paying
            // [miracle cost] rather than its mana cost."
            let ability = crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::MiracleCast { cost: miracle_cost },
                vec![],
                source,
                p,
            );
            let trigger = super::triggers::PendingTrigger {
                source_id: source,
                controller: p,
                condition: None,
                ability: Box::new(ability),
                timestamp: 0,
                target_constraints: vec![],
                distribute: None,
                trigger_event: None,
                modal: None,
                mode_abilities: vec![],
                description: Some("Miracle — you may cast this card".to_string()),
                may_trigger_origin: None,
                subject_match_count: None,
        die_result: None,
            };
            super::triggers::push_pending_trigger_to_stack(state, trigger, &mut events);

            // Return to priority so the trigger can be responded to.
            state.waiting_for = WaitingFor::Priority { player: p };
            super::engine_priority::run_post_action_pipeline(
                state,
                &mut events,
                &WaitingFor::Priority { player: p },
                true,
                false,
            )?
        }
        // CR 702.94a: Miracle reveal — decline path. Reuses the generic
        // DecideOptionalEffect decline; flushes the next pending miracle
        // offer or returns to Priority. Flip `waiting_for` out of MiracleReveal
        // before running the pipeline so its Priority-gated path (line 46 of
        // engine_priority) engages and the flush has a chance to pop the next
        // offer.
        (
            WaitingFor::MiracleReveal { player, .. },
            GameAction::DecideOptionalEffect { accept: false },
        ) => {
            let p = *player;
            state.waiting_for = WaitingFor::Priority { player: p };
            super::engine_priority::run_post_action_pipeline(
                state,
                &mut events,
                &WaitingFor::Priority { player: p },
                true,
                false,
            )?
        }
        // CR 702.94a + CR 608.2g: Miracle cast offer — the miracle triggered
        // ability has resolved. The player may now cast for the miracle cost.
        // This cast happens during trigger resolution, so timing restrictions
        // do not apply (CR 608.2g).
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Miracle { object_id, cost },
            },
            GameAction::CastSpellAsMiracle {
                object_id: action_obj,
                card_id,
                payment_mode,
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMiracle object_id does not match miracle cast offer".to_string(),
                ));
            }
            let p = *player;
            let obj = action_obj;
            // CR 702.94a + CR 608.2g: forward the cost latched at offer-enqueue as
            // the sole cost authority — live keywords are not re-read (the granting
            // source may have left the battlefield, CR 608.2b).
            let latched_cost = Some(cost.clone());
            super::casting::handle_cast_spell_as_miracle_with_payment_mode(
                state,
                p,
                obj,
                card_id,
                payment_mode,
                latched_cost,
                &mut events,
            )?
        }
        // CR 702.94a: Miracle cast offer — decline. Resume resolution.
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Miracle { .. },
            },
            GameAction::DecideOptionalEffect { accept: false },
        ) => {
            let p = *player;
            state.waiting_for = WaitingFor::Priority { player: p };
            super::engine_priority::run_post_action_pipeline(
                state,
                &mut events,
                &WaitingFor::Priority { player: p },
                true,
                false,
            )?
        }
        // CR 702.35a: Madness cast offer — the madness triggered ability has
        // resolved. The player may now cast the exiled card for its madness cost.
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Madness { object_id, .. },
            },
            GameAction::CastSpellAsMadness {
                object_id: action_obj,
                card_id,
                payment_mode,
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMadness object_id does not match madness cast offer".to_string(),
                ));
            }
            let p = *player;
            let obj = action_obj;
            super::casting::handle_cast_spell_as_madness_with_payment_mode(
                state,
                p,
                obj,
                card_id,
                payment_mode,
                &mut events,
            )?
        }
        // CR 702.35a: Madness decline — put the exiled card into its owner's graveyard.
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Madness { object_id, .. },
            },
            GameAction::DecideOptionalEffect { accept: false },
        ) => {
            let p = *player;
            let obj = *object_id;
            // CR 702.35a + CR 614.6: a declined madness card is put into its
            // owner's graveyard from exile — route it through the zone-change
            // pipeline so a `Moved` graveyard→exile redirect (Rest in Peace /
            // Leyline of the Void) fires on it. The raw `move_to_zone` never
            // proposed the inner ZoneChange, silently dropping those redirects.
            // The card moves itself (no external source), so it anchors its own
            // attribution. A CR 616.1 ordering choice (two simultaneous
            // redirects) is parked centrally by `move_object`; bail before
            // overwriting `waiting_for` / running the post-action pipeline so the
            // parked prompt is not clobbered (its resume runs the pipeline).
            match super::zone_pipeline::move_object(
                state,
                super::zone_pipeline::ZoneMoveRequest::effect(obj, Zone::Graveyard, obj),
                &mut events,
            ) {
                super::zone_pipeline::ZoneMoveResult::Done => {
                    state.waiting_for = WaitingFor::Priority { player: p };
                    super::engine_priority::run_post_action_pipeline(
                        state,
                        &mut events,
                        &WaitingFor::Priority { player: p },
                        true,
                        false,
                    )?
                }
                // The graveyard move paused on a CR 616.1 ordering choice; the
                // parked prompt is already in `state.waiting_for`. Evaluate the
                // arm to it (non-`Priority`), so the post-match block skips the
                // post-action pipeline and the prompt is surfaced intact — its
                // replacement-choice resume finishes the move and re-runs the
                // pipeline.
                super::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
                | super::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                    state.waiting_for.clone()
                }
            }
        }
        (waiting_for, action) if engine_resolution_choices::handles(waiting_for) => {
            match engine_resolution_choices::handle_resolution_choice(
                state,
                waiting_for.clone(),
                action,
                &mut events,
            )? {
                engine_resolution_choices::ResolutionChoiceOutcome::WaitingFor(waiting_for) => {
                    waiting_for
                }
                engine_resolution_choices::ResolutionChoiceOutcome::WaitingForWithInlineTriggers(
                    waiting_for,
                ) => {
                    triggers_processed_inline = true;
                    waiting_for
                }
                engine_resolution_choices::ResolutionChoiceOutcome::WaitingForWithParkedObservers(
                    waiting_for,
                ) => {
                    triggers_processed_inline = true;
                    skip_deferred_trigger_drain = true;
                    waiting_for
                }
                engine_resolution_choices::ResolutionChoiceOutcome::ActionResult(result) => {
                    return Ok(result);
                }
            }
        }
        (WaitingFor::Priority { player }, GameAction::PlayFaceDown { object_id, card_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            // Validate object_id matches card_id and is in hand
            let valid = state.objects.get(&object_id).is_some_and(|obj| {
                obj.card_id == card_id && obj.owner == p && obj.zone == Zone::Hand
            });
            if !valid {
                return Err(EngineError::InvalidAction(
                    "Card not found in hand".to_string(),
                ));
            }
            super::morph::play_face_down(state, p, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        (WaitingFor::Priority { player }, GameAction::TurnFaceUp { object_id, x }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            let announced_x = x;
            // CR 116.2b + CR 702.37e / CR 702.168d / CR 701.40b + CR 106.6: turning
            // a face-down permanent face up is a special action whose morph/disguise/
            // manifest cost must be paid *before* the flip. `turn_face_up_prepare`
            // validates the action and derives that cost; payment routes through
            // `PaymentContext::SpecialAction(TurnFaceUp)` so spend-restricted mana
            // ("only to turn permanents face up", Overgrown Zealot / Tin Street
            // Gossip) is eligible here while other-context mana is rejected. Mirrors
            // the `UnlockDoor` special-action handler.
            let cost = super::morph::turn_face_up_prepare(state, object_id, p)?;
            let mut cost = casting::apply_special_action_cost_reduction(
                state,
                p,
                crate::types::mana::SpecialAction::TurnFaceUp,
                cost,
            );

            // CR 107.3d: "If a cost associated with a special action, such as a suspend
            // cost or a morph cost, has an {X} ... in it, the value of X is chosen by the
            // player taking the special action immediately before they pay that cost."
            // The announcement happens HERE — inside the action, with no priority window
            // between choosing X and paying it, exactly as the rule describes.
            //
            // Warbreak Trumpeter (Morph {X}{X}{R}), Bane of the Living (Morph {X}{B}{B})
            // and Aurelia's Vindicator (Disguise {X}{3}{W}) are the live faces.
            let has_x = casting_costs::cost_has_x(&cost);
            if has_x {
                // CR 118.3: a player can't announce an X they cannot pay for. The cap is
                // computed with `object_id: None` deliberately — this is a SPECIAL ACTION,
                // not a cast, so cast-time cost modifiers and floors must not apply (the
                // special-action reduction was already applied above).
                let max_x = casting_costs::max_x_value(state, p, &cost, None);
                if announced_x > max_x {
                    return Err(EngineError::InvalidAction(format!(
                        "X={announced_x} exceeds the maximum payable value of {max_x} for this \
                         turn-face-up cost"
                    )));
                }
                // CR 107.1b + CR 601.2f: each `{X}` shard becomes `announced_x` generic, so
                // Warbreak Trumpeter's `{X}{X}{R}` costs 2X + {R}. Without this the X shards
                // reach mana payment unresolved and are dropped — the permanent flips for
                // its non-X remainder alone.
                cost.concretize_x(announced_x);
            } else if announced_x != 0 {
                // A cost with no {X} admits no choice: CR 107.3d only grants one "if a cost
                // ... has an {X} ... in it". Reject rather than silently ignore, so a client
                // bug cannot masquerade as a legal flip.
                return Err(EngineError::InvalidAction(
                    "This permanent's turn-face-up cost has no {X}, so X must be 0".to_string(),
                ));
            }
            casting::pay_special_action_mana_cost(
                state,
                p,
                Some(object_id),
                &cost,
                crate::types::mana::SpecialAction::TurnFaceUp,
                &mut events,
            )?;

            // CR 702.37f (morph) / CR 702.168e (disguise): "If a permanent's morph cost
            // includes X, other abilities of that permanent may also refer to X. The value
            // of X in those abilities is equal to the value of X chosen as the morph special
            // action was taken." Publish the announced X on the source-keyed carrier BEFORE
            // the flip emits `TurnedFaceUp`, so `triggers::build_triggered_ability` — the
            // single trigger-instantiation authority — stamps it onto the turn-face-up
            // trigger's `chosen_x`.
            //
            // The stamp must land at INSTANTIATION, not resolution: Aurelia's Vindicator
            // spends its X in `multi_target.max` ("exile up to X other target creatures"),
            // which is consumed during target selection, before the trigger ever resolves.
            //
            // Published only when the cost actually HAS an {X} (CR 107.3d grants a choice
            // only then). A no-X flip leaves the carrier untouched rather than clobbering it
            // with `Some((.., 0))`: an unrelated activated ability of ANOTHER object may be
            // on the stack with its own announced X in flight, and that value must survive.
            // The carrier is cleared at the start of the next `resolve_top`, so this
            // publication cannot outlive the trigger it is for.
            if has_x {
                state.announced_source_x = Some((object_id, announced_x));
            }

            super::morph::turn_face_up(state, p, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        (
            WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                target_constraints,
                ..
            },
            GameAction::SelectTargets { targets },
        ) => engine_stack::handle_trigger_target_selection_select_targets(
            state,
            *player,
            target_slots,
            target_constraints,
            targets,
            &mut events,
        )?,
        (WaitingFor::TriggerTargetSelection { .. }, GameAction::ChooseTarget { target }) => {
            let waiting_for = state.waiting_for.clone();
            engine_stack::handle_trigger_target_selection_choose_target(
                state,
                waiting_for,
                target,
                &mut events,
            )?
        }
        (
            WaitingFor::BetweenGamesSideboard { player, .. },
            GameAction::SubmitSideboard { main, sideboard },
        ) => match_flow::handle_submit_sideboard(state, *player, main, sideboard, &mut events)
            .map_err(EngineError::InvalidAction)?,
        (
            WaitingFor::BetweenGamesChoosePlayDraw { player, .. },
            GameAction::ChoosePlayDraw { play_first },
        ) => match_flow::handle_choose_play_draw(state, *player, play_first, &mut events)
            .map_err(EngineError::InvalidAction)?,
        (
            waiting_for @ WaitingFor::AbilityModeChoice { .. },
            GameAction::SelectModes { indices },
        ) => engine_modes::handle_ability_mode_choice(
            state,
            waiting_for.clone(),
            indices,
            &mut events,
        )?,
        // CR 602.2b + CR 601.2b: The controller chooses modes for an activated modal
        // ability BEFORE any cost is paid, target is chosen, or stack object is created
        // (those steps run later in engine_modes::handle_activated_mode_choice). At this
        // pre-commit sub-step nothing has changed in the game state, so cancelling is a
        // pure rollback to priority — mirroring the modal-spell (ModeChoice, CancelCast)
        // and (ChoosePermanentTypeSlot, CancelCast) arms.
        // CR 603.3c: A modal *triggered* ability's entry is already on the stack when the
        // mode prompt appears; its controller MUST choose a mode. This arm is guarded to
        // is_activated: true, so the triggered case falls through to the catch-all reject.
        (
            WaitingFor::AbilityModeChoice {
                player,
                is_activated: true,
                ..
            },
            GameAction::CancelCast,
        ) => WaitingFor::Priority { player: *player },
        // CR 601.2c: Player selected targets from a multi-target set ("any number of").
        (WaitingFor::MultiTargetSelection { .. }, GameAction::SelectCards { cards: selected }) => {
            let waiting_for = state.waiting_for.clone();
            engine_stack::handle_multi_target_selection(state, waiting_for, &selected, &mut events)?
        }
        // CR 702.139a: Pre-game companion reveal
        (
            WaitingFor::CompanionReveal { player, .. },
            GameAction::DeclareCompanion { choice },
        ) => super::companion::handle_declare_companion(state, *player, choice, &mut events)
            .map_err(EngineError::InvalidAction)?,
        // CR 702.139a: Special action — pay {3} to put companion into hand (see rule 116.2g).
        (WaitingFor::Priority { player }, GameAction::CompanionToHand) => {
            super::companion::handle_companion_to_hand(state, *player, &mut events)?
        }
        // CR 116.2c: Special action — pay a continuous effect's printed
        // termination cost to end it ("You may pay {W} to end this effect").
        // CR 116.1: special actions don't use the stack, so nothing is put on
        // the stack and no player gets a chance to respond.
        //
        // NO timing gate: CR 116.2c grants the action "any time they have
        // priority, unless that effect specifies another timing restriction",
        // and no card in the shipped class states one. This deliberately
        // diverges from `CompanionToHand` above, whose CR 116.2g DOES carry a
        // sorcery-speed restriction.
        (
            WaitingFor::Priority { player },
            GameAction::EndContinuousEffect { group, .. },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            super::end_continuous_effect::handle_end_continuous_effect(
                state,
                *player,
                group,
                &mut events,
            )?
        }
        // CR 722.3c / CR 601.2: Prepare (Strixhaven) — cast a copy of the
        // prepared face through the normal spell-casting pipeline (costs,
        // targeting, and mode choices all run through casting.rs single
        // authority). Assign when WotC publishes SOS CR update.
        (WaitingFor::Priority { player }, GameAction::CastPreparedCopy { source }) => {
            let p = *player;
            // Validate controller.
            let src = source;
            let Some(obj) = state.objects.get(&src) else {
                return Err(EngineError::InvalidAction(format!(
                    "CastPreparedCopy: source {src:?} not found"
                )));
            };
            if obj.controller != p {
                return Err(EngineError::InvalidAction(
                    "CastPreparedCopy: source not controlled by acting player".to_string(),
                ));
            }
            effects::prepare::cast_prepared_copy(state, src, p, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        // CR 702.xxx: Paradigm (Strixhaven) — accept the turn-based offer to
        // cast a copy of an exiled paradigm source. Assign when WotC
        // publishes SOS CR update.
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Paradigm { offers },
            },
            GameAction::CastParadigmCopy { source },
        ) => {
            let src = source;
            if !offers.contains(&src) {
                return Err(EngineError::InvalidAction(format!(
                    "CastParadigmCopy: source {src:?} not in current offer set"
                )));
            }
            let p = *player;
            let copy_id = effects::paradigm::cast_paradigm_copy(state, src, p, &mut events)
                .map_err(EngineError::InvalidAction)?;
            let remaining: Vec<ObjectId> = offers
                .iter()
                .copied()
                .filter(|id| *id != src)
                .collect();
            // CR 707.10c: If the paradigm spell has target slots, open target
            // selection via CopyRetarget. Otherwise re-offer any remaining
            // paradigm sources before returning to priority.
            if effects::prepare::open_copy_target_selection(
                state,
                copy_id,
                p,
                Some(remaining.clone()),
            )
            .map_err(EngineError::InvalidAction)?
            {
                state.waiting_for.clone()
            } else {
                effects::paradigm::waiting_after_remaining_offers(p, remaining)
            }
        }
        // CR 702.xxx: Paradigm (Strixhaven) — decline the turn-based offer.
        // Assign when WotC publishes SOS CR update.
        (
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Paradigm { .. },
            },
            GameAction::PassParadigmOffer,
        ) => WaitingFor::Priority { player: *player },
        (WaitingFor::Priority { player }, GameAction::SetAutoPass { mode }) => {
            if super::precast_copy_shortcut::blocks_pass(state, *player) {
                return Err(EngineError::ActionNotAllowed(
                    "A shortened pre-cast shortcut requires a different meaningful action before passing"
                        .to_string(),
                ));
            }
            // Convert request to stored mode, capturing engine state as needed.
            let stored_mode = match mode {
                AutoPassRequest::UntilStackEmpty => AutoPassMode::UntilStackEmpty {
                    initial_stack_len: state.stack.len(),
                },
                AutoPassRequest::UntilTurnBoundary { until } => {
                    AutoPassMode::UntilTurnBoundary { until }
                }
            };
            state.auto_pass.insert(*player, stored_mode);
            let wf = pass_priority_once_with_pipeline(state, &mut events, None)?;
            return Ok(ActionResult {
                events,
                waiting_for: wf,
                log_entries: vec![],
            });
        }
        // CR 701.34a: Proliferate — player selected targets to proliferate.
        (
            WaitingFor::ProliferateChoice { player, eligible },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let eligible_set = eligible.clone();
            // Validate all selected targets are in the eligible set.
            for t in &targets {
                if !eligible_set.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Selected target not eligible for proliferate".to_string(),
                    ));
                }
            }
            if !effects::proliferate::apply_proliferate(state, p, &targets, &mut events) {
                return Ok(ActionResult {
                    events,
                    waiting_for: state.waiting_for.clone(),
                    log_entries: vec![],
                });
            }
            // CR 701.34a: Emit player-action event so proliferate triggers fire.
            events.push(GameEvent::PlayerPerformedAction {
                player_id: p,
                action: PlayerActionKind::Proliferate,
                look_count: None,
                scry_bottom_count: None,
            });
            let pending = state
                .take_active_proliferate_frame()
                .map_err(|error| EngineError::InvalidAction(error.to_string()))?
                .ok_or_else(|| {
                    EngineError::InvalidAction("No active proliferate frame to resume".to_string())
                })?;
            let completion_source = pending.source_id;
            // FIX-1 (CR 701.34a): record the proliferate-target choice on the current loop-period
            // step so the object-growth detection drive replays the EXACT permanent(s) grown
            // (Pentad's charge) — never "all eligible", which could grow an opponent's
            // counters/poison and introduce a loss axis. Slot source = the trigger source (Kilo);
            // `index: 0` (distinct source from the Relic tap-cost/color pins).
            if let Some(source) = object_decision_source(state, completion_source) {
                let target_pins: Vec<crate::analysis::decision_template::TargetPin> = targets
                    .iter()
                    .filter_map(|t| match t {
                        crate::types::ability::TargetRef::Object(id) => object_decision_source(
                            state, *id,
                        )
                        .map(crate::analysis::decision_template::TargetPin::ByIdentity),
                        crate::types::ability::TargetRef::Player(pl) => {
                            Some(crate::analysis::decision_template::TargetPin::Player(*pl))
                        }
                    })
                    .collect();
                if !target_pins.is_empty() {
                    record_loop_pin(
                        state,
                        p,
                        crate::analysis::decision_template::PinnedDecision::Targets {
                            slot: crate::analysis::decision_template::DecisionSlot {
                                source,
                                index: 0,
                            },
                            targets: target_pins,
                        },
                    );
                }
            }
            if !effects::proliferate::resume_proliferate_actions(state, pending, &mut events) {
                return Ok(ActionResult {
                    events,
                    waiting_for: state.waiting_for.clone(),
                    log_entries: vec![],
                });
            }
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Proliferate,
                source_id: completion_source,
            subject: None,});
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            resume_pending_continuation_if_priority(state, &mut events)?;
            state.waiting_for.clone()
        }
        // CR 701.56a: Time travel — player selected objects for the current phase
        // (remove a time counter, then add). Validate against the eligible set,
        // apply the per-object counter change, then advance to the add phase or
        // finish. Counter changes drive the existing suspend/vanishing triggers.
        (
            WaitingFor::TimeTravelChoice {
                player,
                eligible,
                phase,
            },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let phase = *phase;
            let eligible_set = eligible.clone();
            for t in &targets {
                if !eligible_set.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Selected object not eligible for time travel".to_string(),
                    ));
                }
            }
            effects::time_travel::apply_phase(state, p, &targets, phase, &mut events);

            if phase == crate::types::game_state::TimeTravelPhase::Remove {
                // CR 701.56a: after the remove phase, offer the add phase over the
                // still-eligible objects, excluding any just chosen to remove.
                let add_eligible: Vec<_> = effects::time_travel::eligible_objects(state, p)
                    .into_iter()
                    .filter(|t| !targets.contains(t))
                    .collect();
                if !add_eligible.is_empty() {
                    state.waiting_for = WaitingFor::TimeTravelChoice {
                        player: p,
                        eligible: add_eligible,
                        phase: crate::types::game_state::TimeTravelPhase::Add,
                    };
                    state.waiting_for.clone()
                } else {
                    events.push(GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::TimeTravel,
                        source_id: ObjectId(0),
                    subject: None,});
                    state.waiting_for = WaitingFor::Priority { player: p };
                    state.priority_player = p;
                    resume_pending_continuation_if_priority(state, &mut events)?;
                    state.waiting_for.clone()
                }
            } else {
                events.push(GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::TimeTravel,
                    source_id: ObjectId(0),
                subject: None,});
                state.waiting_for = WaitingFor::Priority { player: p };
                state.priority_player = p;
                resume_pending_continuation_if_priority(state, &mut events)?;
                state.waiting_for.clone()
            }
        }
        // CR 608.2c: ChooseObjectsIntoTrackedSet — player submitted their
        // battlefield-permanent selection. Publish a fresh tracked set so the
        // downstream `PayCost { ScaledMana }` and the `IfYouDo`/`Untap` tail
        // resolve against exactly this selection, then resume the chain.
        (
            WaitingFor::ChooseObjectsSelection {
                player,
                eligible,
                trigger_event,
            },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let eligible_set = eligible.clone();
            let pending_event = trigger_event.clone();
            // Validate all selected targets are in the eligible set.
            for t in &targets {
                if !eligible_set.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Selected target not eligible for object selection".to_string(),
                    ));
                }
            }
            // Map TargetRef → ObjectId. The eligible set is all battlefield
            // permanents, so every selected target is an Object.
            let ids: Vec<ObjectId> = targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    TargetRef::Player(_) => None,
                })
                .collect();
            // CR 603.7: Always allocate a fresh tracked set — a player-chosen
            // "those creatures" set is a new resolution scope. An empty
            // selection yields an empty fresh set (size 0).
            effects::publish_fresh_tracked_set(state, ids);
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::ChooseObjectsIntoTrackedSet,
                source_id: ObjectId(0), // Source not tracked through choice state
                subject: None,
            });
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            // CR 608.2: restore the triggering event so the stashed
            // `PayCost { ScaledMana, payer: TriggeringPlayer }` continuation
            // resolves the payer correctly — the trigger's resolution is still
            // in flight.
            // CR 603.2c + CR 608.2: the batched-trigger subject count is also
            // part of the trigger's resolution scope — mirror its save/restore
            // so an `EventContextAmount` inside the resumed continuation reads
            // the original "that many" instead of `None`.
            let previous_trigger_event = state.current_trigger_event.clone();
            let previous_trigger_match_count = state.current_trigger_match_count;
            state.current_trigger_event = pending_event;
            state.current_trigger_match_count = state
                .active_ability_continuation()
                .and_then(|continuation| continuation.trigger_context.as_ref())
                .and_then(|context| context.match_count);
            resume_pending_continuation_if_priority(state, &mut events)?;
            state.current_trigger_event = previous_trigger_event;
            state.current_trigger_match_count = previous_trigger_match_count;
            state.waiting_for.clone()
        }
        // CR 707.10c: Copy retarget — player chose target for the current slot
        // via battlefield click. Advances slot-by-slot; finalizes on the last slot.
        (
            WaitingFor::CopyRetarget {
                player,
                copy_id,
                target_slots,
                effect_kind,
                effect_source_id,
                current_slot,
                paradigm_remaining_offers,
            },
            GameAction::ChooseTarget { target },
        ) => {
            let p = *player;
            let cid = *copy_id;
            let slot_idx = *current_slot;
            if let Some(ref t) = target {
                let slot = &target_slots[slot_idx];
                // CR 707.10c: A retarget choice must produce a legal target. Both
                // `prepare::open_copy_target_selection` and `copy_spell::resolve`
                // populate `legal_alternatives` from `build_target_slots`, so an
                // empty list means "no legal alternative exists" — the caller
                // must use `KeepAllCopyTargets` (or send `target: None`).
                if !slot.legal_alternatives.contains(t) {
                    return Err(EngineError::InvalidAction(format!(
                        "Target {t:?} not a legal alternative for copy slot {slot_idx}"
                    )));
                }
            } else if target_slots[slot_idx].current.is_none() {
                return Err(EngineError::InvalidAction(format!(
                    "Copy target slot {slot_idx} has no current target to keep"
                )));
            }
            let mut updated_slots = target_slots.clone();
            if let Some(t) = target {
                updated_slots[slot_idx].current = Some(t.clone());
            }
            let next_slot = slot_idx + 1;
            if next_slot < updated_slots.len() {
                state.waiting_for = WaitingFor::CopyRetarget {
                    player: p,
                    copy_id: cid,
                    target_slots: updated_slots,
                    effect_kind: *effect_kind,
                    effect_source_id: *effect_source_id,
                    current_slot: next_slot,
                    paradigm_remaining_offers: paradigm_remaining_offers.clone(),
                };
            } else {
                finalize_copy_retarget(
                    state,
                    p,
                    cid,
                    &updated_slots,
                    *effect_kind,
                    *effect_source_id,
                    &mut events,
                )?;
            }
            state.waiting_for.clone()
        }
        // CR 707.10c: "Keep Current Targets" — accept every remaining slot's
        // current value in one action. Equivalent to dispatching
        // `ChooseTarget { target: None }` for each remaining slot, but resolved
        // server-side so the UI doesn't pay N round-trips. The slot-by-slot
        // `ChooseTarget` path above remains the single authority for the
        // per-slot legality/advance semantics.
        (
            WaitingFor::CopyRetarget {
                player,
                copy_id,
                target_slots,
                effect_kind,
                effect_source_id,
                ..
            },
            GameAction::KeepAllCopyTargets,
        ) => {
            let p = *player;
            let cid = *copy_id;
            let slots = target_slots.clone();
            finalize_copy_retarget(
                state,
                p,
                cid,
                &slots,
                *effect_kind,
                *effect_source_id,
                &mut events,
            )?;
            state.waiting_for.clone()
        }
        // CR 510.1c/d: Combat damage assignment from attacker to blockers.
        (
            WaitingFor::AssignCombatDamage {
                player,
                attacker_id,
                total_damage,
                blockers,
                assignment_modes,
                trample,
                defending_player,
                attack_target,
                pw_loyalty,
                pw_controller,
            },
            GameAction::AssignCombatDamage {
                mode,
                assignments,
                trample_damage,
                controller_damage,
            },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_assign_combat_damage(
                state,
                *player,
                *attacker_id,
                *total_damage,
                blockers,
                assignment_modes,
                *trample,
                *defending_player,
                attack_target,
                *pw_loyalty,
                *pw_controller,
                mode,
                &assignments,
                trample_damage,
                controller_damage,
                &mut events,
            )?
        }
        // CR 510.1d + CR 702.22k: A banded blocker's combat damage is divided by
        // the active player among the attackers it blocks.
        (
            WaitingFor::AssignBlockerDamage {
                player,
                blocker_id,
                total_damage,
                attackers,
            },
            GameAction::AssignBlockerDamage { assignments },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_assign_blocker_damage(
                state,
                *player,
                *blocker_id,
                *total_damage,
                attackers,
                &assignments,
                &mut events,
            )?
        }
        // CR 601.2d: Distribute among targets (casting-time distribution).
        (WaitingFor::DistributeAmong { player, .. }, GameAction::CancelCast) => {
            ensure_assist_cancellation_is_allowed(state)?;
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)?
                }
                None => {
                    return Err(EngineError::InvalidAction(
                        "No pending cast to cancel during distribution".to_string(),
                    ));
                }
            }
        }
        (
            WaitingFor::DistributeAmong {
                player,
                total,
                targets,
                ..
            },
            GameAction::DistributeAmong { distribution },
        ) => {
            let p = *player;
            let expected_total = *total;

            // Validate: each target gets ≥ 1, and total matches.
            let actual_total: u32 = distribution.iter().map(|(_, a)| *a).sum();
            if actual_total != expected_total {
                return Err(EngineError::InvalidAction(format!(
                    "Distribution total {} != required {}",
                    actual_total, expected_total
                )));
            }
            for (t, amount) in &distribution {
                if *amount == 0 {
                    return Err(EngineError::InvalidAction(
                        "Each target must receive at least 1".to_string(),
                    ));
                }
                if !targets.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Distribution target not in legal set".to_string(),
                    ));
                }
            }

            // Store on the pending cast's resolved ability if we're mid-casting.
            // The distribution will be read during effect resolution.
            if let Some(pending) = state.pending_cast.as_mut() {
                pending.ability.distribution =
                    Some(distribution.iter().map(|(t, a)| (t.clone(), *a)).collect());
            }

            // CR 601.2d: Resume casting pipeline after distribution.
            if state.pending_cast.is_some() {
                let pending = state.pending_cast.take().unwrap();
                if pending.activation_ability_index.is_some() {
                    // CR 602.2b + CR 601.2d: an activated ability that divides
                    // damage among targets goes on the stack as an ActivatedAbility
                    // after the division is announced — not as a spell (Captain
                    // America's Throw). The payment boundary retains the original
                    // target-first root while it pays the residual mana leg.
                    // The spell-only cost-determination authority used in the `else`
                    // branch (`finish_pending_cast_cost_or_pay`) must NOT be reached
                    // here: it routes into `finalize_cast`, which would commit the
                    // source permanent to the stack as a spell.
                    casting_costs::finish_target_selected_activated_ability_at_payment_boundary(
                        state,
                        p,
                        *pending,
                        &mut events,
                    )?
                } else {
                    // CR 601.2c + CR 601.2d + CR 601.2f: Targets and their division are now
                    // committed, so the total cost — including any target-dependent
                    // surcharge (Strive, CR 207.2c) — is finally determinable. Route through
                    // the single cost-determination authority every other post-target-
                    // selection path uses (`casting_targets::handle_select_targets` /
                    // `handle_choose_target`) instead of calling `finalize_cast` directly
                    // with the stale cost that was locked in at `ChooseXValue` time, before
                    // targets (and hence any per-target surcharge) were known.
                    //
                    // CR 601.2h ("Unpayable costs can't be paid"): mirror
                    // `finalize_mana_payment`'s `pending_for_restore` pattern
                    // (casting_costs.rs ~8623-8627/8778-8787) — `finish_pending_cast_cost_or_pay`'s
                    // downstream chain has no restore-on-error wrapper of its own, and
                    // `state.pending_cast` is already `None` here (unlike
                    // `handle_select_targets`, whose `pending_cast` lives inside the
                    // `WaitingFor::TargetSelection` variant and so is never destructively
                    // taken). Without this clone-and-restore, a recomputed cost that turns
                    // out unpayable would return `Err` with `state.pending_cast` gone while
                    // `state.waiting_for` still reports `DistributeAmong` — a resubmitted
                    // `DistributeAmong` action would then fall through to the
                    // resolution-time continuation branch below instead of being cleanly
                    // rejected.
                    let pending_for_restore = pending.clone();
                    let ability = pending.ability.clone();
                    let cost = pending.cost.clone();
                    match casting_costs::finish_pending_cast_cost_or_pay(
                        state,
                        p,
                        *pending,
                        *ability,
                        cost,
                        &mut events,
                    ) {
                        Ok(waiting_for) => waiting_for,
                        Err(err) => {
                            state.pending_cast = Some(pending_for_restore);
                            return Err(err);
                        }
                    }
                }
            } else if let Some(mut pending_trigger) = state.pending_trigger.take() {
                // CR 601.2d + CR 603.3d: Triggered abilities divide effects
                // while being put on the stack. The chosen per-target amounts
                // are resolution data on the resolved ability. The entry is
                // already on the stack (pushed at distribute-among pause time);
                // mutate its ability with the distribution and clear
                // `pending_trigger_entry` so the resolver may now fire it.
                pending_trigger.ability.distribution =
                    Some(distribution.iter().map(|(t, a)| (t.clone(), *a)).collect());
                if !triggers::finalize_pending_trigger_entry(state, &pending_trigger.ability) {
                    // Unexpected dangling cursor: the entry is no longer on the
                    // stack. Recover per CR 608.2b / CR 800.4a (a stack object
                    // that has left the stack does not resolve) — record the
                    // diagnostic, abandon, and return priority instead of
                    // panicking (re-normalized next pass; CR 117.3b would give
                    // the active player).
                    triggers::abandon_ceased_pending_trigger(state, &pending_trigger.ability);
                    priority::clear_priority_passes(state);
                    WaitingFor::Priority { player: p }
                } else {
                    priority::clear_priority_passes(state);
                    // CR 113.2c + CR 603.2 + CR 603.3b: Drain siblings deferred
                    // behind this distribute-among trigger so each independent
                    // instance reaches the stack (issue #416).
                    debug_assert!(
                        !triggers::is_pending_trigger_construction_active(state),
                        "deferred-trigger drain entered with construction still active",
                    );
                    if let Some(waiting_for) =
                        triggers::drain_deferred_trigger_queue(state, &mut events)
                    {
                        waiting_for
                    } else {
                        WaitingFor::Priority { player: p }
                    }
                }
            } else {
                // Resolution-time distribution continuation path.
                state.waiting_for = WaitingFor::Priority { player: p };
                state.priority_player = p;
                resume_pending_continuation_if_priority(state, &mut events)?;
                state.waiting_for.clone()
            }
        }
        (
            WaitingFor::MoveCountersDistribution {
                player,
                source_id,
                available,
                destinations,
                pending_effect,
                ..
            },
            GameAction::ChooseCounterMoveDistribution { selections },
        ) => {
            let p = *player;
            effects::counters::validate_and_queue_counter_move_distribution(
                state,
                &selections,
                *source_id,
                available,
                destinations,
                pending_effect,
            )
            .map_err(|err| EngineError::InvalidAction(err.to_string()))?;
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            effects::counters::drain_pending_counter_moves(state, &mut events);
            resume_pending_continuation_if_priority(state, &mut events)?;
            state.waiting_for.clone()
        }
        // CR 107.1c + CR 608.2d: Submit the "remove any number of counters"
        // resolution-time selection (Rhys, the Evermore; Tetravus). ORDERING
        // INVARIANT: apply removals (stamping `last_effect_count`) BEFORE draining
        // the continuation, so a chained "create that many" rider reads the count.
        (
            WaitingFor::RemoveCountersChoice {
                player,
                source_id,
                available,
                pending_effect,
                ..
            },
            GameAction::ChooseCountersToRemove { selections },
        ) => {
            let p = *player;
            effects::counters::validate_and_queue_counter_removal(
                state,
                &selections,
                *source_id,
                available,
                pending_effect,
            )
            .map_err(|err| EngineError::InvalidAction(err.to_string()))?;
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            effects::counters::drain_pending_counter_removals(state, &mut events);
            resume_pending_continuation_if_priority(state, &mut events)?;
            state.waiting_for.clone()
        }
        // CR 115.7: Retarget a spell or ability on the stack via the dialog
        // path — the multi-target (`All`-scope) UI submits every new target at
        // once.
        (
            WaitingFor::RetargetChoice {
                player,
                stack_entry_index,
                scope,
                current_targets,
                legal_new_targets,
                ..
            },
            GameAction::RetargetSpell { new_targets },
        ) => apply_retarget(
            state,
            &mut events,
            RetargetSubmission {
                player: *player,
                stack_entry_index: *stack_entry_index,
                scope,
                current_targets,
                legal_new_targets,
                new_targets,
            },
        )?,
        // CR 115.7: Retarget a single-target spell via a board click. The
        // universal `ChooseTarget` action — already consumed by every other
        // targeting state — drives single-target retargets (Bolt Bend,
        // Redirect, Misdirection) so the player picks the new target directly
        // on the battlefield instead of through a dialog.
        (
            WaitingFor::RetargetChoice {
                player,
                stack_entry_index,
                scope: RetargetScope::Single,
                current_targets,
                legal_new_targets,
                ..
            },
            GameAction::ChooseTarget { target: Some(t) },
        ) => apply_retarget(
            state,
            &mut events,
            RetargetSubmission {
                player: *player,
                stack_entry_index: *stack_entry_index,
                scope: &RetargetScope::Single,
                current_targets,
                legal_new_targets,
                new_targets: vec![t],
            },
        )?,
        (waiting, action) => {
            return Err(EngineError::ActionNotAllowed(format!(
                "Cannot perform {:?} while waiting for {:?}",
                action, waiting
            )));
        }
    };

    // A shortened shortcut is discharged only by an action the normal reducer
    // accepted. In particular, a rejected cast/land attempt must leave the
    // CR 732.2c divergence requirement armed; preference actions returned
    // earlier and priority passes never reach this successful-reducer seam.
    super::precast_copy_shortcut::note_meaningful_action(
        state,
        semantic_actor,
        &action_for_divergence,
    );

    // Run post-action pipeline (SBAs, triggers, layers) and check for terminal states.
    // When triggers were already processed inline (e.g., DeclareAttackers, combat damage),
    // pass the flag to skip the trigger scan but still run SBAs, delayed triggers, and layers.
    if matches!(waiting_for, WaitingFor::Priority { .. }) {
        // Sync state.waiting_for before the pipeline so SBA/trigger checks see
        // the action's result, not the pre-action state (fixes stale TargetSelection
        // after CancelCast).
        state.waiting_for = waiting_for.clone();
        // CR 704.3 + CR 704.5f: a token battlefield entry postponed by an as-enters choice is
        // realized HERE, before the pipeline below, so the CR 400.7 row is written ahead of that
        // pipeline's SBA pass and survives a copy that enters with 0 toughness. It also puts the
        // entry pair into this action's `events` ahead of the CR 603.2 / CR 603.6a scan — no longer
        // the ONLY way that check runs (the action-boundary convergence in
        // `apply_action_boundary_core` runs the same pipeline for direct-return handlers), but
        // still the only placement that beats the SBA pass. Same gate as that boundary call, one
        // authority; keeping it here also avoids two full pipeline passes per settling action.
        effects::token::realize_settled_token_battlefield_entry(state, &mut events);
        let wf = engine_priority::run_post_action_pipeline(
            state,
            &mut events,
            &waiting_for,
            triggers_processed_inline,
            skip_deferred_trigger_drain,
        )?;
        state.waiting_for = wf.clone();
        return Ok(ActionResult {
            events,
            waiting_for: wf,
            log_entries: vec![],
        });
    }

    // CR 603.2 + CR 603.3b + CR 608.2g: a cast made during an unresolved
    // effect can leave the reducer at that effect's next choice (not Priority).
    // Park its SpellCast observers now; they are drained only when the parent
    // resolution reaches a genuine priority boundary.
    if let Some(waiting_for) =
        engine_resolution_choices::park_cast_during_resolution_cast_observers(
            state,
            &mut events,
            0,
            &waiting_for,
        )?
    {
        state.waiting_for = waiting_for.clone();
        return Ok(ActionResult {
            events,
            waiting_for,
            log_entries: vec![],
        });
    }

    // CR 704.3 / CR 800.4: SBAs may have ended the game during phase auto-advance (e.g.,
    // combat damage step) before we reach this point. state.waiting_for is the authoritative
    // result — written directly by eliminate_player → check_game_over. Guard against
    // overwriting it with the computed `waiting_for` from auto_advance.
    if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        match_flow::handle_game_over_transition(state);
        let wf = state.waiting_for.clone();
        return Ok(ActionResult {
            events,
            waiting_for: wf,
            log_entries: vec![],
        });
    }

    state.waiting_for = waiting_for.clone();

    Ok(ActionResult {
        events,
        waiting_for,
        log_entries: vec![],
    })
}

struct RetargetSubmission<'a> {
    player: PlayerId,
    stack_entry_index: usize,
    scope: &'a RetargetScope,
    current_targets: &'a [TargetRef],
    legal_new_targets: &'a [TargetRef],
    new_targets: Vec<TargetRef>,
}

/// CR 115.7d: Apply a validated retarget to the stack entry, then hand priority
/// back to the retargeting player. Single authority for both retarget entry
/// points — the board-click (`ChooseTarget`) and dialog (`RetargetSpell`) paths
/// — so target validation and stack mutation can never drift apart.
fn apply_retarget(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    submission: RetargetSubmission<'_>,
) -> Result<WaitingFor, EngineError> {
    let RetargetSubmission {
        player,
        stack_entry_index,
        scope,
        current_targets,
        legal_new_targets,
        new_targets,
    } = submission;

    match scope {
        RetargetScope::Single => {
            if new_targets.len() != 1 {
                return Err(EngineError::InvalidAction(
                    "Retarget: single-target change requires exactly one target".to_string(),
                ));
            }
            if !legal_new_targets.contains(&new_targets[0]) {
                return Err(EngineError::InvalidAction(
                    "Retarget: chosen target not in legal alternatives".to_string(),
                ));
            }
        }
        RetargetScope::All => {
            if new_targets.len() != current_targets.len() {
                return Err(EngineError::InvalidAction(
                    "Retarget: choose-new-targets submission must preserve target count"
                        .to_string(),
                ));
            }
            // CR 115.7d: For "choose new targets", unchanged targets may remain
            // unchanged even if they are no longer legal. Changed targets still
            // must be legal alternatives.
            for (idx, target) in new_targets.iter().enumerate() {
                if current_targets.get(idx) == Some(target) {
                    continue;
                }
                if !legal_new_targets.contains(target) {
                    return Err(EngineError::InvalidAction(
                        "Retarget: chosen target not in legal alternatives".to_string(),
                    ));
                }
            }
        }
        RetargetScope::ForcedTo(_) => {
            return Err(EngineError::InvalidAction(
                "Retarget: forced retarget is not interactive".to_string(),
            ));
        }
    }

    // CR 115.7a: "each target can be changed only to another legal target." The
    // `legal_new_targets` pool checked above is flat, so for a multi-slot node it
    // cannot tell slot 0's legal set from slot 1's. Re-check positionally against
    // the node's own per-slot filters before mutating the stack. Applies to both
    // `Single` and `All`. It is NOT a blanket no-op for `Single`: alongside the
    // two-surfaced-slot `Both`, `mana_multi_role` also admits the context-ref
    // recipient `Both` (surfaced == 1, generic == 0), which is parser-reachable
    // ("That player adds {R} for each card in target opponent's hand"). A
    // `Single`-scope retarget (Bolt Bend, Redirect) of that shape therefore does
    // run this per-slot validation — CR 115.7a-correct, and the reason the check
    // is wired for both scopes rather than only `All`.
    if let Some(ability) = state
        .stack
        .get(stack_entry_index)
        .and_then(|entry| entry.ability())
    {
        if let Some(slot) =
            crate::game::ability_utils::retarget_slot_violation(state, ability, &new_targets)
        {
            return Err(EngineError::InvalidAction(format!(
                "Retarget: chosen target is not legal for target slot {slot}"
            )));
        }
    }

    if stack_entry_index < state.stack.len() {
        if let Some(ability) = state.stack[stack_entry_index].ability_mut() {
            ability.targets = new_targets;
        }
    } else {
        return Err(EngineError::InvalidAction(
            "Invalid stack entry index for retargeting".to_string(),
        ));
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChangeTargets,
        source_id: state
            .stack
            .get(stack_entry_index)
            .map(|e| e.source_id)
            .unwrap_or(ObjectId(0)),
        subject: None,
    });
    state.waiting_for = WaitingFor::Priority { player };
    state.priority_player = player;
    resume_pending_continuation_if_priority(state, events)?;
    Ok(state.waiting_for.clone())
}

/// CR 603.3c + CR 608.2c: Drop a mid-construction optional triggered modal that
/// was declined before mode choice.
pub(super) fn drop_mid_construction_pending_trigger(state: &mut GameState) {
    super::stack::pop_uncommitted_pending_trigger_entry(
        state,
        super::lifecycle::DelayedTerminalDisposition::NoLegalChoice,
    );
    state.pending_trigger = None;
    state.pending_trigger_firing = None;
}

/// Clear optionality after the controller accepts a "you may choose N" gate so
/// mode choice can proceed and resolution does not re-prompt.
pub(super) fn clear_pending_trigger_optional(state: &mut GameState) {
    if let Some(trigger) = state.pending_trigger.as_mut() {
        trigger.ability.optional = false;
    }
    if let Some(entry_id) = state.pending_trigger_entry {
        if let Some(entry) = state.stack.iter_mut().find(|e| e.id == entry_id) {
            if let Some(ability) = entry.ability_mut() {
                ability.optional = false;
            }
        }
    }
}

/// Run state-based actions, exile returns, delayed triggers, and trigger processing
/// after an action that produced `WaitingFor::Priority`. Returns the resulting
/// `WaitingFor` state — may be terminal (GameOver, interactive choice) or
/// a continuation (Priority for next player/active player).
///
/// `default_wf` is the WaitingFor computed by the action handler, used as fallback
/// when no terminal/trigger/SBA outcome overrides it.
///
/// `skip_trigger_scan` — when `true`, skips the `process_triggers` call because
/// triggers were already processed inline (e.g., combat damage, declare attackers).
/// SBAs, exile returns, delayed triggers, and layer evaluation still run.
pub(super) fn begin_pending_trigger_target_selection(
    state: &mut GameState,
) -> Result<Option<WaitingFor>, EngineError> {
    let Some(trigger) = state.pending_trigger.as_ref() else {
        return Ok(None);
    };

    // CR 700.2b: Modal trigger — prompt for mode selection before stack.
    if let Some(ref modal) = trigger.modal {
        if !trigger.mode_abilities.is_empty() {
            let player = trigger.controller;
            let source_id = trigger.source_id;
            let mode_abilities = trigger.mode_abilities.clone();
            let trigger_event = trigger.trigger_event.clone();
            // Clone optional-gate fields before any `&mut state` borrow so the
            // `pending_trigger` imm borrow from `trigger` does not overlap.
            let ability_optional = trigger.ability.optional;
            let may_trigger_origin = trigger.may_trigger_origin.clone();
            let trigger_description = trigger.description.clone();
            let trigger_events = if state.pending_trigger_event_batch.is_empty() {
                trigger_event.iter().cloned().collect::<Vec<_>>()
            } else {
                state.pending_trigger_event_batch.clone()
            };
            let subject_match_count = trigger.subject_match_count;
            let modal = modal.clone();
            // CR 603.3c + CR 603.3d: a triggered modal's mode choice is announced as
            // the ability is put on the stack, by the same process as casting a spell
            // (CR 601.2c-d). The triggering event must be live for the ENTIRE choice,
            // including the "choose up to X" dynamic cap resolved by
            // modal_choice_for_player -- push the event window BEFORE cap resolution,
            // not just around target-legality filtering, so event-context quantity
            // refs (e.g. EventContextSourceModesChosen, Riku of Many Paths) resolve
            // against the triggering spell rather than an unset event.
            let context_snapshot = super::triggers::push_trigger_event_context(
                state,
                trigger_event.as_ref(),
                &trigger_events,
                subject_match_count,
            );
            let modal = modal_choice_for_player(
                state,
                player,
                source_id,
                &modal,
                &crate::types::ability::SpellContext::default(),
            );
            let mut unavailable_modes = compute_unavailable_modes(state, source_id, &modal);
            super::ability_utils::filter_modes_by_target_legality(
                state,
                source_id,
                player,
                &mode_abilities,
                &modal,
                &mut unavailable_modes,
            );
            super::triggers::restore_trigger_event_context(state, context_snapshot);
            let Some(modal) = super::ability_utils::modal_choice_with_target_assignment_limit(
                state,
                source_id,
                player,
                &modal,
                &mode_abilities,
                &unavailable_modes,
            ) else {
                super::stack::pop_uncommitted_pending_trigger_entry(
                    state,
                    super::lifecycle::DelayedTerminalDisposition::NoLegalChoice,
                );
                state.pending_trigger = None;
                state.pending_trigger_firing = None;
                return Ok(None);
            };

            // CR 700.2b (override) + CR 701.9b (analogous): "choose ... at
            // random" modal triggers (Cult of Skaro) are resolved inline by
            // `dispatch_pending_trigger_context` via `state.rng` — they clear
            // `modal` before this re-entry surfaces a `WaitingFor`, so reaching
            // here with a `Random` selection means the dispatcher was bypassed.
            // This router cannot thread `events` into the random resolver, so
            // emitting `AbilityModeChoice` would (wrongly) prompt the controller.
            // Drop the trigger defensively instead of prompting incorrectly.
            debug_assert!(
                !modal.selection.is_random(),
                "random modal trigger reached begin_pending_trigger_target_selection; \
                 dispatch_pending_trigger_context must resolve it inline",
            );
            if modal.selection.is_random() {
                super::stack::pop_uncommitted_pending_trigger_entry(
                    state,
                    super::lifecycle::DelayedTerminalDisposition::NoLegalChoice,
                );
                state.pending_trigger = None;
                state.pending_trigger_firing = None;
                return Ok(None);
            }

            // CR 700.2b + CR 603.3c: All modes unavailable (previously chosen
            // OR no legal targets) — ability cannot remain on the stack.
            // Under the "push first, choose second" contract, the entry may
            // already have been pushed by `dispatch_pending_trigger_context`;
            // remove it before clearing the cursor. The new flow filters this
            // case BEFORE pushing in the modal branch, so this is normally a
            // dead branch — kept as a defensive cleanup for any
            // delayed-revalidation paths.
            if unavailable_modes.len() >= modal.mode_count {
                super::stack::pop_uncommitted_pending_trigger_entry(
                    state,
                    super::lifecycle::DelayedTerminalDisposition::NoLegalChoice,
                );
                state.pending_trigger = None;
                state.pending_trigger_firing = None;
                return Ok(None);
            }

            // CR 608.2c: "you may choose N" (Shadrix Silverquill) — modes are
            // chosen as the triggered ability is put on the stack (CR 700.2b +
            // CR 603.3d). Offer the decline first so accepting still requires
            // exactly `min_choices` modes; declining removes the mid-construction
            // stack entry without choosing zero modes (count stays fixed).
            if ability_optional {
                let may_trigger_key = may_trigger_origin.map(|origin| MayTriggerAutoChoiceKey {
                    player,
                    source_id,
                    origin,
                });
                if let Some(ref key) = may_trigger_key {
                    if let Some(choice) = state.may_trigger_auto_choice(key) {
                        match choice {
                            AutoMayChoice::Decline => {
                                drop_mid_construction_pending_trigger(state);
                                return Ok(None);
                            }
                            AutoMayChoice::Accept => {
                                clear_pending_trigger_optional(state);
                                return Ok(Some(WaitingFor::AbilityModeChoice {
                                    player,
                                    modal,
                                    source_id,
                                    mode_abilities,
                                    is_activated: false,
                                    ability_index: None,
                                    ability_cost: None,
                                    unavailable_modes,
                                }));
                            }
                        }
                    }
                }
                return Ok(Some(WaitingFor::OptionalEffectChoice {
                    player,
                    source_id,
                    description: trigger_description,
                    may_trigger_key,
                }));
            }

            return Ok(Some(WaitingFor::AbilityModeChoice {
                player,
                modal,
                source_id,
                mode_abilities,
                is_activated: false,
                ability_index: None,
                ability_cost: None,
                unavailable_modes,
            }));
        }
    }

    let ability = trigger.ability.clone();
    // CR 601.2c + CR 603.3d + CR 109.5: a targeted "of their choice" trigger routes
    // target selection to the scoped (upkeep) player, not the source's controller.
    let player = ability
        .target_chooser
        .as_ref()
        .and_then(|f| crate::game::targeting::resolve_effect_player_ref(state, &ability, f))
        .unwrap_or(trigger.controller);
    let source_id = trigger.source_id;
    let target_constraints = trigger.target_constraints.clone();
    let description = trigger.description.clone();
    let trigger_controller = trigger.controller;
    let trigger_event = trigger.trigger_event.clone();
    let trigger_events = if state.pending_trigger_event_batch.is_empty() {
        trigger_event.iter().cloned().collect::<Vec<_>>()
    } else {
        state.pending_trigger_event_batch.clone()
    };
    let subject_match_count = trigger.subject_match_count;
    let context_snapshot = super::triggers::push_trigger_event_context(
        state,
        trigger_event.as_ref(),
        &trigger_events,
        subject_match_count,
    );
    // CR 603.3d: "If a choice is required when the triggered ability goes on the
    // stack but no legal choices can be made for it ... the ability is simply
    // removed from the stack." `build_target_slots` returns `Err` ONLY to report
    // exactly that — every error site in `collect_target_slots` is a
    // `No legal targets available` `ActionNotAllowed`. A targeted trigger's
    // targets can be legal at "push first" dispatch yet become illegal here at
    // "choose second" when an effect earlier in the SAME simultaneous cascade
    // removed the only legal target (e.g. the artifact a Schema Thief token would
    // copy was destroyed by a damage trigger that resolved first). Map that to
    // the no-prompt drop path below — never propagate it and abort the in-flight
    // action, which would leave the game unable to pass priority (a soft-lock
    // freeze). Errors from `begin_target_selection_for_ability` are genuine
    // selection-invariant violations and MUST still propagate (via `?` below).
    let selection_result = match build_target_slots(state, &ability) {
        Ok(target_slots) if !target_slots.is_empty() => {
            begin_target_selection_for_ability(state, &ability, &target_slots, &target_constraints)
                .map(|selection| Some((target_slots, selection)))
        }
        // Empty target slots (no targeting), or CR 603.3d no-legal-target: no
        // prompt is needed/possible — fall through to the removal branch.
        Ok(_) | Err(_) => Ok(None),
    };
    super::triggers::restore_trigger_event_context(state, context_snapshot);
    let Some((target_slots, selection)) = selection_result? else {
        // CR 603.3d: No target prompt is required — empty target slots, or
        // `build_target_slots` reported no legal target at choose-time (mapped to
        // `Ok(None)` above). Symmetric to the modal `all-modes-unavailable`
        // branch above: if the "push first" dispatcher already pushed an
        // in-construction entry for this trigger, pop it before clearing the
        // cursor.
        super::stack::pop_uncommitted_pending_trigger_entry(
            state,
            super::lifecycle::DelayedTerminalDisposition::NoLegalChoice,
        );
        state.pending_trigger = None;
        state.pending_trigger_firing = None;
        return Ok(None);
    };
    Ok(Some(WaitingFor::TriggerTargetSelection {
        player,
        trigger_controller: Some(trigger_controller),
        trigger_event,
        trigger_events,
        target_slots,
        mode_labels: Vec::new(),
        target_constraints,
        selection,
        source_id: Some(source_id),
        description,
    }))
}

/// CR 604.2 + CR 110.4: If a land was played from the graveyard via a
/// frequency-bounded permission source, record the appropriate per-turn slot
/// as used to prevent a second play/cast from the same source/slot this turn.
///
/// - `OncePerTurn` (Crucible-of-Worlds-class): record the source in
///   `graveyard_cast_permissions_used`.
/// - `OncePerTurnPerPermanentType` (Muldrotha-class): record the
///   `(source, slot_type)` pair in `graveyard_cast_permissions_used_per_type`.
///   The slot is picked here (not stashed beforehand) because lands take the
///   non-stack play-land path; the picker reads the live used-set so concurrent
///   frequency-bounded permissions are handled correctly.
/// - `Unlimited` (Crucible-of-Worlds-with-no-rider): no tracking.
fn record_graveyard_play_permission(
    state: &mut GameState,
    source: Option<ObjectId>,
    played_object: ObjectId,
) {
    let Some(source_id) = source else {
        return;
    };
    let Some(obj) = state.objects.get(&source_id) else {
        return;
    };
    let frequency =
        super::functioning_abilities::active_static_definitions(state, obj).find_map(|s| {
            match s.mode {
                StaticMode::GraveyardCastPermission { frequency, .. } => Some(frequency),
                _ => None,
            }
        });
    match frequency {
        Some(crate::types::statics::CastFrequency::OncePerTurn) => {
            crate::game::ledger::consume_once_per_turn_permission(
                state,
                source_id,
                crate::types::resolved_commands::ResolvedOncePerTurnPermission::GraveyardCast,
            )
            .expect("graveyard play permission must have an unused ledger slot");
        }
        Some(crate::types::statics::CastFrequency::OncePerTurnPerPermanentType) => {
            // CR 110.4: Use the player-chosen slot if one was stashed by the
            // ChoosePermanentTypeSlot dispatch (multi-type card). Otherwise
            // auto-pick (single-type card).
            let slot = state
                .pending_permanent_type_slot
                .take()
                .filter(|(src, _)| *src == source_id)
                .map(|(_, ct)| ct)
                .or_else(|| {
                    super::casting::pick_per_permanent_type_slot(state, source_id, played_object)
                });
            if let Some(slot) = slot {
                crate::game::ledger::consume_once_per_turn_permission(
                    state,
                    source_id,
                    crate::types::resolved_commands::ResolvedOncePerTurnPermission::GraveyardCastPermanentType {
                        permanent_type: slot,
                    },
                )
                .expect("graveyard permanent-type play slot must be unused");
            }
        }
        Some(crate::types::statics::CastFrequency::Unlimited) | None => {
            // Unlimited (Crucible of Worlds) or no permission: no tracking.
        }
    }
}

fn record_exile_play_permission(
    state: &mut GameState,
    authorization: Option<casting::ExileLandPlayAuthorization>,
) {
    match authorization {
        Some(casting::ExileLandPlayAuthorization::ObjectAttached {
            source,
            frequency: CastFrequency::OncePerTurn,
        }) => crate::game::ledger::consume_once_per_turn_permission(
            state,
            source,
            ResolvedOncePerTurnPermission::ExilePlay,
        )
        .expect("object-attached exile play permission must have an unused ledger slot"),
        Some(casting::ExileLandPlayAuthorization::Static {
            source,
            frequency: CastFrequency::OncePerTurn | CastFrequency::OncePerTurnPerPermanentType,
        }) => crate::game::ledger::consume_once_per_turn_permission(
            state,
            source,
            ResolvedOncePerTurnPermission::ExileCast,
        )
        .expect("static exile play permission must have an unused ledger slot"),
        Some(casting::ExileLandPlayAuthorization::ObjectAttached {
            frequency: CastFrequency::Unlimited | CastFrequency::OncePerTurnPerPermanentType,
            ..
        })
        | Some(casting::ExileLandPlayAuthorization::Static {
            frequency: CastFrequency::Unlimited,
            ..
        })
        | None => {}
    }
}

/// CR 305.1 + CR 116.2a + CR 401.5: Consume the per-turn slot when a
/// `OncePerTurn` `TopOfLibraryCastPermission { play_mode: Play }` authorizes a
/// land play from the library. Playing a land is a special action (CR 305.1,
/// CR 116.2a) — not a spell cast — so CR 601.2a does not apply here; CR 401.5
/// governs top-of-library visibility during the special action. Receives the
/// pre-captured `(src_id, frequency)` that was resolved BEFORE the zone change
/// — `top_of_library_permission_source` reads `library.front()`, which no
/// longer points to the played land after the land is delivered to the
/// battlefield. `Unlimited` permissions (Future Sight, Bolas's Citadel) do not
/// spend a slot.
fn record_top_of_library_land_permission(
    state: &mut GameState,
    src_id: ObjectId,
    frequency: crate::types::statics::CastFrequency,
) {
    if matches!(frequency, crate::types::statics::CastFrequency::OncePerTurn) {
        crate::game::ledger::consume_once_per_turn_permission(
            state,
            src_id,
            crate::types::resolved_commands::ResolvedOncePerTurnPermission::TopOfLibraryCast,
        )
        .expect("top-of-library play permission must have an unused ledger slot");
    }
}

/// CR 305.1 + CR 116.2a: Finalize a land play once its zone change has
/// committed. A paused delivery tail may still require a choice, but the land
/// is already on the battlefield and no continuation retains the selected play
/// authority, so per-play accounting must happen at this seam.
#[allow(clippy::too_many_arguments)]
fn finalize_committed_land_play(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    origin_zone: Zone,
    graveyard_permission_source: Option<ObjectId>,
    exile_play_authorization: Option<casting::ExileLandPlayAuthorization>,
    library_permission_source: Option<(ObjectId, CastFrequency)>,
    events: &mut Vec<GameEvent>,
) {
    state.lands_played_this_turn += 1;
    record_land_played_from_zone(state, player, object_id, origin_zone);
    record_graveyard_play_permission(state, graveyard_permission_source, object_id);
    record_exile_play_permission(state, exile_play_authorization);
    if let Some((source_id, frequency)) = library_permission_source {
        record_top_of_library_land_permission(state, source_id, frequency);
    }
    let player_data = state
        .players
        .iter_mut()
        .find(|candidate| candidate.id == player)
        .expect("priority player exists");
    player_data.lands_played_this_turn += 1;
    priority::clear_priority_passes(state);
    events.push(GameEvent::LandPlayed {
        object_id,
        player_id: player,
        from_zone: origin_zone,
    });
}

fn mark_land_played_from_zone(state: &mut GameState, object_id: ObjectId, zone: Zone) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.played_from_zone = Some(zone);
    }
}

fn record_land_played_from_zone(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    zone: Zone,
) {
    mark_land_played_from_zone(state, object_id, zone);
    state
        .lands_played_this_turn_by_player
        .entry(player)
        .or_default()
        .push_back(LandPlayRecord { from_zone: zone });
}

fn handle_play_land(
    state: &mut GameState,
    acting_player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate main phase
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Can only play lands during main phases".to_string(),
            ));
        }
    }
    // CR 305.1 + CR 116.2a: A land play is a special action only during a
    // main phase of the player's turn while the stack is empty.
    if !state.stack.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "Can only play lands while the stack is empty".to_string(),
        ));
    }

    // CR 305.2 + CR 505.6b: Validate land limit.
    // Base limit is max_lands_per_turn (normally 1), plus any additional drops
    // from static abilities like Exploration or Azusa.
    //
    // CR 805.4c: "Each player on a team may play a land during each of that
    // team's turns" — under the shared team turns option, the nonactive
    // teammate plays from their OWN hand against their OWN once-per-turn
    // allowance, not the turn's nominal resource owner (`active_player`).
    // `turn_resource_owner` stays correct for turn-control effects (CR 723,
    // e.g. Mindslaver), which always act on the active player's own
    // resources regardless of who submits the choice — that path is
    // unaffected since it never uses shared team turns.
    let player = if state.format_config.topology().has_shared_team_turns() {
        if !super::topology::team_members(state, state.active_player).contains(&acting_player) {
            return Err(EngineError::ActionNotAllowed(
                "Only the active team may play lands during its turn".to_string(),
            ));
        }
        acting_player
    } else {
        turn_control::turn_resource_owner(state)
    };
    // CR 305.2: "Can't play lands" suppresses the play-land special action outright.
    if super::static_abilities::player_has_static_other(state, player, "CantPlayLand") {
        return Err(EngineError::ActionNotAllowed(
            "Player is under a CantPlayLand static (CR 305.2)".to_string(),
        ));
    }
    // CR 116.2a + CR 305.1: The shared restriction gate covers both temporary
    // play-from-zone and per-land prohibitions for every legal source zone.
    if let Some(obj) = state.objects.get(&object_id) {
        if !super::casting::land_play_is_permitted_by_restrictions(state, player, obj) {
            return Err(EngineError::ActionNotAllowed(
                "A temporary effect prevents playing this land (CR 305.1)".to_string(),
            ));
        }
    }
    let additional = super::static_abilities::additional_land_drops(state, player);
    let effective_limit = state.max_lands_per_turn.saturating_add(additional);
    // CR 805.4c: per-player land count under team turns (each teammate has
    // their own allowance); the legacy single-counter `lands_played_this_turn`
    // is correct outside team-based formats, where only the active player
    // ever plays lands during their own turn.
    let lands_played = if state.format_config.topology().has_shared_team_turns() {
        state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.lands_played_this_turn)
            .unwrap_or(0)
    } else {
        state.lands_played_this_turn
    };
    if lands_played >= effective_limit {
        return Err(EngineError::ActionNotAllowed(
            "Already played maximum lands this turn".to_string(),
        ));
    }

    // Validate that object_id exists in hand or graveyard (with permission)
    // or on top of library (with TopOfLibraryCastPermission { play_mode: Play })
    // and matches card_id.
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("priority player exists");
    let in_hand = player_data.hand.contains(&object_id);
    // CR 305.1 + CR 604.2: Check graveyard for play-from-graveyard permission
    // CR 604.2: Find graveyard play permission source (if any) for once-per-turn tracking.
    let gy_permission_source = if player_data.graveyard.contains(&object_id) {
        super::casting::graveyard_lands_playable_by_permission(state, player)
            .iter()
            .find(|(obj_id, _)| *obj_id == object_id)
            .map(|(_, source_id)| *source_id)
    } else {
        None
    };
    let in_graveyard_with_permission = gy_permission_source.is_some();

    // CR 401.5 + CR 305.1: Check top of library for
    // `TopOfLibraryCastPermission { play_mode: Play }` (Future Sight,
    // Bolas's Citadel, Magus of the Future, The Fourth Doctor).
    //
    // IMPORTANT: capture (src_id, frequency) HERE — before the zone change.
    // `top_of_library_permission_source` reads `library.front()`, which will
    // point to the next card once the land is delivered to the battlefield.
    // Recording in the post-delivery epilogue would always see the wrong top
    // card and silently skip the once-per-turn slot, allowing a OncePerTurn
    // permission to be reused indefinitely. CR 305.1 + CR 116.2a + CR 401.5:
    // land play is a special action, not a spell cast (CR 601.2a does not apply).
    let library_permission_src: Option<(ObjectId, crate::types::statics::CastFrequency)> =
        super::casting::top_of_library_permission_source(
            state,
            player,
            Some(crate::types::ability::CardPlayMode::Play),
        )
        .and_then(|(top_id, src_id, frequency, _)| {
            if top_id != object_id {
                return None;
            }
            // CR 305.1: only land cards qualify for the Play-permission path.
            let obj = state.objects.get(&top_id)?;
            if !obj
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
            {
                return None;
            }
            Some((src_id, frequency))
        });
    let in_library_with_permission = library_permission_src.is_some();
    let exile_play_authorization = if state.exile.contains(&object_id) {
        super::casting::exile_land_play_authorization(state, player, object_id)
    } else {
        None
    };
    let in_exile_with_permission = exile_play_authorization.is_some();

    if !in_hand
        && !in_graveyard_with_permission
        && !in_library_with_permission
        && !in_exile_with_permission
    {
        return Err(EngineError::InvalidAction(
            "Card not found in hand, graveyard, exile, or library with play permission".to_string(),
        ));
    }
    if !state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.card_id == card_id)
    {
        return Err(EngineError::InvalidAction(
            "Card not found or card_id mismatch".to_string(),
        ));
    }

    // CR 110.4: For multi-type graveyard lands via OncePerTurnPerPermanentType,
    // prompt the player to choose which permanent type slot to consume. Skip
    // if a slot was already chosen (pending_permanent_type_slot is set).
    if in_graveyard_with_permission && state.pending_permanent_type_slot.is_none() {
        if let Some(source) = gy_permission_source {
            if let Some(src_obj) = state.objects.get(&source) {
                let is_per_type = super::functioning_abilities::active_static_definitions(
                    state, src_obj,
                )
                .any(|s| {
                    matches!(
                        s.mode,
                        StaticMode::GraveyardCastPermission {
                            frequency:
                                crate::types::statics::CastFrequency::OncePerTurnPerPermanentType,
                            ..
                        }
                    )
                });
                if is_per_type {
                    let slots =
                        super::casting::available_permanent_type_slots(state, source, object_id);
                    if slots.len() > 1 {
                        return Ok(WaitingFor::ChoosePermanentTypeSlot {
                            player,
                            object_id,
                            card_id,
                            source,
                            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
                            available_slots: slots,
                        });
                    }
                }
            }
        }
    }

    // CR 712.12: MDFC land face selection
    if let Some(obj) = state.objects.get(&object_id) {
        let is_modal = obj
            .back_face
            .as_ref()
            .is_some_and(|bf| bf.layout_kind == Some(crate::types::card::LayoutKind::Modal));
        let front_is_land = obj
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Land);
        let back_is_land = obj.back_face.as_ref().is_some_and(|bf| {
            bf.card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
        });

        if is_modal && front_is_land && back_is_land {
            // Both faces are lands — player must choose which face to put into play.
            // The land path never consumes payment_mode (lands cost no mana), but
            // the field is required; Auto is the inert default.
            return Ok(WaitingFor::ModalFaceChoice {
                player,
                object_id,
                card_id,
                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            });
        }

        if is_modal && !front_is_land && back_is_land {
            // CR 712.12: Only back face is a land — auto-swap (player already chose "play as land")
            let obj = state.objects.get_mut(&object_id).unwrap();
            let back = obj.back_face.take().expect("MDFC has back face");
            let front_snapshot = super::printed_cards::snapshot_object_face(obj);
            super::printed_cards::apply_back_face_to_object(obj, back);
            obj.back_face = Some(front_snapshot);
            // CR 712.8a: Mark back-face so apply_zone_exit_cleanup reverts to front face
            // when this land leaves the battlefield. Do NOT set obj.transformed — MDFC
            // face selection is not transformation.
            obj.modal_back_face = true;
        }
    }

    // Determine origin zone for the zone change event
    let origin_zone = if in_hand {
        Zone::Hand
    } else if in_graveyard_with_permission {
        Zone::Graveyard
    } else if in_exile_with_permission {
        Zone::Exile
    } else {
        // CR 401.5: in_library_with_permission — the card moves Library → Battlefield.
        Zone::Library
    };

    // Route through the replacement pipeline (handles ETB replacements like shock lands)
    let mut proposed = crate::types::proposed_event::ProposedEvent::zone_change(
        object_id,
        origin_zone,
        Zone::Battlefield,
        None,
    );

    // CR 110.2 + CR 110.2a (GitHub #696): A played land's controller
    // defaults to whoever played it, not the card's owner. `player` is the
    // acting land-player already resolved above (turn_resource_owner, or
    // acting_player under shared team turns) — the same identity already
    // used throughout this function for hand/zone lookups, and the correct
    // one even under Mindslaver-style turn control (the turn's rightful
    // player controls what gets played on their turn, not whoever is
    // making the decisions). This is a no-op for the overwhelmingly common
    // owner==player case. A genuine self-ETB "enters under [X]'s control"
    // replacement (enters_under) still wins — it runs later in the same
    // replacement pipeline this event is routed through below, and
    // hard-overwrites this default unconditionally (identical safety
    // property to the stack.rs spell-cast seam this mirrors).
    if let crate::types::proposed_event::ProposedEvent::ZoneChange {
        controller_override,
        ..
    } = &mut proposed
    {
        *controller_override = Some(player);
    }

    // CR 306.5b + CR 310.4b + CR 614.1c: Seed the intrinsic "enters with N
    // counters" replacement for planeswalkers and battles entering the
    // battlefield via a play-from-zone action.
    if let Some(obj) = state.objects.get(&object_id) {
        let intrinsic = super::printed_cards::intrinsic_etb_counters(obj, None);
        if !intrinsic.is_empty() {
            if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                enter_with_counters,
                ..
            } = &mut proposed
            {
                enter_with_counters.extend(intrinsic);
            }
        }
    }

    // CR 614.1c: A land played via a `PlayFromExile` grant that carries
    // `land_enter_tapped` enters the battlefield tapped (Lightstall Inquisitor:
    // "Each land played this way enters tapped."). Seed the tap state on the
    // proposed event so the replacement pipeline applies it like any other
    // ETB-tapped land. Only the exile-play path can carry this grant field.
    if in_exile_with_permission {
        let enters_tapped = state
            .objects
            .get(&object_id)
            .is_some_and(|obj| super::casting::exile_play_land_enters_tapped(obj, player));
        if enters_tapped {
            if let Some(slot) = proposed.battlefield_entry_tap_state_mut() {
                *slot = crate::types::zones::EtbTapState::Tapped;
            }
        }
    }

    match super::replacement::replace_event(state, proposed, events) {
        super::replacement::ReplacementResult::Execute(event) => {
            if let crate::types::proposed_event::ProposedEvent::ZoneChange { object_id, .. } = event
            {
                // Phase B (PLAN §6.2 / §7): the divergent partial copy of
                // `deliver_replaced_zone_change` that used to live here is
                // dissolved — the post-`replace_event` event is a
                // `ReplacementResult::Execute` payload, sealed through the third
                // mint path (`approve_post_replacement`) and delivered by the
                // shared `zone_pipeline::deliver`. The land entry now gets the
                // FULL delivery tail the copy skipped (CR 614.1c
                // `EntersWithAdditionalCounters` statics snapshot, the CR 303.4f
                // `attach_to` host, `entered_via_ability_source` provenance, the
                // CR 701.24a library-shuffle arm). `drain = CallerEpilogue`: the
                // land-play epilogue below owns the `post_replacement_continuation`
                // drain (it clears `post_replacement_source` and runs the
                // land-specific accounting), so the tail must not also drain it.
                let Ok(approved) =
                    crate::game::zone_pipeline::ApprovedZoneChange::approve_post_replacement(event)
                else {
                    unreachable!("`if let ZoneChange` guarantees a ZoneChange payload");
                };
                match crate::game::zone_pipeline::deliver(
                    state,
                    approved,
                    crate::game::zone_pipeline::DeliveryCtx {
                        source_id: None,
                        exile_links: crate::game::zone_pipeline::ExileLinkSpec::default(),
                        drain: crate::types::game_state::PostReplacementDrainOwner::CallerEpilogue,
                        // This resume delivery is not a library placement.
                        library_placement: None,
                    },
                    events,
                ) {
                    crate::game::zone_pipeline::ZoneDeliveryResult::Done => {}
                    // CR 614.1c / CR 614.12a: the delivery tail parked a
                    // counter-replacement prompt and stashed the remaining tail
                    // (carrying `CallerEpilogue`). The land has already entered
                    // the battlefield (the move precedes the counter pause in the
                    // tail), so stamp the play origin now — matching the pre-token
                    // arm, which stamped before the `apply_etb_counters`
                    // early-return — then surface the parked prompt. The land
                    // play itself is already committed.
                    crate::game::zone_pipeline::ZoneDeliveryResult::NeedsChoice(_) => {
                        finalize_committed_land_play(
                            state,
                            player,
                            object_id,
                            origin_zone,
                            gy_permission_source,
                            exile_play_authorization,
                            library_permission_src,
                            events,
                        );
                        return Ok(state.waiting_for.clone());
                    }
                }
                // CR 305.1 + CR 400.7i: stamp land-play provenance ("where it
                // was played from") so effects can find the permanent the
                // played land became. Stamped fresh AFTER delivery (this site
                // records a brand-new origin); the stamp then survives until
                // battlefield EXIT (`reset_for_battlefield_exit`).
                mark_land_played_from_zone(state, object_id, origin_zone);
            }

            // CR 614.12a: Drain post-replacement side effects (e.g., "As this land
            // enters, choose a color") that were stashed by the pipeline when the
            // execute ability is non-modifier work (Choose, etc.). Without this,
            // the choice prompt would fire at a random later resolution point with
            // the wrong controller context.
            if state.has_post_replacement_drain() {
                state.clear_post_replacement_source();
                if let Some(next_waiting_for) =
                    engine_replacement::apply_pending_post_replacement_effect(
                        state,
                        Some(object_id),
                        None,
                        Some(crate::types::replacements::ReplacementEvent::Moved),
                        events,
                    )
                {
                    finalize_committed_land_play(
                        state,
                        player,
                        object_id,
                        origin_zone,
                        gy_permission_source,
                        exile_play_authorization,
                        library_permission_src,
                        events,
                    );
                    return Ok(next_waiting_for);
                }
            }
        }
        super::replacement::ReplacementResult::Prevented => {
            // Land play was prevented — don't increment counters
            return Ok(WaitingFor::Priority {
                player: state.priority_player,
            });
        }
        super::replacement::ReplacementResult::NeedsChoice(player) => {
            // A replacement needs player choice (e.g., shock land "pay 2 life?").
            // Increment counters now — the land play is committed, only the ETB
            // effect is pending.
            finalize_committed_land_play(
                state,
                player,
                object_id,
                origin_zone,
                gy_permission_source,
                exile_play_authorization,
                library_permission_src,
                events,
            );

            return Ok(super::replacement::replacement_choice_waiting_for(
                player, state,
            ));
        }
    }

    finalize_committed_land_play(
        state,
        player,
        object_id,
        origin_zone,
        gy_permission_source,
        exile_play_authorization,
        library_permission_src,
        events,
    );

    // Player retains priority after playing a land
    Ok(WaitingFor::Priority { player })
}

pub(super) fn handle_tap_land_for_mana(
    state: &mut GameState,
    player: PlayerId,
    selection: &crate::types::mana::ManaSourceSelection,
    resume: ManaAbilityResume,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 117.1d + CR 605.3a: the player with priority, or the player making a
    // mana payment, activates their own mana ability. The semantic selection
    // is revalidated against live engine-authored options before any cost or
    // production is applied.
    let option = mana_sources::live_land_mana_option_for_selection(state, player, selection)?;
    mana_sources::activate_mana_source_option(state, player, &option, events, resume)
}

/// CR 605.3b: Reverse a manual land tap — untap source and remove its mana from pool.
/// Rejects if the land isn't tracked or its mana was already spent.
pub(super) fn handle_untap_land_for_mana(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    // Validate: object_id is in this player's lands_tapped_for_mana
    let tracked = state
        .lands_tapped_for_mana
        .get(&player)
        .is_some_and(|ids| ids.contains(&object_id));
    if !tracked {
        return Err(EngineError::InvalidAction(
            "Land was not manually tapped for mana".to_string(),
        ));
    }

    // CR 605.3: Mana abilities resolve immediately — once consumed, irreversible.
    // CR 605.1b: Aura/Equipment with a `TapsForMana` trigger that fired off this
    // land's tap (Fertile Ground / Wild Growth / Utopia Sprawl / Trace of
    // Abundance / Verdant Haven / Market Festival / Weirding Wood / Overgrowth
    // class) added their bonus mana to the same pool with `source_id = aura_id`,
    // not `source_id = land_id`. Refunding only the land's source would strand
    // the aura's mana in the pool, allowing an infinite tap-untap-tap exploit
    // (each cycle adds one bonus, refund only takes the land's mana). Walk every
    // active TapsForMana trigger whose `valid_card` matches the land and refund
    // mana keyed at the trigger's source object too. This preserves CR 605.3b
    // (mana abilities resolve immediately) — the manual-untap convenience is the
    // single irreversibility-bypass channel and must reverse all coupled mana,
    // not just the land's own contribution.
    let aura_sources: Vec<ObjectId> =
        super::mana_sources::aura_taps_for_mana_sources_for_land(state, object_id, player);
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    let removed = player_data.mana_pool.remove_from_source(object_id);
    if removed == 0 {
        return Err(EngineError::InvalidAction(
            "Mana from this source was already spent".to_string(),
        ));
    }
    for aura_id in &aura_sources {
        player_data.mana_pool.remove_from_source(*aura_id);
    }

    // CR 118.3a: an UntapLandForMana during ManaPayment can drain a pinned unit
    // out of the pool. Prune any dangling pins so the finalize spend never tries
    // to honor a pip that no longer exists. Done AFTER the `player_data` borrow
    // above ends so the immutable pool read and the `pending_cast` mutation don't
    // overlap a live `&mut`.
    if state.pending_cast.is_some() {
        let surviving: std::collections::HashSet<crate::types::mana::ManaPipId> = state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| p.mana_pool.mana.iter().map(|u| u.pip_id).collect())
            .unwrap_or_default();
        if let Some(pc) = state.pending_cast.as_mut() {
            pc.pinned_pool_units.retain(|id| surviving.contains(id));
        }
    }

    // Untap the land
    let untapped = crate::game::object_state::resolve_and_apply_object_edit(
        state,
        object_id,
        crate::types::resolved_commands::ResolvedObjectStatus::Tapped,
        false,
    )
    .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
    debug_assert!(untapped, "a tracked manually tapped land must be tapped");
    events.push(GameEvent::PermanentUntapped { object_id });

    // Remove from tracking
    if let Some(ids) = state.lands_tapped_for_mana.get_mut(&player) {
        ids.retain(|&id| id != object_id);
        if ids.is_empty() {
            state.lands_tapped_for_mana.remove(&player);
        }
    }

    Ok(())
}

/// CR 118.3a: Record a player-directed pin on a specific pool unit so the
/// finalize spend prefers it. The unit stays in the pool — this is a priority
/// hint, not a removal. A pin is accepted only when the unit is eligible to pay
/// at least one shard (or a generic pip) of the full locked cost; otherwise the
/// pin could never be honored, so it is rejected (`ActionNotAllowed`).
pub(super) fn handle_spend_pool_mana(
    state: &mut GameState,
    player: PlayerId,
    pip_id: crate::types::mana::ManaPipId,
) -> Result<(), EngineError> {
    // The unit must currently exist in the player's pool.
    let unit = state
        .players
        .iter()
        .find(|p| p.id == player)
        .and_then(|p| p.mana_pool.mana.iter().find(|u| u.pip_id == pip_id))
        .cloned()
        .ok_or_else(|| {
            EngineError::ActionNotAllowed("No such mana unit in pool to pin".to_string())
        })?;

    let pending = state.pending_cast.as_ref().ok_or_else(|| {
        EngineError::ActionNotAllowed("No pending cast to pin mana for".to_string())
    })?;
    let object_id = pending.object_id;
    let cost = pending.cost.clone();
    let activation_ability_index = pending.activation_ability_index;

    // CR 118.3a: eligibility against the full LOCKED cost. Nothing is paid at pin
    // time, so there is no "currently-unpaid" subset — the unit qualifies if it
    // could pay any shard (or generic pip) of the whole cost under the SAME
    // spend-restriction context the finalize spend will use. A `pending_cast`
    // can be an activated ability, not just a spell (CR 602): mirror
    // `finalize_mana_payment` and build a `PaymentContext::Activation` so an
    // activation-restricted unit (`OnlyForActivation`, `allows_spell == false`)
    // is correctly eligible to pin when it can legally pay the activation.
    // Owned holders so the context's borrowed slices outlive the eligibility check.
    let spell_meta;
    let activation_context;
    let ctx = if let Some(ability_index) = activation_ability_index {
        activation_context =
            super::casting::activation_payment_context(state, object_id, Some(ability_index));
        Some(activation_context.as_payment_context())
    } else {
        spell_meta = super::casting::build_spell_meta(state, player, object_id);
        spell_meta
            .as_ref()
            .map(crate::types::mana::PaymentContext::Spell)
    };

    if !mana_unit_eligible_for_cost(&unit, &cost, ctx.as_ref()) {
        return Err(EngineError::ActionNotAllowed(
            "Mana unit cannot pay any part of this cost".to_string(),
        ));
    }

    if let Some(pc) = state.pending_cast.as_mut() {
        if !pc.pinned_pool_units.contains(&pip_id) {
            pc.pinned_pool_units.push(pip_id);
        }
    }
    Ok(())
}

/// CR 118.3a: Remove a previously-recorded pin. Always legal — a no-op if the
/// pin is absent or there is no pending cast.
pub(super) fn handle_unspend_pool_mana(
    state: &mut GameState,
    pip_id: crate::types::mana::ManaPipId,
) {
    if let Some(pc) = state.pending_cast.as_mut() {
        pc.pinned_pool_units.retain(|id| *id != pip_id);
    }
}

/// CR 118.3a: True when `unit` could legally pay at least one shard or generic
/// pip of `cost` under the spell's spend-restriction context. Combines
/// restriction gating (`ManaRestriction::allows`) with shard color/attribute
/// matching (`shard_to_mana_type`) — the same predicates the spend funnel uses.
fn mana_unit_eligible_for_cost(
    unit: &crate::types::mana::ManaUnit,
    cost: &crate::types::mana::ManaCost,
    ctx: Option<&crate::types::mana::PaymentContext<'_>>,
) -> bool {
    use crate::types::mana::{ManaCost, ManaType};
    use mana_payment::ShardRequirement;

    // CR 106.6: a unit whose restrictions reject this context can pay nothing here.
    if let Some(ctx) = ctx {
        if !mana_payment::mana_unit_permits_payment_context(unit, ctx) {
            return false;
        }
    }
    // Convoke/improvise/delve markers are creature-tap stand-ins, never pinned.
    if unit.is_convoke_payment() {
        return false;
    }

    let (shards, generic) = match cost {
        ManaCost::Cost { shards, generic } => (shards, *generic),
        // No-cost / self-referential costs have no payable pip.
        _ => return false,
    };

    // CR 107.4b: any unit can pay a generic pip ({N} or {X}).
    if generic > 0 {
        return true;
    }

    shards.iter().any(|&shard| {
        // CR 107.4: a unit pays a shard if its color (or attribute, for {S}/{Z})
        // is among those the shard accepts.
        let accepts = |c: ManaType| unit.color == c;
        match mana_payment::shard_to_mana_type(shard) {
            ShardRequirement::Single(mt) => accepts(mt),
            ShardRequirement::Hybrid(a, b) => accepts(a) || accepts(b),
            ShardRequirement::Phyrexian(c) => accepts(c),
            ShardRequirement::HybridPhyrexian(a, b) => accepts(a) || accepts(b),
            // {2/C} and {C/color}: payable with the color, or (for {2/C}) generic.
            ShardRequirement::TwoGenericHybrid(c) => accepts(c),
            ShardRequirement::ColorlessHybrid(c) => accepts(ManaType::Colorless) || accepts(c),
            ShardRequirement::Snow => unit.is_snow(),
            ShardRequirement::TwoOrMoreColorSource => unit.source_could_produce_two_or_more_colors,
            // {X} contributes nothing off the stack (CR 107.3); generic-payable
            // when X > 0 is already covered by the `generic` check above.
            ShardRequirement::X => false,
            ShardRequirement::TwoGenericHybridPhyrexian(c) => accepts(c),
        }
    })
}

fn handle_equip_activation(
    state: &mut GameState,
    player: PlayerId,
    equipment_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate sorcery-speed timing: main phase, empty stack, active player
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Equip can only be activated during main phases".to_string(),
            ));
        }
    }
    if !state.stack.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "Equip can only be activated when the stack is empty".to_string(),
        ));
    }
    if state.active_player != player {
        return Err(EngineError::ActionNotAllowed(
            "Equip can only be activated by the active player".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&equipment_id)
        .ok_or_else(|| EngineError::InvalidAction("Equipment not found".to_string()))?;

    // Validate it's an equipment on the battlefield controlled by player
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Equipment is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this equipment".to_string(),
        ));
    }
    if !obj.card_types.subtypes.contains(&"Equipment".to_string()) {
        return Err(EngineError::InvalidAction(
            "Object is not an equipment".to_string(),
        ));
    }

    // Find valid targets: creatures controlled by the equipping player on battlefield
    let valid_targets: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|o| {
                    o.controller == player
                        && o.card_types
                            .core_types
                            .contains(&crate::types::card_type::CoreType::Creature)
                })
                .unwrap_or(false)
        })
        .collect();

    if valid_targets.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "No valid creatures to equip".to_string(),
        ));
    }

    // If only one target, auto-equip: CR 113.3b still requires the stack entry
    // + priority window; we skip only the target-selection UI.
    if valid_targets.len() == 1 {
        let target_id = valid_targets[0];
        return Ok(push_keyword_action(
            state,
            player,
            equipment_id,
            KeywordAction::Equip {
                equipment_id,
                target_creature_id: target_id,
            },
            events,
        ));
    }

    priority::clear_priority_passes(state);
    Ok(WaitingFor::EquipTarget {
        player,
        equipment_id,
        valid_targets,
    })
}

/// CR 702.122a: Activate a Vehicle's crew ability from Priority.
/// Unlike Equip (CR 702.6a) and Saddle (CR 702.171a), Crew has NO "Activate only as a
/// sorcery" restriction — it can be activated any time the controller has priority.
/// CR 702.122a + CR 702.122d: can this creature legally be tapped to pay a crew
/// cost right now?
///
/// Composes the two halves the crew payment path enforces — the tappability rule
/// in [`is_tappable_creature_for_cost`] (controlled, untapped, a creature, and
/// not under a `CantTap` restriction) and the `CantCrew` static — into one
/// named authority.
///
/// `pub` so consumers outside this module (`phase-ai`'s
/// `VehicleDeploymentPolicy`) ask THIS question instead of assembling their own
/// filter from the parts. A partial duplicate silently over-counts: omitting the
/// `object_cant_tap` term alone makes a `CantTap` 3/3 look like it can pay
/// Crew 3, which it cannot.
pub fn creature_can_pay_crew(state: &GameState, id: ObjectId, player: PlayerId) -> bool {
    is_tappable_creature_for_cost(state, id, player)
        && !super::static_abilities::object_has_cant_crew(state, id)
}

fn is_tappable_creature_for_cost(state: &GameState, id: ObjectId, player: PlayerId) -> bool {
    state.objects.get(&id).is_some_and(|o| {
        o.controller == player
            && !o.tapped
            && o.card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Creature)
            && !crate::game::restrictions::object_cant_tap(state, id)
    })
}

/// CR 602.5b + CR 702.122a: "activate only once each turn" is keyed to the exact
/// object incarnation, so a Vehicle that leaves and returns (a new object per
/// CR 400.7) may be crewed again. Single authority for reading the crew-cadence
/// set — callers never touch `crew_activated_this_turn` directly.
pub(crate) fn crew_activated_this_turn_contains(state: &GameState, vehicle_id: ObjectId) -> bool {
    state
        .objects
        .get(&vehicle_id)
        .map(crate::types::identifiers::ObjectIncarnationRef::from_object)
        .is_some_and(|r| state.crew_activated_this_turn.contains(&r))
}

/// CR 602.5b + CR 702.122a: record a crew activation against the Vehicle's current
/// incarnation. Single authority for writing the crew-cadence set.
pub(crate) fn record_crew_activation(state: &mut GameState, vehicle_id: ObjectId) {
    if let Some(r) = state
        .objects
        .get(&vehicle_id)
        .map(crate::types::identifiers::ObjectIncarnationRef::from_object)
    {
        state.crew_activated_this_turn.insert(r);
    }
}

fn handle_crew_activation(
    state: &mut GameState,
    player: PlayerId,
    vehicle_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&vehicle_id)
        .ok_or_else(|| EngineError::InvalidAction("Vehicle not found".to_string()))?;

    // Validate it's a Vehicle on the battlefield controlled by player
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Vehicle is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this Vehicle".to_string(),
        ));
    }
    if !obj.card_types.subtypes.contains(&"Vehicle".to_string()) {
        return Err(EngineError::InvalidAction(
            "Object is not a Vehicle".to_string(),
        ));
    }

    // Extract crew power and once-each-turn cadence from keywords.
    let (crew_power, crew_once_per_turn) = obj
        .keywords
        .iter()
        .find_map(|kw| {
            if let crate::types::keywords::Keyword::Crew {
                power,
                once_per_turn,
            } = kw
            {
                // CR 602.5b: once_per_turn is `Some(OnlyOnceEachTurn)` when the
                // Vehicle's crew ability is limited to once each turn.
                let limited = matches!(
                    once_per_turn.as_deref(),
                    Some(crate::types::ability::ActivationRestriction::OnlyOnceEachTurn)
                );
                Some((*power, limited))
            } else {
                None
            }
        })
        .ok_or_else(|| EngineError::InvalidAction("Vehicle has no Crew keyword".to_string()))?;

    // CR 602.5b: "Activate only once each turn" — reject a second crew activation
    // of this Vehicle in the same turn.
    if crew_once_per_turn && crew_activated_this_turn_contains(state, vehicle_id) {
        return Err(EngineError::ActionNotAllowed(
            "This Vehicle's crew ability can be activated only once each turn".to_string(),
        ));
    }

    // CR 702.122d: Exclude creatures with "can't crew Vehicles".
    let eligible_creatures: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            id != vehicle_id
                && is_tappable_creature_for_cost(state, id, player)
                && !super::static_abilities::object_has_cant_crew(state, id)
        })
        .collect();

    // Validate total power of all eligible creatures can meet the threshold.
    // CR 702.122a: a creature's contribution may be modified ("as though its
    // power were N greater" / "using its toughness rather than its power"). The
    // per-creature contributions travel with the choice so the UI gates the
    // selection on the same adjusted values the engine validates against, rather
    // than re-deriving from raw power.
    let contributions: Vec<i32> = eligible_creatures
        .iter()
        .map(|&id| {
            super::static_abilities::object_crew_power_contribution(
                state,
                id,
                crate::types::statics::CrewAction::Crew,
            )
        })
        .collect();
    let total_power: i32 = contributions.iter().sum();

    if total_power < crew_power as i32 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough total power among eligible creatures to crew".to_string(),
        ));
    }

    let _ = events; // No events emitted during activation
    priority::clear_priority_passes(state);
    Ok(WaitingFor::CrewVehicle {
        player,
        vehicle_id,
        crew_power,
        eligible_creatures,
        contributions,
    })
}

/// CR 113.3b: Push an activated keyword ability onto the stack and reset
/// priority. Called by the *_announcement handlers after costs have been paid
/// and targets selected. The payload is resolved via `stack::resolve_top`
/// once all players pass priority.
fn push_keyword_action(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    action: KeywordAction,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    super::stack::push_to_stack(
        state,
        StackEntry {
            id: entry_id,
            source_id,
            controller: player,
            kind: StackEntryKind::KeywordAction { action },
        },
        events,
    );
    priority::clear_priority_passes(state);
    WaitingFor::Priority { player }
}

/// CR 702.122a + CR 113.3b: Announce a Vehicle's crew ability. Pays the cost
/// (tap selected creatures) and pushes a `KeywordAction::Crew` stack entry.
/// The Vehicle animation happens at stack resolution, not here — opening a
/// priority window for counterspell-class effects (CR 113.3b).
fn handle_crew_announcement(
    state: &mut GameState,
    player: PlayerId,
    vehicle_id: ObjectId,
    crew_power: u32,
    eligible_creatures: &[ObjectId],
    creature_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if creature_ids.is_empty() {
        return Err(EngineError::InvalidAction(
            "Must select at least one creature to crew".to_string(),
        ));
    }

    // Validate Vehicle is still on battlefield and controlled by player
    let vehicle = state
        .objects
        .get(&vehicle_id)
        .ok_or_else(|| EngineError::InvalidAction("Vehicle no longer exists".to_string()))?;
    if vehicle.zone != Zone::Battlefield || vehicle.controller != player {
        return Err(EngineError::InvalidAction(
            "Vehicle is no longer valid for crewing".to_string(),
        ));
    }

    // Validate all creature_ids are in eligible_creatures
    for &cid in creature_ids {
        if !eligible_creatures.contains(&cid) {
            return Err(EngineError::InvalidAction(
                "Creature not in eligible list".to_string(),
            ));
        }
    }

    // Re-validate and read power of each creature BEFORE tapping (HarmonizeTap idiom)
    let mut total_power: i32 = 0;
    for &cid in creature_ids {
        let obj = state
            .objects
            .get(&cid)
            .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".to_string()))?;
        if obj.zone != Zone::Battlefield || obj.tapped {
            return Err(EngineError::InvalidAction(
                "Creature is no longer eligible for crewing".to_string(),
            ));
        }
        if crate::game::restrictions::object_cant_tap(state, cid) {
            return Err(EngineError::InvalidAction(
                "Creature can't become tapped".to_string(),
            ));
        }
        if super::static_abilities::object_has_cant_crew(state, cid) {
            return Err(EngineError::InvalidAction(
                "Creature can't crew Vehicles".to_string(),
            ));
        }
        // CR 702.122a: apply any crew power-contribution modifier.
        total_power += super::static_abilities::object_crew_power_contribution(
            state,
            cid,
            crate::types::statics::CrewAction::Crew,
        );
    }

    // CR 702.122a: Total power must meet threshold
    if total_power < crew_power as i32 {
        return Err(EngineError::InvalidAction(
            "Selected creatures' total power is less than crew requirement".to_string(),
        ));
    }

    // CR 701.26a + CR 702.122b + CR 508.1f: Tap each creature as cost payment —
    // creature "crews" the Vehicle. Routed through the single authority so a
    // "can't become tapped" creature is refused.
    for &cid in creature_ids {
        crate::game::restrictions::tap_permanent_for_cost(state, cid, events)?;
    }

    // CR 602.5b: Record this crew activation so an "Activate only once each turn"
    // Vehicle cannot be crewed a second time this turn. Cleared at turn start.
    record_crew_activation(state, vehicle_id);

    Ok(push_keyword_action(
        state,
        player,
        vehicle_id,
        KeywordAction::Crew {
            vehicle_id,
            paid_creature_ids: creature_ids.to_vec(),
        },
        events,
    ))
}

// ---------------------------------------------------------------------------
// CR 702.184a: Station — keyword action with per-card dispatch (mirrors Crew)
// ---------------------------------------------------------------------------

/// CR 702.184a: Activate a Spacecraft's station ability from Priority.
/// Per CR 702.184a: "Activate only as a sorcery." — the activation is rejected
/// outside the active player's main phase, empty stack, own priority.
fn handle_station_activation(
    state: &mut GameState,
    player: PlayerId,
    spacecraft_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&spacecraft_id)
        .ok_or_else(|| EngineError::InvalidAction("Spacecraft not found".to_string()))?;

    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Spacecraft is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this Spacecraft".to_string(),
        ));
    }
    if !obj
        .keywords
        .iter()
        .any(|k| matches!(k, crate::types::keywords::Keyword::Station))
    {
        return Err(EngineError::InvalidAction(
            "Object has no Station keyword".to_string(),
        ));
    }

    // CR 702.184a: "Activate only as a sorcery."
    if !super::restrictions::is_sorcery_speed_window(state, player) {
        return Err(EngineError::ActionNotAllowed(
            "Station may only be activated as a sorcery".to_string(),
        ));
    }

    // CR 702.184a: "Tap another untapped creature you control" — the chosen
    // creature is NOT the Spacecraft, is a creature, is untapped, and is
    // controlled by the activating player.
    let eligible_creatures: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| id != spacecraft_id && is_tappable_creature_for_cost(state, id, player))
        .collect();

    if eligible_creatures.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "No eligible creatures to tap for Station".to_string(),
        ));
    }

    let _ = events; // No events emitted during activation (cost payment happens at resolution).
    priority::clear_priority_passes(state);
    Ok(WaitingFor::StationTarget {
        player,
        spacecraft_id,
        eligible_creatures,
    })
}

/// CR 702.184a + CR 113.3b: Announce Station. Pays the cost (tap the chosen
/// creature), snapshots its power per CR 113.7a, and pushes a
/// `KeywordAction::Station` stack entry. Charge counters are applied at
/// stack resolution, after a priority window (CR 113.3b).
fn handle_station_announcement(
    state: &mut GameState,
    player: PlayerId,
    spacecraft_id: ObjectId,
    eligible_creatures: &[ObjectId],
    creature_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 702.184a: Re-validate the chosen creature is still eligible (pending-effect
    // time gap between activation and resolution). Mirrors the HarmonizeTap idiom.
    if !eligible_creatures.contains(&creature_id) {
        return Err(EngineError::InvalidAction(
            "Creature not in eligible list".to_string(),
        ));
    }

    let spacecraft = state
        .objects
        .get(&spacecraft_id)
        .ok_or_else(|| EngineError::InvalidAction("Spacecraft no longer exists".to_string()))?;
    if spacecraft.zone != Zone::Battlefield || spacecraft.controller != player {
        return Err(EngineError::InvalidAction(
            "Spacecraft is no longer valid for stationing".to_string(),
        ));
    }

    let creature = state
        .objects
        .get(&creature_id)
        .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".to_string()))?;
    if creature.zone != Zone::Battlefield
        || creature.controller != player
        || creature.tapped
        || !creature
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Creature)
        || crate::game::restrictions::object_cant_tap(state, creature_id)
    {
        return Err(EngineError::InvalidAction(
            "Creature is no longer eligible for Station".to_string(),
        ));
    }

    // CR 702.184a + CR 113.7a: Snapshot the creature's power BEFORE tapping —
    // the counter count is determined at cost-payment time and survives the
    // creature leaving the battlefield before resolution. CR 702.184c:
    // static abilities may modify the contributed value ("stations
    // permanents as though its power were N greater"); the helper applies any
    // such modifier and otherwise reads `power`, the default per the rule.
    let snapshot_power = super::static_abilities::object_crew_power_contribution(
        state,
        creature_id,
        crate::types::statics::CrewAction::Station,
    );

    // CR 701.26a: Tap the creature as cost payment. Routed through the single
    // authority (CR 508.1f exempts attacker declaration) so a "can't become
    // tapped" creature is refused.
    crate::game::restrictions::tap_permanent_for_cost(state, creature_id, events)?;

    Ok(push_keyword_action(
        state,
        player,
        spacecraft_id,
        KeywordAction::Station {
            spacecraft_id,
            paid_creature_id: creature_id,
            snapshot_power,
        },
        events,
    ))
}

// ---------------------------------------------------------------------------
// CR 702.171a: Saddle — keyword action with per-card dispatch (mirrors Crew)
// ---------------------------------------------------------------------------

/// CR 702.171a: Activate a Mount's saddle ability from Priority.
/// Enforces the sorcery-speed gate: main phase, empty stack, active player.
fn handle_saddle_activation(
    state: &mut GameState,
    player: PlayerId,
    mount_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&mount_id)
        .ok_or_else(|| EngineError::InvalidAction("Mount not found".to_string()))?;

    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Mount is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this Mount".to_string(),
        ));
    }

    // Extract saddle power from keywords — fails if this permanent has no Saddle keyword.
    let saddle_power = obj
        .keywords
        .iter()
        .find_map(|kw| {
            if let crate::types::keywords::Keyword::Saddle(n) = kw {
                Some(*n)
            } else {
                None
            }
        })
        .ok_or_else(|| EngineError::InvalidAction("Object has no Saddle keyword".to_string()))?;

    // CR 702.171a: "Activate only as a sorcery."
    if !super::restrictions::is_sorcery_speed_window(state, player) {
        return Err(EngineError::ActionNotAllowed(
            "Saddle may only be activated as a sorcery".to_string(),
        ));
    }

    // CR 702.171a: "Tap any number of other untapped creatures you control."
    let eligible_creatures: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| id != mount_id && is_tappable_creature_for_cost(state, id, player))
        .collect();

    // CR 702.171a: a creature's saddle contribution may be modified.
    let contributions: Vec<i32> = eligible_creatures
        .iter()
        .map(|&id| {
            super::static_abilities::object_crew_power_contribution(
                state,
                id,
                crate::types::statics::CrewAction::Saddle,
            )
        })
        .collect();
    let total_power: i32 = contributions.iter().sum();

    if total_power < saddle_power as i32 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough total power among eligible creatures to saddle".to_string(),
        ));
    }

    let _ = events;
    priority::clear_priority_passes(state);
    Ok(WaitingFor::SaddleMount {
        player,
        mount_id,
        saddle_power,
        eligible_creatures,
        contributions,
    })
}

/// CR 702.171a + CR 113.3b: Announce Saddle. Pays the cost (tap selected
/// creatures) and pushes a `KeywordAction::Saddle` stack entry. The "becomes
/// saddled UEOT" designation is applied at stack resolution.
fn handle_saddle_announcement(
    state: &mut GameState,
    player: PlayerId,
    mount_id: ObjectId,
    saddle_power: u32,
    eligible_creatures: &[ObjectId],
    creature_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if creature_ids.is_empty() {
        return Err(EngineError::InvalidAction(
            "Must select at least one creature to saddle".to_string(),
        ));
    }

    let mount = state
        .objects
        .get(&mount_id)
        .ok_or_else(|| EngineError::InvalidAction("Mount no longer exists".to_string()))?;
    if mount.zone != Zone::Battlefield || mount.controller != player {
        return Err(EngineError::InvalidAction(
            "Mount is no longer valid for saddling".to_string(),
        ));
    }

    for &cid in creature_ids {
        if !eligible_creatures.contains(&cid) {
            return Err(EngineError::InvalidAction(
                "Creature not in eligible list".to_string(),
            ));
        }
    }

    let mut total_power: i32 = 0;
    for &cid in creature_ids {
        let obj = state
            .objects
            .get(&cid)
            .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".to_string()))?;
        if obj.zone != Zone::Battlefield || obj.tapped {
            return Err(EngineError::InvalidAction(
                "Creature is no longer eligible for saddling".to_string(),
            ));
        }
        if crate::game::restrictions::object_cant_tap(state, cid) {
            return Err(EngineError::InvalidAction(
                "Creature can't become tapped".to_string(),
            ));
        }
        // CR 702.171a: apply any saddle power-contribution modifier.
        total_power += super::static_abilities::object_crew_power_contribution(
            state,
            cid,
            crate::types::statics::CrewAction::Saddle,
        );
    }

    if total_power < saddle_power as i32 {
        return Err(EngineError::InvalidAction(
            "Selected creatures' total power is less than saddle requirement".to_string(),
        ));
    }

    // CR 701.26a + CR 702.171c + CR 508.1f: Tap each creature as cost payment —
    // creature "saddles" the Mount. Routed through the single authority so a
    // "can't become tapped" creature is refused.
    for &cid in creature_ids {
        crate::game::restrictions::tap_permanent_for_cost(state, cid, events)?;
    }

    Ok(push_keyword_action(
        state,
        player,
        mount_id,
        KeywordAction::Saddle {
            mount_id,
            paid_creature_ids: creature_ids.to_vec(),
        },
        events,
    ))
}

pub fn new_game(seed: u64) -> GameState {
    GameState::new_two_player(seed)
}

/// Maximum number of tie-break reroll rounds in the first-player contest.
///
/// Load-bearing safety cap: if every tied seat re-rolls the same value, the
/// tied group does not shrink, so an unbounded "reroll the tied group" loop
/// could spin forever on a degenerate RNG. After this many rounds the tie is
/// broken deterministically by lowest seat index (see `start_game`).
const FIRST_PLAYER_CONTEST_MAX_ROUNDS: usize = 16;

/// CR 103.1: run the starting-player roll-off and capture its round structure.
///
/// `roll_round` is called once per round with the current contender set (in
/// seat order) and returns each contender's d20 result. Round 1 = all seats;
/// each later round = the prior round's tied-max group (CR 103.1 reroll).
/// Returns the per-round structure and the winner: the unique max of the final
/// round, or the lowest seat index when still tied at
/// `FIRST_PLAYER_CONTEST_MAX_ROUNDS`.
///
/// The selection logic (contenders narrowing, max/top filtering, bounded cap,
/// lowest-seat fallback) is identical to the prior inline loop; the only change
/// is that each round's rolls are captured into a `ContestRound` instead of
/// pushed as flat `DieRolled` events.
fn build_contest_rounds(
    seat_order: &[PlayerId],
    mut roll_round: impl FnMut(&[PlayerId]) -> Vec<(PlayerId, u8)>,
) -> (Vec<ContestRound>, PlayerId) {
    let mut rounds: Vec<ContestRound> = Vec::new();

    // `contenders` is the set of seats still in the running. It starts as every
    // seat and, after each tie, narrows to the tied top group only.
    let mut contenders: Vec<PlayerId> = seat_order.to_vec();
    let mut starting_player: Option<PlayerId> = None;

    // BOUNDED tie loop. Each iteration rolls every contender; a unique high
    // roller wins. On a tie, `contenders` narrows to the tied top group and we
    // reroll just them. INVARIANT: if every tied seat re-rolls the same value
    // the group does NOT shrink, so this loop is bounded by
    // FIRST_PLAYER_CONTEST_MAX_ROUNDS rather than relying on the group ever
    // shrinking. If the cap is reached while still tied, the tie is broken
    // deterministically by lowest seat index below — the engine can never hang.
    for _round in 0..FIRST_PLAYER_CONTEST_MAX_ROUNDS {
        let rolls: Vec<(PlayerId, u8)> = roll_round(&contenders);
        let max_roll = rolls.iter().map(|&(_, r)| r).max().expect("non-empty");
        let top: Vec<PlayerId> = rolls
            .iter()
            .filter(|&&(_, r)| r == max_roll)
            .map(|&(seat, _)| seat)
            .collect();
        rounds.push(ContestRound { rolls });
        if top.len() == 1 {
            starting_player = Some(top[0]);
            break;
        }
        // Tie: reroll only the tied top group on the next round.
        contenders = top;
    }

    // Deterministic fallback: still tied at the cap → lowest seat index wins.
    let starting_player = starting_player.unwrap_or_else(|| {
        contenders
            .iter()
            .copied()
            .min()
            .expect("contenders is always non-empty")
    });

    (rounds, starting_player)
}

/// Start game with mulligan flow. If no cards in libraries, skips mulligan.
///
/// CR 103.1: At the start of game 1 of a match the players determine who takes
/// the first turn "using any mutually agreeable method (flipping a coin,
/// rolling dice, etc.)". This engine models that determination as an
/// authoritative d20 high-roll contest — one d20 per seat using the game's
/// seeded RNG (CR 706, rolling a die) — with ties rerolled among the tied top
/// group. NOTE ON FIDELITY: the literal CR 103.1 sequence is "contest winner
/// *chooses* who takes the first turn"; this engine collapses that to "contest
/// winner *becomes* the starting player" (it does not present a play/draw
/// choice here), an existing, accepted simplification — the annotation does not
/// claim the choose-step is implemented. Subsequent games in a multi-game match
/// route through `match_flow::start_next_game`, which uses `next_game_chooser`
/// instead, so this function is always the game-1 path.
///
/// The contest is surfaced as a single authoritative
/// `GameEvent::StartingPlayerContest` carrying the full round structure (round
/// 1 = all seats, each later round = the prior round's tied-max reroll group)
/// plus the engine's authoritative `winner`, so downstream consumers render the
/// contest round by round without re-deriving anything. It is inserted at the
/// front of the result, ahead of `GameStarted` → `TurnStarted`. This replaces
/// the prior flat per-roll `DieRolled` batch; in-game die rolls still emit
/// `DieRolled`.
///
/// DETERMINISM: the contest draws only from `state.rng` (the seeded
/// `ChaCha20Rng`), never thread/global RNG, so replays and AI search stay
/// deterministic. The RNG draw count and order are EXACTLY as before — one
/// `random_range(1..=20)` per contender per round, in seat order — so this
/// representation change introduces ZERO determinism shift relative to the
/// prior `DieRolled`-batch implementation. (It still differs from the original
/// single `random_range(0..len)` pick that predated the contest, an earlier,
/// accepted shift.)
///
/// Callers that need a deterministic starter (tests, fixed scenarios) must use
/// `start_game_with_starting_player` directly — that path runs no contest and
/// emits no `StartingPlayerContest` event.
pub fn start_game(state: &mut GameState) -> ActionResult {
    if state.seat_order.is_empty() {
        return start_game_with_starting_player(state, PlayerId(0));
    }

    if let Some(archenemy) = super::topology::archenemy(state) {
        // CR 904.6: The archenemy takes the first turn. Default Archenemy does
        // not run the CR 103.1 starting-player contest.
        return start_game_with_starting_player(state, archenemy);
    }

    // CR 103.1 / CR 706: roll one d20 per seat; the high roller becomes the
    // starting player. Draw order/count is identical to the prior
    // implementation — one `random_range(1..=20)` per contender, in seat order.
    let seat_order = state.seat_order.clone();
    let (rounds, starting_player) = build_contest_rounds(&seat_order, |contenders| {
        contenders
            .iter()
            .map(|&seat| (seat, state.rng.random_range(1..=20u8)))
            .collect()
    });

    let mut result = start_game_with_starting_player(state, starting_player);
    // CR 103.1: StartingPlayerContest → GameStarted → TurnStarted.
    result.events.insert(
        0,
        GameEvent::StartingPlayerContest {
            rounds,
            winner: starting_player,
        },
    );
    result
}

/// Start game with a specific player taking the first turn.
pub fn start_game_with_starting_player(
    state: &mut GameState,
    starting_player: PlayerId,
) -> ActionResult {
    let mut events = Vec::new();
    state.outside_game_cards_brought_in.clear();
    let starting_player = super::topology::archenemy(state).unwrap_or(starting_player);

    if state.match_config.match_type == MatchType::Bo3
        && state.players.len() != 2
        && super::topology::archenemy(state).is_none()
    {
        state.match_config.match_type = MatchType::Bo1;
    }

    events.push(GameEvent::GameStarted);

    // Begin the game: set turn 1
    state.turn_number = 1;
    state.active_player = starting_player;
    state.priority_player = starting_player;
    state.current_starting_player = starting_player;
    // First-game default chooser is the starting player; BO3 restarts can pre-set this.
    if state.next_game_chooser.is_none() {
        state.next_game_chooser = Some(starting_player);
    }
    // Rotate seat order so mulligan starts with the starting player.
    if let Some(idx) = state.seat_order.iter().position(|&p| p == starting_player) {
        state.seat_order.rotate_left(idx);
    }
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: starting_player,
        turn_number: 1,
    });

    // If players have cards in their libraries, start mulligan flow
    let has_libraries = state.players.iter().any(|p| !p.library.is_empty());
    let waiting_for = if has_libraries {
        // CR 702.139a: Check for eligible companions before mulligans.
        if let Some(companion_wf) = super::companion::check_all_companion_reveals(state) {
            companion_wf
        } else {
            mulligan::start_mulligan(state, &mut events)
        }
    } else {
        // No cards to mulligan with, skip straight to game
        crate::game::planechase::reveal_starting_plane(state);
        turns::auto_advance(state, &mut events)
    };

    state.waiting_for = waiting_for.clone();
    bump_state_revision(state);
    mark_public_state_all_dirty(state);
    finalize_public_state(state);

    let log_entries = super::log::resolve_log_entries(&events, state);
    ActionResult {
        events,
        waiting_for,
        log_entries,
    }
}

/// Start game without mulligan (for backward compatibility with existing tests).
pub fn start_game_skip_mulligan(state: &mut GameState) -> ActionResult {
    let mut events = Vec::new();
    state.outside_game_cards_brought_in.clear();
    let starting_player = super::topology::archenemy(state).unwrap_or(PlayerId(0));

    events.push(GameEvent::GameStarted);

    state.turn_number = 1;
    state.active_player = starting_player;
    state.priority_player = starting_player;
    state.current_starting_player = starting_player;
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: starting_player,
        turn_number: 1,
    });

    crate::game::planechase::reveal_starting_plane(state);
    let waiting_for = turns::auto_advance(state, &mut events);
    state.waiting_for = waiting_for.clone();
    bump_state_revision(state);
    mark_public_state_all_dirty(state);
    finalize_public_state(state);

    let log_entries = super::log::resolve_log_entries(&events, state);
    ActionResult {
        events,
        waiting_for,
        log_entries,
    }
}

/// CR 607.2a + CR 406.6: Check if any exile-return sources have left the battlefield.
/// If so, move the exiled cards back — linked abilities track which cards were exiled by the source.
pub(super) fn check_exile_returns(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let mut to_return: Vec<crate::types::game_state::ExileLink> = Vec::new();

    for event in events.iter() {
        if let GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Battlefield),
            ..
        } = event
        {
            // Find exile links where this object was the source and the exile
            // effect specified an automatic return when that source leaves.
            for link in &state.exile_links {
                if link.source_id == *object_id
                    && matches!(
                        &link.kind,
                        crate::types::game_state::ExileLinkKind::UntilSourceLeaves { .. }
                    )
                {
                    to_return.push(link.clone());
                }
            }
        }
    }

    if to_return.is_empty() {
        return;
    }

    // CR 610.3 + CR 614.6: Return each exiled card to its previous zone through
    // the zone-change pipeline so a battlefield return seeds enters-with-counters
    // statics (Hardened Scales class) and so a `Moved` redirect fires on any
    // non-battlefield return — the raw `move_to_zone` skipped the delivery tail.
    // Group by destination zone (CR 603.10a: cards returning to the same zone do
    // so simultaneously); within a group each card self-anchors its attribution
    // (CR 400.7 — the pre-pipeline raw move recorded no source).
    //
    // The spent `UntilSourceLeaves` links are dropped via a per-group
    // `RemoveExileLinks` completion so the cleanup runs exactly once after the
    // group's pile lands, even when a returned creature pauses on an as-enters /
    // aura-host choice (CR 303.4f / 616.1): the parked batch tail + completion
    // are drained by the replacement-choice / aura-attachment resume.
    // First-seen insertion order (not a HashMap) so group processing is
    // deterministic for the engine's reproducibility guarantee.
    let mut groups: Vec<(Zone, Vec<ObjectId>)> = Vec::new();
    for link in &to_return {
        let still_in_exile = state
            .objects
            .get(&link.exiled_id)
            .map(|obj| obj.zone == Zone::Exile)
            .unwrap_or(false);
        if !still_in_exile {
            continue;
        }
        let crate::types::game_state::ExileLinkKind::UntilSourceLeaves { return_zone } = &link.kind
        else {
            continue;
        };
        let return_zone = *return_zone;
        let gi = match groups.iter().position(|(zone, _)| *zone == return_zone) {
            Some(i) => i,
            None => {
                groups.push((return_zone, Vec::new()));
                groups.len() - 1
            }
        };
        if !groups[gi].1.contains(&link.exiled_id) {
            groups[gi].1.push(link.exiled_id);
        }
        // CR 730.3c: if the source exiled a MERGED permanent, it split into
        // multiple objects (CR 730.3). The implicit "return when the source
        // leaves" must bring back ALL of them, not just the tracked survivor —
        // the components are co-located in exile with the survivor and return to
        // the same zone. (A no-op when the exiled card was not a merged permanent.)
        let components = super::merge::co_split_components(state, link.exiled_id, &groups[gi].1);
        groups[gi].1.extend(components);
    }

    // Links for cards that already left exile (not returned by us) are still spent
    // and must be dropped now — only the IN-FLIGHT group ids ride their batch
    // completion. (The common case is a single battlefield group; a mid-group
    // pause defers only that group's cleanup, while any remaining groups process
    // after — `move_objects_simultaneously_then` parks the tail per group.)
    let returning_ids: std::collections::HashSet<ObjectId> = groups
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect();
    let returned_all: Vec<ObjectId> = to_return.iter().map(|l| l.exiled_id).collect();
    state.exile_links.retain(|link| {
        !returned_all.contains(&link.exiled_id) || returning_ids.contains(&link.exiled_id)
    });

    for (return_zone, ids) in groups {
        let reqs: Vec<_> = ids
            .iter()
            .map(|&id| super::zone_pipeline::ZoneMoveRequest::effect(id, return_zone, id))
            .collect();
        let completion =
            crate::types::game_state::BatchCompletion::RemoveExileLinks { returned_ids: ids };
        if matches!(
            super::zone_pipeline::move_objects_simultaneously_then(
                state,
                reqs,
                Some(completion),
                events,
            ),
            super::zone_pipeline::BatchMoveResult::NeedsChoice
        ) {
            // CR 616.1 / CR 303.4f: this group paused; its tail + cleanup are
            // parked and drained on resume. Stop processing further groups so a
            // later group's moves do not run over the parked prompt; the spent
            // links of any unprocessed group remain in `exile_links` until their
            // (now-gone) source re-checks — acceptable, as multi-destination
            // returns from one source-leaves event do not occur in the pool.
            return;
        }
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "engine_trigger_target_tests.rs"]
mod trigger_target_tests;

#[cfg(test)]
#[path = "engine_exile_return_tests.rs"]
mod exile_return_tests;

#[cfg(test)]
#[path = "engine_phase_trigger_regression_tests.rs"]
mod phase_trigger_regression_tests;

#[cfg(test)]
#[path = "engine_crew_tests.rs"]
mod crew_tests;

#[cfg(test)]
#[path = "engine_station_tests.rs"]
mod station_tests;

#[cfg(test)]
#[path = "engine_keyword_action_stack_tests.rs"]
mod keyword_action_stack_tests;

#[cfg(test)]
#[path = "engine_mdfc_land_tests.rs"]
mod mdfc_land_tests;

#[cfg(test)]
mod priority_reducer_census_tests {
    use super::PriorityReducerFamily;
    use std::collections::BTreeSet;

    /// R14–R18: Freeze the independently discovered Priority-reducer surface
    /// before the private announcement facade is introduced. This scans the
    /// normal reducer source rather than deriving the result from a future
    /// facade inventory, so a new direct Priority arm cannot hide behind the
    /// inventory it is required to update.
    #[test]
    fn priority_reducer_has_the_frozen_action_family_census() {
        let source = include_str!("engine.rs");
        let start = source
            .find("(WaitingFor::Priority { player }, GameAction::PassPriority)")
            .expect("Priority reducer starts at PassPriority");
        let end_marker = "(WaitingFor::Priority { player }, GameAction::SetAutoPass { mode })";
        let end = source[start..]
            .find(end_marker)
            .map(|offset| start + offset + end_marker.len())
            .expect("Priority reducer ends at SetAutoPass");

        let mut families = BTreeSet::new();
        let mut lines_after_priority_pattern = 0usize;
        for line in source[start..end].lines() {
            if line.contains("WaitingFor::Priority { player }") {
                lines_after_priority_pattern = 6;
            }
            if lines_after_priority_pattern > 0 {
                if let Some(action) = line.split("GameAction::").nth(1) {
                    let name = action
                        .chars()
                        .take_while(|character| {
                            character.is_ascii_alphabetic() || *character == '_'
                        })
                        .collect::<String>();
                    if !name.is_empty() {
                        families.insert(name);
                    }
                }
                lines_after_priority_pattern -= 1;
            }
        }

        let expected = [
            "ActivateAbility",
            "ActivateManaSource",
            "ActivateNinjutsu",
            "ActivateStation",
            "CastPreparedCopy",
            "CastSpell",
            "CastSpellAsSneak",
            "CastSpellAsWebSlinging",
            "CastSpellForFree",
            "CompanionToHand",
            "CrewVehicle",
            "EndContinuousEffect",
            "Equip",
            "Foretell",
            "PassPriority",
            "PlayFaceDown",
            "PlayLand",
            "RollPlanarDie",
            "SaddleMount",
            "SetAutoPass",
            "TapLandForMana",
            "Transform",
            "TurnFaceUp",
            "UnlockRoomDoor",
            "UntapLandForMana",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

        assert_eq!(families, expected);
        let preflight_families = PriorityReducerFamily::ALL
            .into_iter()
            .map(|family| format!("{family:?}"))
            .collect::<BTreeSet<_>>();
        let expected_preflight_families = expected
            .iter()
            .filter(|family| *family != "PassPriority" && *family != "SetAutoPass")
            .map(|family| (*family).to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(preflight_families, expected_preflight_families);
    }
}

#[cfg(test)]
mod priority_facade_boundary_tests {
    /// R18: Provider announcements remain opaque until the one engine-owned
    /// facade. This source guard is intentionally scoped to the frozen provider
    /// set: ordinary `GameAction` producers are not part of this boundary.
    #[test]
    fn facade_access_is_constructed_once_and_provider_accessors_only_borrow_it() {
        let engine_source = include_str!("engine.rs");
        let facade_constructor = ["PriorityAnnouncementFacadeAccess::", "new()"].concat();
        assert_eq!(
            engine_source.matches(&facade_constructor).count(),
            1,
            "the facade capability must have one constructor call"
        );

        let announcement_start = engine_source
            .find("enum PriorityAnnouncement {")
            .map(|offset| offset + "enum PriorityAnnouncement {".len())
            .expect("private announcement sum exists");
        let announcement_end = engine_source[announcement_start..]
            .find("\n}\n\nimpl PriorityAnnouncement")
            .map(|offset| announcement_start + offset)
            .expect("private announcement sum closes before its family match");
        assert!(
            !engine_source[announcement_start..announcement_end].contains('{'),
            "Priority announcements must carry provider-owned opaque values, not raw fields"
        );

        for provider_source in [
            include_str!("casting.rs"),
            include_str!("mana_sources.rs"),
            include_str!("morph.rs"),
            include_str!("transform.rs"),
            include_str!("keywords.rs"),
            include_str!("room.rs"),
            include_str!("companion.rs"),
            include_str!("planechase.rs"),
            include_str!("end_continuous_effect.rs"),
            include_str!("crew_payment.rs"),
            include_str!("effects/prepare.rs"),
            include_str!("effects/attach.rs"),
        ] {
            assert!(
                !provider_source.contains("PriorityAnnouncementFacadeAccess::"),
                "providers may not construct or invoke the facade capability"
            );
            for line in provider_source
                .lines()
                .filter(|line| line.contains("PriorityAnnouncementFacadeAccess"))
            {
                assert!(
                    line.contains("use ") || line.contains("_access: &"),
                    "provider capability mentions must be imports or borrowed read-only accessors: {line}"
                );
            }
        }
    }
}

#[cfg(test)]
mod priority_principal_tests {
    use super::{
        apply_actionless_priority_pass_for_prospective, preflight_priority_window,
        priority_principal_for_preflight, PriorityPreflight, PriorityPreflightBlock,
        PriorityPreflightIndeterminate, PriorityReducerFamily,
    };
    use crate::game::zones;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{GameState, WaitingFor};
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn principal_preserves_the_controlled_priority_seat() {
        let controlled_seat = PlayerId(0);
        let controller = PlayerId(1);
        let mut state = GameState::new_two_player(42);
        state.active_player = controlled_seat;
        state.waiting_for = WaitingFor::Priority {
            player: controlled_seat,
        };
        state.turn_decision_controller = Some(controller);
        state.priority_player = controller;

        let principal = priority_principal_for_preflight(&state)
            .expect("a synchronized controlled Priority window has a principal");

        assert_eq!(principal.semantic_holder(), controlled_seat);
        assert_eq!(principal.authenticated_actor(), controller);
        assert_eq!(principal.land_resource_owner(), controlled_seat);
    }

    #[test]
    fn principal_rejects_stale_priority_authority_without_mutating_the_wait() {
        let controlled_seat = PlayerId(0);
        let controller = PlayerId(1);
        let mut state = GameState::new_two_player(42);
        state.active_player = controlled_seat;
        state.waiting_for = WaitingFor::Priority {
            player: controlled_seat,
        };
        state.turn_decision_controller = Some(controller);
        state.priority_player = controlled_seat;
        let waiting_before = state.waiting_for.clone();

        assert!(matches!(
            priority_principal_for_preflight(&state),
            Err(PriorityPreflightIndeterminate::PriorityAuthorityMismatch)
        ));
        assert_eq!(state.waiting_for, waiting_before);
    }

    #[test]
    fn principal_rejects_non_priority_without_mutating_the_wait() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::GameOver { winner: None };
        let waiting_before = state.waiting_for.clone();

        assert!(matches!(
            priority_principal_for_preflight(&state),
            Err(PriorityPreflightIndeterminate::NotPriority)
        ));
        assert_eq!(state.waiting_for, waiting_before);
    }

    #[test]
    fn priority_preflight_reports_an_actionless_empty_window() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };

        assert_eq!(
            preflight_priority_window(&state),
            PriorityPreflight::Actionless
        );
    }

    #[test]
    fn priority_preflight_reports_stale_authority_without_inventing_a_family() {
        let controlled_seat = PlayerId(0);
        let controller = PlayerId(1);
        let mut state = GameState::new_two_player(42);
        state.active_player = controlled_seat;
        state.waiting_for = WaitingFor::Priority {
            player: controlled_seat,
        };
        state.turn_decision_controller = Some(controller);
        state.priority_player = controlled_seat;
        let before = state.clone();

        assert_eq!(
            preflight_priority_window(&state),
            PriorityPreflight::Indeterminate {
                family: None,
                block: PriorityPreflightBlock::Principal(
                    PriorityPreflightIndeterminate::PriorityAuthorityMismatch,
                ),
            }
        );
        assert_eq!(state, before);
    }

    #[test]
    fn priority_preflight_clones_a_land_announcement_without_mutating_live_state() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        state.active_player = player;
        state.priority_player = player;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player };
        let land = zones::create_object(
            &mut state,
            CardId(1),
            player,
            "Island".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land)
            .expect("new hand object exists")
            .card_types
            .core_types
            .push(CoreType::Land);
        state.players[player.0 as usize].hand.push_back(land);
        let before = state.clone();

        assert_eq!(
            preflight_priority_window(&state),
            PriorityPreflight::Actionable {
                family: PriorityReducerFamily::PlayLand
            }
        );
        assert_eq!(state, before);
    }

    #[test]
    fn priority_preflight_routes_basic_land_mana_through_the_mana_provider() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };
        let forest = zones::create_object(
            &mut state,
            CardId(1),
            player,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let forest = state
            .objects
            .get_mut(&forest)
            .expect("new battlefield Forest exists");
        forest.card_types.core_types.push(CoreType::Land);
        forest.card_types.subtypes.push("Forest".to_string());
        let before = state.clone();

        assert_eq!(
            preflight_priority_window(&state),
            PriorityPreflight::Actionable {
                family: PriorityReducerFamily::TapLandForMana,
            }
        );
        assert_eq!(state, before);
    }

    #[test]
    fn prospective_actionless_pass_rejects_an_actionable_window_without_mutation() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        state.active_player = player;
        state.priority_player = player;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player };
        let land = zones::create_object(
            &mut state,
            CardId(1),
            player,
            "Island".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land)
            .expect("new hand object exists")
            .card_types
            .core_types
            .push(CoreType::Land);
        state.players[player.0 as usize].hand.push_back(land);
        let before = state.clone();

        assert!(apply_actionless_priority_pass_for_prospective(&mut state).is_err());
        assert_eq!(state, before);
    }
}

#[cfg(test)]
mod shortcut_schema_tests {
    use super::shortcut_iteration_count;
    use crate::analysis::decision_template::IterationCount;
    use crate::analysis::loop_check::WinKind;

    /// T3: `iteration_count` is exhaustive over all six `WinKind`s — the two determinate-lethal
    /// axes (CR 704.5a life / CR 704.5c poison) map to `UntilLethal`; every other win seeds
    /// `Fixed(1)`. Revert-probe: swapping any arm flips the corresponding assertion.
    #[test]
    fn iteration_count_maps_every_win_kind() {
        assert_eq!(
            shortcut_iteration_count(WinKind::LethalDamage),
            IterationCount::UntilLethal
        );
        assert_eq!(
            shortcut_iteration_count(WinKind::PoisonLoss),
            IterationCount::UntilLethal
        );
        assert_eq!(
            shortcut_iteration_count(WinKind::Decking),
            IterationCount::Fixed(1)
        );
        assert_eq!(
            shortcut_iteration_count(WinKind::ExtraTurns),
            IterationCount::Fixed(1)
        );
        assert_eq!(
            shortcut_iteration_count(WinKind::ImmediateWin),
            IterationCount::Fixed(1)
        );
        assert_eq!(
            shortcut_iteration_count(WinKind::Advantage),
            IterationCount::Fixed(1)
        );
    }
}

/// PR-7 Combo-UI Stage 2: the mid-drive pin injector (item 4) + the drive-period seam (item 6).
#[cfg(test)]
mod stage2_injector_tests {
    use super::*;
    use crate::analysis::decision_template::{
        DecisionGroupKey, DecisionKind, DecisionSlot, DecisionTemplate, IterationCount,
        PinnedDecision, ReplayMode, TargetPin, TargetSchedule,
    };
    use crate::game::scenario::GameScenario;
    use crate::types::game_state::{LoopDetectionMode, YieldTarget};

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);
    const P2: PlayerId = PlayerId(2);
    const TARGET_DRAIN: &str = "Whenever you gain life, target opponent loses that much life.";
    const FEEDBACK: &str = "Whenever an opponent loses life, you gain that much life.";
    const KICKOFF: &str = "You gain 1 life.";

    fn life(state: &GameState, p: PlayerId) -> i32 {
        state.players.iter().find(|pl| pl.id == p).unwrap().life
    }

    fn this_object(id: ObjectId) -> YieldTarget {
        YieldTarget::ThisObject {
            source_id: id,
            incarnation: None,
            trigger_description: None,
        }
    }

    /// A template routing two distinct drainers to two distinct opponents by source identity.
    fn two_drainer_template(
        d0: ObjectId,
        opp0: PlayerId,
        d1: ObjectId,
        opp1: PlayerId,
    ) -> DecisionTemplate {
        let s0 = this_object(d0);
        let s1 = this_object(d1);
        DecisionTemplate {
            owner: P0,
            decisions: vec![
                PinnedDecision::Targets {
                    slot: DecisionSlot {
                        source: s0.clone(),
                        index: 0,
                    },
                    targets: vec![TargetPin::Player(opp0)],
                },
                PinnedDecision::Targets {
                    slot: DecisionSlot {
                        source: s1.clone(),
                        index: 0,
                    },
                    targets: vec![TargetPin::Player(opp1)],
                },
            ],
            replay: ReplayMode::Scheduled {
                count: IterationCount::UntilLethal,
            },
            key: DecisionGroupKey::from_sources(&[s0, s1], DecisionKind::LoopChoice),
        }
    }

    /// Test F ⭐ (item 4 — per-source target routing, the two-authority claim): a 3p loop with
    /// TWO targeted drainers raises a `TriggerTargetSelection` per drainer (two legal opponents
    /// ⇒ not forced-unique) plus `OrderTriggers`. `inject_pinned_answer` matches EACH prompt's
    /// `source_id` to the pin for THAT drainer (not the first pin), so the two drainers hit
    /// DISTINCT opponents. Discriminator: P2 dropping proves per-source routing — a first-pin
    /// injector would drain only P1.
    #[test]
    fn injector_routes_pinned_targets_per_source() {
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        scenario.with_life(P1, 500);
        scenario.with_life(P2, 500);
        let drainer_a = scenario
            .add_creature_from_oracle(P0, "Drainer A", 1, 4, TARGET_DRAIN)
            .id();
        let drainer_b = scenario
            .add_creature_from_oracle(P0, "Drainer B", 2, 2, TARGET_DRAIN)
            .id();
        scenario.add_creature_from_oracle(P0, "Feedback", 3, 4, FEEDBACK);
        let kickoff = scenario
            .add_spell_to_hand_from_oracle(P0, "Kickoff", false, KICKOFF)
            .id();
        let mut runner = scenario.build();
        // Off: drive the raw cascade directly through the injector (no offer/auto-win path).
        runner.state_mut().loop_detection = LoopDetectionMode::Off;
        // Cast the seed lifegain via the INTERNAL path (the CastBuilder's auto-resolver cannot
        // satisfy the non-forced-unique 2-opponent target prompt — that is exactly the arm the
        // injector is under test for).
        let card_id = runner.state().objects.get(&kickoff).unwrap().card_id;
        apply_action(
            runner.state_mut(),
            P0,
            GameAction::CastSpell {
                object_id: kickoff,
                card_id,
                targets: vec![],
                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            },
            None,
        )
        .expect("cast the seed lifegain");

        let template = two_drainer_template(drainer_a, P1, drainer_b, P2);

        // The target each drainer's trigger actually got, read off the stack right after the
        // injector answered its prompt (independent of drain-resolution order).
        let target_on_stack = |state: &GameState, src: ObjectId| -> Option<Vec<TargetRef>> {
            state
                .stack
                .iter()
                .find(|e| e.source_id == src)
                .and_then(|e| match &e.kind {
                    crate::types::game_state::StackEntryKind::TriggeredAbility {
                        ability, ..
                    } => Some(ability.targets.clone()),
                    _ => None,
                })
        };
        let mut a_target: Option<Vec<TargetRef>> = None;
        let mut b_target: Option<Vec<TargetRef>> = None;

        for _ in 0..40 {
            let wf = runner.state().waiting_for.clone();
            match wf {
                WaitingFor::Priority { player } => {
                    apply_action(runner.state_mut(), player, GameAction::PassPriority, None)
                        .expect("pass priority");
                }
                WaitingFor::OrderTriggers { .. } => {
                    inject_pinned_answer(runner.state_mut(), None, 0, &wf)
                        .expect("OrderTriggers arm is template-INDEPENDENT (None is fine)");
                }
                WaitingFor::TriggerTargetSelection { source_id, .. } => {
                    // Guard: at a target prompt, a None template fails CLOSED (the guard lives
                    // in THIS arm, not at the top of the injector).
                    assert!(
                        inject_pinned_answer(&mut runner.state().clone(), None, 0, &wf).is_err(),
                        "template=None must abort the TriggerTargetSelection arm"
                    );
                    inject_pinned_answer(runner.state_mut(), Some(&template), 0, &wf)
                        .expect("pinned target injected");
                    let src = source_id.expect("targeted trigger has a source");
                    if src == drainer_a {
                        a_target = target_on_stack(runner.state(), src);
                    } else if src == drainer_b {
                        b_target = target_on_stack(runner.state(), src);
                    }
                }
                _ => break,
            }
            if a_target.is_some() && b_target.is_some() {
                break;
            }
        }

        // Per-source routing: each drainer's trigger got ITS OWN pinned opponent — a first-pin
        // injector would route both to P1.
        assert_eq!(
            a_target,
            Some(vec![TargetRef::Player(P1)]),
            "Drainer A's trigger routed to its pinned P1"
        );
        assert_eq!(
            b_target,
            Some(vec![TargetRef::Player(P2)]),
            "Drainer B's trigger routed to its pinned P2 (per-source, not first-pin)"
        );
    }

    /// Test F (production-path twin, item 4): drive a primed 3p targeted loop through the REAL
    /// `drive_one_shortcut_cycle` and confirm its `Ok(other)` arm routes to the injector. Both
    /// pinned opponents drain to death in the driven cycle ⇒ `CrossLethal{winner: Some(P0)}`,
    /// which is REACHABLE ONLY if each drainer's trigger hit its OWN pinned opponent (a
    /// first-pin injector would drain only P1, leaving P2 alive and no single winner).
    #[test]
    fn drive_one_cycle_reaches_injector_for_3p_targeted() {
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        scenario.with_life(P1, 400);
        scenario.with_life(P2, 400);
        let drainer_a = scenario
            .add_creature_from_oracle(P0, "Drainer A", 1, 4, TARGET_DRAIN)
            .id();
        let drainer_b = scenario
            .add_creature_from_oracle(P0, "Drainer B", 2, 2, TARGET_DRAIN)
            .id();
        scenario.add_creature_from_oracle(P0, "Feedback", 3, 4, FEEDBACK);
        let kickoff = scenario
            .add_spell_to_hand_from_oracle(P0, "Kickoff", false, KICKOFF)
            .id();
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Off;
        let card_id = runner.state().objects.get(&kickoff).unwrap().card_id;
        apply_action(
            runner.state_mut(),
            P0,
            GameAction::CastSpell {
                object_id: kickoff,
                card_id,
                targets: vec![],
                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            },
            None,
        )
        .expect("cast seed");

        // Prime: drive (targeting P1 for anything) until a Priority{P0} beat with a pending
        // cascade — the settle beat the drive re-fires from.
        let prime = two_drainer_template(drainer_a, P1, drainer_b, P1);
        let mut primed = false;
        for _ in 0..40 {
            let wf = runner.state().waiting_for.clone();
            match wf {
                WaitingFor::Priority { player }
                    if player == P0 && !runner.state().stack.is_empty() =>
                {
                    primed = true;
                    break;
                }
                WaitingFor::Priority { player } => {
                    apply_action(runner.state_mut(), player, GameAction::PassPriority, None)
                        .unwrap();
                }
                WaitingFor::OrderTriggers { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                    inject_pinned_answer(runner.state_mut(), Some(&prime), 0, &wf).unwrap();
                }
                _ => break,
            }
        }
        assert!(primed, "must reach a primed Priority{{P0}} settle beat");

        // Reset opponents to equal LOW life so the driven cycle crosses lethal (both die) —
        // reachable only if each drainer hits its own pinned opponent.
        for p in [P1, P2] {
            runner
                .state_mut()
                .players
                .iter_mut()
                .find(|pl| pl.id == p)
                .unwrap()
                .life = 8;
        }
        let committed = runner.state().clone();
        let boundary = {
            let mut seed = committed.clone();
            priority::reset_priority(&mut seed);
            seed.waiting_for = WaitingFor::Priority {
                player: seed.active_player,
            };
            seed.normalize_for_loop()
        };
        let template = two_drainer_template(drainer_a, P1, drainer_b, P2);
        let cap = auto_pass_loop_max_iterations(&committed);

        // `None`: this row is about the injector arm on a board-recurring targeted loop, so
        // it drives under the same no-signature delimiter every pre-bounded offer uses.
        match drive_one_shortcut_cycle(&committed, &boundary, Some(&template), 0, cap, None) {
            CycleOutcome::CrossLethal { winner, state, .. } => {
                assert_eq!(
                    winner,
                    Some(P0),
                    "both pinned opponents drained to death ⇒ P0 sole winner (per-source \
                     routing through the production drive)"
                );
                assert!(
                    life(&state, P1) <= 0 && life(&state, P2) <= 0,
                    "both opponents at 0-or-less"
                );
            }
            CycleOutcome::Recurred { state, .. } => {
                assert!(
                    life(&state, P1) < 8 && life(&state, P2) < 8,
                    "both pinned opponents drained through drive_one_shortcut_cycle"
                );
            }
            CycleOutcome::Abort => panic!("the pinned drive must not abort"),
        }
    }

    /// Item 6: `shortcut_drive_period` = the max schedule length over the template's target
    /// pins (Constant/Player/ByIdentity ⇒ 1), defaulting to 1 (no template / non-target pins).
    #[test]
    fn shortcut_drive_period_is_schedule_max() {
        assert_eq!(shortcut_drive_period(None), 1, "no template ⇒ period 1");

        let a = this_object(ObjectId(1));
        let b = this_object(ObjectId(2));
        let c = this_object(ObjectId(3));
        let slot = DecisionSlot {
            source: a.clone(),
            index: 0,
        };
        let mk = |targets: Vec<TargetPin>| DecisionTemplate {
            owner: P0,
            decisions: vec![PinnedDecision::Targets {
                slot: slot.clone(),
                targets,
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::UntilLethal,
            },
            key: DecisionGroupKey::from_sources(std::slice::from_ref(&a), DecisionKind::LoopChoice),
        };

        let constant = mk(vec![TargetPin::Player(P1)]);
        assert_eq!(shortcut_drive_period(Some(&constant)), 1, "Player pin ⇒ 1");

        let rr = mk(vec![TargetPin::Scheduled(TargetSchedule::RoundRobin(
            vec![a.clone(), b.clone(), c.clone()],
        ))]);
        assert_eq!(shortcut_drive_period(Some(&rr)), 3, "RoundRobin(3) ⇒ 3");

        let pw = mk(vec![TargetPin::Scheduled(TargetSchedule::Piecewise(vec![
            (0, a.clone()),
            (5, b.clone()),
        ]))]);
        assert_eq!(shortcut_drive_period(Some(&pw)), 2, "Piecewise(2) ⇒ 2");

        // CR 732.2a SAFETY LIMIT: an over-cap schedule clamps to MAX_SHORTCUT_CYCLES.
        // Revert-probe: restore `.max(1)` (drop the `.clamp`) ⇒ returns MAX+5 (1005) ≠ 1000.
        let oversized = mk(vec![TargetPin::Scheduled(TargetSchedule::RoundRobin(
            vec![a.clone(); (MAX_SHORTCUT_CYCLES + 5) as usize],
        ))]);
        assert_eq!(
            shortcut_drive_period(Some(&oversized)),
            MAX_SHORTCUT_CYCLES,
            "RoundRobin(MAX+5) clamps to MAX_SHORTCUT_CYCLES"
        );
    }

    /// Place a bare object in `zone` without touching the zone vectors — enough for the
    /// identity/zone predicates under test, which read `state.objects` only.
    fn place(state: &mut GameState, id: u64, zone: crate::types::zones::Zone) -> ObjectId {
        let oid = ObjectId(id);
        let mut o = crate::game::game_object::GameObject::new(
            oid,
            CardId(0),
            P0,
            "Emblem".to_string(),
            zone,
        );
        o.incarnation = 3;
        state.objects.insert(oid, o);
        oid
    }

    /// CR 114.2 + CR 608.2b: a pinned SLOT whose source is a command-zone emblem must match
    /// the prompt that emblem raised; a graveyard or exile source must NOT.
    ///
    /// This is the zone predicate `inject_pinned_answer`'s `TriggerTargetSelection` arm
    /// dispatches on. Its production drive lands with the bounded offer in a later commit,
    /// so it is pinned here at the seam — the shipped BATTLEFIELD arm is exercised
    /// end-to-end by `injector_routes_pinned_targets_per_source` above and by the
    /// `kilo_live_offer_from_real_dump` rows, and this row asserts that arm is unchanged.
    ///
    /// REVERT-PROBES: (a) delete the command-zone disjunct in `slot_source_prompted` ⇒ the
    /// Command assertion FAILS (and `inject_pinned_answer` would `RecastAbort` on an
    /// emblem-pinned drive); (b) widen the disjunct to accept any zone ⇒ the graveyard and
    /// exile assertions FAIL; (c) drop the incarnation conjunct ⇒ the CR 400.7 assertion
    /// FAILS.
    #[test]
    fn command_zone_sourced_slot_matches_and_graveyard_still_aborts() {
        use crate::types::zones::Zone;
        let mut state = GameScenario::new_n_player(2, 7).build().state().clone();
        let battlefield = place(&mut state, 900, Zone::Battlefield);
        let emblem = place(&mut state, 901, Zone::Command);
        let graveyard = place(&mut state, 902, Zone::Graveyard);
        let exiled = place(&mut state, 903, Zone::Exile);

        let pin = |id: ObjectId, inc: Option<u64>| YieldTarget::ThisObject {
            source_id: id,
            incarnation: inc,
            trigger_description: None,
        };

        // Shipped behaviour, unchanged: the battlefield arm still matches.
        assert!(
            slot_source_prompted(&state, &pin(battlefield, Some(3)), battlefield),
            "the shipped CR 608.2b battlefield arm must be untouched"
        );
        // NEW: CR 114.2 — an emblem lives in the command zone and prompts from there.
        assert!(
            slot_source_prompted(&state, &pin(emblem, Some(3)), emblem),
            "CR 114.2: a command-zone emblem's slot must match the prompt it raised"
        );
        // Fail-closed: every other off-battlefield zone still misses ⇒ `RecastAbort`.
        assert!(
            !slot_source_prompted(&state, &pin(graveyard, Some(3)), graveyard),
            "a graveyard-sourced slot must NOT match — the drive aborts to manual"
        );
        assert!(
            !slot_source_prompted(&state, &pin(exiled, Some(3)), exiled),
            "an exile-sourced slot must NOT match"
        );
        // CR 400.7: the command arm re-binds ONE incarnation, exactly like the
        // battlefield arm — a re-created emblem does not answer the old pin.
        assert!(
            !slot_source_prompted(&state, &pin(emblem, Some(2)), emblem),
            "CR 400.7: a stale incarnation must not match even in the command zone"
        );
        // A pin naming a DIFFERENT object never answers this prompt.
        assert!(
            !slot_source_prompted(&state, &pin(emblem, Some(3)), battlefield),
            "the matcher is keyed on identity, not merely on zone"
        );
    }

    /// CR 732.2a + CR 603.5: `bounded_cycle_pin_slots` publishes the per-iteration TARGET
    /// choice for a proposer-controlled player-targeting trigger, plus a second `MayChoice`
    /// point (disambiguated by `slot.index`) when that trigger is optional.
    ///
    /// MATCHED PAIRS, one variable each:
    /// * `optional` false ⇒ 1 point; true ⇒ 2 points. REVERT-PROBE: delete the `optional`
    ///   branch ⇒ the 2-point assertion FAILS.
    /// * controller == proposer ⇒ published; a bystander proposer gets nothing.
    ///   REVERT-PROBE: delete the `entry.controller != proposer` filter ⇒ the bystander
    ///   assertion FAILS.
    #[test]
    fn bounded_cycle_pin_slots_publishes_the_may_gate_of_an_optional_trigger() {
        use crate::analysis::decision_template::{DecisionPointKind, DecisionSlot};
        use crate::types::ability::{
            ControllerRef, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TypedFilter,
        };

        let mut state = GameScenario::new_n_player(3, 7).build().state().clone();
        let src = place(&mut state, 910, crate::types::zones::Zone::Battlefield);

        let entry = |id: u64, controller: PlayerId, optional: bool| {
            let mut ability = ResolvedAbility::new(
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: Some(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![],
                        controller: Some(ControllerRef::Opponent),
                        properties: vec![],
                    })),
                },
                vec![],
                src,
                controller,
            );
            ability.optional = optional;
            StackEntry {
                id: ObjectId(id),
                source_id: src,
                controller,
                kind: StackEntryKind::TriggeredAbility {
                    source_id: src,
                    ability: Box::new(ability),
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: String::new(),
                    subject_match_count: None,
                    die_result: None,
                },
            }
        };
        let live_source =
            object_decision_source(&state, src).expect("the source is on the battlefield");
        let expected_slot = |index: u8| DecisionSlot {
            source: live_source.clone(),
            index,
        };

        // Mandatory: exactly one point, the target choice.
        state.stack.push_back(entry(920, P0, false));
        let mandatory = bounded_cycle_pin_slots(&state, P0);
        assert_eq!(
            mandatory.len(),
            1,
            "a mandatory trigger publishes one point"
        );
        assert_eq!(mandatory[0].slot, expected_slot(0));
        assert!(
            matches!(
                &mandatory[0].kind,
                DecisionPointKind::Targets { legal_targets, .. }
                    if *legal_targets == vec![TargetRef::Player(P1), TargetRef::Player(P2)]
            ),
            "the legal set comes from `find_legal_targets`, not from a declaration: {:?}",
            mandatory[0].kind
        );

        // Bystander proposer: the same board publishes nothing for a seat that controls
        // none of the entries (CR 732.2a — the proposer specifies their OWN choices).
        assert!(
            bounded_cycle_pin_slots(&state, P1).is_empty(),
            "a bystander proposer controls none of these choices"
        );

        // Optional: the SAME entry with one field flipped publishes the CR 603.5 gate too.
        state.stack.clear();
        state.stack.push_back(entry(921, P0, true));
        let optional = bounded_cycle_pin_slots(&state, P0);
        assert_eq!(
            optional.len(),
            2,
            "an optional trigger publishes two points"
        );
        assert_eq!(
            optional[1].slot,
            expected_slot(1),
            "`slot.index` disambiguates"
        );
        assert_eq!(optional[1].kind, DecisionPointKind::MayChoice);

        // Fail-closed: no source object ⇒ no point (never a slot that cannot re-bind).
        state.objects.remove(&src);
        assert!(
            bounded_cycle_pin_slots(&state, P0).is_empty(),
            "an absent source emits nothing rather than an unbindable slot"
        );
    }

    /// Each of [`declares_opponent_player_target`]'s three conjuncts, discriminated
    /// SEPARATELY. The gross "accept any `Typed`" widening is caught by the row above; this
    /// row is what fails when ONE conjunct is dropped.
    ///
    /// Why each matters: `find_legal_targets` collapses a `Typed` filter to PLAYERS ONLY
    /// when both `type_filters` and `properties` are empty (`targeting.rs:192-193`, issue
    /// #2004). A type- or property-bearing filter therefore falls through to OBJECT
    /// enumeration — publishing it would put a point whose legal set is object refs into
    /// player-pin machinery. `controller: You` does collapse to players, but to exactly ONE
    /// (the controller), which is not the per-opponent choice a bounded drain cycle pins.
    ///
    /// THE BOARD IS LOAD-BEARING, and picking the wrong one is what hollows this row out.
    /// On a head-announced board an object-shaped filter enumerates NOTHING, so
    /// `build_target_slots` returns `Err` and the caller's CARDINALITY conjunct rejects
    /// first — measured: with both arms on that board, dropping `type_filters.is_empty()` or
    /// `properties.is_empty()` left the row GREEN. Those two arms therefore run on the
    /// MIRROR shape, the one thing they alone reject: an object-shaped head at
    /// `TargetChoiceTiming::Resolution` (announces nothing) chained to a `target opponent`
    /// sub-ability (announces the one slot). Cardinality and all-`Player` both pass there.
    /// The per-arm reach-guard asserts that announced slot verbatim, so a future change that
    /// re-hollows an arm fails the reach-guard instead of passing silently.
    ///
    /// REVERT-PROBES (each measured on the boards below): drop `type_filters.is_empty()` ⇒
    /// ONLY the Creature arm publishes; drop `properties.is_empty()` ⇒ ONLY the Token arm;
    /// drop `controller == Some(Opponent)` ⇒ ONLY the `You` arm. The accepted shape is
    /// asserted to publish in the SAME row, so a constant-`false` predicate cannot pass.
    #[test]
    fn bounded_cycle_pin_slots_conjuncts_are_each_load_bearing() {
        use crate::types::ability::{
            ControllerRef, Effect, FilterProp, QuantityExpr, ResolvedAbility, TargetChoiceTiming,
            TargetFilter, TargetRef, TypeFilter, TypedFilter,
        };

        let mut base = GameScenario::new_n_player(3, 7).build().state().clone();
        let src = place(&mut base, 940, crate::types::zones::Zone::Battlefield);

        let board = |ability: ResolvedAbility| {
            let mut state = base.clone();
            state.stack.push_back(StackEntry {
                id: ObjectId(950),
                source_id: src,
                controller: P0,
                kind: StackEntryKind::TriggeredAbility {
                    source_id: src,
                    ability: Box::new(ability),
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: String::new(),
                    subject_match_count: None,
                    die_result: None,
                },
            });
            state
        };
        let accepted = TypedFilter {
            type_filters: vec![],
            controller: Some(ControllerRef::Opponent),
            properties: vec![],
        };
        let head_only = |tf: TypedFilter| {
            ResolvedAbility::new(
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: Some(TargetFilter::Typed(tf)),
                },
                vec![],
                src,
                P0,
            )
        };
        // The head's own choice is made on resolution, so CR 601.2c is never reached for it
        // and it announces no slot; the ONE announced slot is the chained sub-ability's
        // `target opponent` player choice (CR 603.3d). Cardinality and all-`Player` pass
        // whatever shape the head declares — which is what leaves the head-shape conjunct
        // alone to reject.
        let chained = |head: TypedFilter| {
            let mut ability = head_only(head);
            ability.target_choice_timing = TargetChoiceTiming::Resolution;
            ability.sub_ability = Some(Box::new(head_only(accepted.clone())));
            ability
        };

        // POSITIVE CONTROL, same row: the accepted "target opponent" shape publishes.
        assert_eq!(
            bounded_cycle_pin_slots(&board(head_only(accepted.clone())), P0).len(),
            1,
            "the accepted CR 115.2 player shape must publish — otherwise the three zeros \
             below are vacuous"
        );

        let opponents = vec![TargetRef::Player(P1), TargetRef::Player(P2)];
        // Collected, not asserted per-arm: a revert-probe must show which arms flipped, and
        // a bare `assert!` in the loop would abort at the first and hide the rest.
        let mut still_published: Vec<&str> = Vec::new();
        for (label, ability, announced) in [
            (
                "type_filters: a creature filter enumerates OBJECTS, not players",
                chained(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    ..accepted.clone()
                }),
                opponents.clone(),
            ),
            (
                "properties: issue #2004 — `token` is an object characteristic",
                chained(TypedFilter {
                    properties: vec![FilterProp::Token],
                    ..accepted.clone()
                }),
                opponents.clone(),
            ),
            (
                "controller: `You` is a single forced seat, not a per-opponent choice",
                head_only(TypedFilter {
                    controller: Some(ControllerRef::You),
                    ..accepted.clone()
                }),
                vec![TargetRef::Player(P0)],
            ),
        ] {
            let state = board(ability);
            let announcement = crate::game::ability_utils::build_target_slots(
                &state,
                state.stack[0]
                    .ability()
                    .expect("the board pushes a trigger"),
            )
            .map(|slots| {
                slots
                    .iter()
                    .map(|slot| (slot.optional, slot.legal_targets.clone()))
                    .collect::<Vec<_>>()
            })
            .ok();
            assert_eq!(
                announcement,
                Some(vec![(false, announced)]),
                "reach-guard [{label}]: exactly ONE mandatory slot and every candidate a \
                 PLAYER — so the cardinality and all-`Player` conjuncts both PASS on this \
                 board and the head-shape conjunct under test is the SOLE rejector"
            );
            if !bounded_cycle_pin_slots(&state, P0).is_empty() {
                still_published.push(label);
            }
        }
        assert!(
            still_published.is_empty(),
            "each conjunct must reject its own arm ALONE; these arms published anyway: \
             {still_published:?}"
        );

        // ORDERING-INPUT CONJUNCTS. The published point declares `min/max_targets: 1`, and
        // the gate-(3) relief is a bare `continue` that discharges the WHOLE of
        // `stack_entry_has_no_ordering_input` (analysis/resource.rs) — which rejects on
        // four facts, only one of which a slot answers. The three ABILITY facts must block
        // publication outright (the state-dependent fourth, `pending_trigger_entry`, is the
        // relief's, pinned by `a_pinned_slot_skips_gate_three_and_six`'s arm 5). Shares the
        // positive control above.
        {
            use crate::types::ability::MultiTargetSpec;
            use crate::types::game_state::TargetSelectionConstraint;

            fn ability_of(state: &mut GameState) -> &mut ResolvedAbility {
                let StackEntryKind::TriggeredAbility { ability, .. } = &mut state.stack[0].kind
                else {
                    unreachable!("the fixture board pushes a TriggeredAbility")
                };
                ability.as_mut()
            }

            type Mutate = fn(&mut GameState);
            let ordering_input: [(&str, Mutate); 3] = [
                ("multi_target — CR 601.2c variable target count", |s| {
                    ability_of(s).multi_target = Some(MultiTargetSpec::fixed(1, 2))
                }),
                ("distribution — CR 601.2d divide-among", |s| {
                    ability_of(s).distribution = Some(vec![(TargetRef::Player(P1), 1)])
                }),
                ("target_constraints — CR 601.2c cross-target", |s| {
                    ability_of(s).target_constraints =
                        vec![TargetSelectionConstraint::DifferentTargetPlayers]
                }),
            ];
            for (label, mutate) in ordering_input {
                let mut state = board(head_only(accepted.clone()));
                mutate(&mut state);
                assert!(
                    bounded_cycle_pin_slots(&state, P0).is_empty(),
                    "{label}: announcement-time ordering input NO published slot specifies \
                     ⇒ the mint must not publish"
                );
            }
        }
    }

    /// The real 4p acceptance board (dump B): a CR 114.2 emblem in the COMMAND zone
    /// (obj 541, incarnation 0) whose triggered ability drains `target opponent`.
    fn load_dellian_dump() -> GameState {
        use crate::types::game_state::PersistedGameState;
        use std::io::Read;
        let gz: &[u8] = include_bytes!("../../tests/fixtures/dellian_emblem_conqueror_4p.json.gz");
        let mut json = String::new();
        flate2::read::GzDecoder::new(gz)
            .read_to_string(&mut json)
            .expect("fixture inflates");
        let envelope: serde_json::Value = serde_json::from_str(&json).expect("envelope parses");
        // Cross the dump through the PRODUCTION decoder rather than a bare `GameState`
        // decode wrapped in `Raw`: `PersistedGameState`'s own `Deserialize` runs
        // `reject_legacy_raw_prompt_authority` and `decode_persisted_resolution_state`
        // first, so this helper exercises the chokepoint the server's `from_persisted`
        // and WASM's `decode_restored_game_state` actually funnel through — including
        // the CR 732.2a load-seam bound invariant.
        // `.expect(..)`, not `?`: `into_game_state` returns `GameState`, not `Result`.
        serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
            .expect("gameState deserializes through the production decoder")
            .into_game_state()
    }

    const EMBLEM: ObjectId = ObjectId(541);

    /// CR 601.2c + CR 115.1: the migrated fixture's `effect_kind` is not an assertion of
    /// taste — it is exactly what the ANNOUNCEMENT AUTHORITY builds for this ability on
    /// this board. `scripts/migrate-dump-fixture.sh` takes the value as an explicit
    /// argument precisely so the engine, and never a jq name→variant table, stays the
    /// authority for it; this row is what holds the script's operator to that.
    ///
    /// It is also the row that would have caught upstream #6718 (`0468df1f4`, which added
    /// `TargetSelectionSlot::effect_kind` with no `#[serde(default)]`) the day it landed:
    /// `TargetSelectionSlot` derives `PartialEq`/`Eq`, so this compares ALL five fields,
    /// not just the migrated one.
    ///
    /// REVERT-PROBE: re-run the migration script with `--effect-kind NoOp` ⇒ the stamped
    /// slot stops matching what `build_target_slots` derives ⇒ FAILS. This assertion is
    /// the SOLE guard on the migrated value — nothing on the drive path reads the field
    /// (its only production readers are the two `target_intent` calls in the interaction
    /// DTO projection), so a wrong value cannot move the game and must be caught by a
    /// reading test instead of by a behavioural one.
    #[test]
    fn dellian_dump_slots_are_what_the_announcement_authority_builds() {
        let state = load_dellian_dump();
        let pt = state
            .pending_trigger
            .as_ref()
            .expect("dellian dump pauses on a pending trigger");
        // `let … else` rather than `if let`: it fails LOUDLY if the dump ever restores
        // into some other prompt, which would otherwise make the assertion unreachable
        // rather than false.
        let WaitingFor::TriggerTargetSelection { target_slots, .. } = &state.waiting_for else {
            panic!(
                "dellian dump must restore into TriggerTargetSelection, got {:?}",
                state.waiting_for
            );
        };
        // Reach-guard: an empty slot vector would satisfy a total-equality assertion
        // vacuously on both sides.
        assert_eq!(
            target_slots.len(),
            1,
            "the dellian dump publishes exactly one target slot"
        );
        assert_eq!(
            &crate::game::ability_utils::build_target_slots(&state, &pt.ability)
                .expect("the emblem's drain ability builds its announcement slots"),
            target_slots,
            "the restored dump's slots must equal what the announcement authority builds"
        );
    }

    /// CR 732.2a: the offer publishes the SET of open per-iteration choices — one point per
    /// SOURCE, not one per stack ENTRY.
    ///
    /// `DecisionSlot`'s sub-index disambiguates two choices of ONE ability instance, so N
    /// entries from one source would mint N byte-identical slots: N identical frontend
    /// pickers, and `predictability_gate` demanding N pins for a choice
    /// `inject_pinned_answer` answers ONCE per source (its `find_map` matches on the slot's
    /// SOURCE and is index-blind). Real boards reach this shape — this very dump carries 35
    /// entries on source 25, 34 on 126 and 34 on 208.
    ///
    /// Built on the LOADED 4p board plus ONE measured mutation: a byte-copy of the real
    /// emblem entry under a fresh stack-entry id, which is exactly what a second loop
    /// iteration puts there.
    ///
    /// REVERT-PROBE: drop either `points.iter().any(|p| p.slot == ..)` dedupe guard ⇒ the
    /// two-entry board publishes 2 points ⇒ FAILS.
    #[test]
    fn bounded_cycle_pin_slots_publishes_one_point_per_source_not_per_entry() {
        let mut state = load_dellian_dump();
        let emblem_entry = state
            .stack
            .iter()
            .find(|e| e.source_id == EMBLEM)
            .expect("reach-guard: the dump carries the emblem's trigger")
            .clone();
        let single = bounded_cycle_pin_slots(&state, P0);
        // Reach-guard, re-derived from the loaded board rather than from a literal: the
        // mint qualifies FOUR sources here — the CR 115.2 emblem target (541) plus three
        // shape-(B) CR 603.5 may-only sources (126, 208, 274). 126 and 208 carry 34 stack
        // entries apiece, so the shipped board ALREADY exercises the dedupe: without it
        // this count would be 69, not 4.
        assert_eq!(
            single.len(),
            4,
            "reach-guard: the shipped board qualifies exactly four sources"
        );
        assert_eq!(
            single
                .iter()
                .filter(|p| matches!(
                    p.kind,
                    crate::analysis::decision_template::DecisionPointKind::Targets { .. }
                ))
                .count(),
            1,
            "reach-guard: exactly one of them is the emblem's TARGETS point"
        );

        let mut second = emblem_entry;
        second.id = ObjectId(9_001);
        state.stack.push_back(second);
        assert_eq!(
            state.stack.iter().filter(|e| e.source_id == EMBLEM).count(),
            2,
            "reach-guard: two live entries now share one source"
        );

        assert_eq!(
            bounded_cycle_pin_slots(&state, P0),
            single,
            "a second entry from the SAME source is the same open choice — one published \
             point, byte-identical to the one-entry board's"
        );
    }

    /// CR 114.2 + CR 608.2b, on a REAL restored 4p board: `inject_pinned_answer` accepts a
    /// pin whose slot source is the COMMAND-zone emblem (obj 541) that raised the prompt.
    ///
    /// This is the production-path row for [`slot_source_prompted`]. The seam is live
    /// TODAY: `inject_pinned_answer` calls it, and every pin-recording site builds its slot
    /// source with the zone-agnostic `object_decision_source`, so a command-zone-sourced pin
    /// already flips from `RecastAbort` (safe handback) to accepted injection.
    ///
    /// The dump ships AT that prompt (`TriggerTargetSelection { source_id: 541 }`), so no
    /// synthetic placement is involved.
    ///
    /// REVERT-PROBES (each measured): delete the command-zone disjunct ⇒ the accept arm
    /// raises `RecastAbort` ⇒ FAILS; drop the incarnation conjunct ⇒ the stale-pin arm is
    /// accepted ⇒ FAILS. The negative arms are paired with a positive on the SAME board, so
    /// neither an always-accept nor an always-abort matcher survives.
    ///
    /// R3 — CR 608.2b + CR 113.7a: this row is ALSO the witness that a pin's SEAT legality
    /// does not depend on recovering the SOURCE's characteristics. The choice authority
    /// `game::players::player_exists_for_choice` takes no source parameter at all, by
    /// design: a command-zone emblem has no battlefield LKI to recover, and if seat
    /// legality consulted the source, a pin raised by such a source would be invalidated
    /// for a reason the rules do not state. The claim is STRUCTURAL — there is no source
    /// parameter to revert — so what makes it checkable is the PAIR: this row proves a
    /// source with no recoverable LKI still answers its prompt, and
    /// `analysis::decision_template::tests::a_dead_player_pin_is_illegal`'s LIVE half
    /// proves the seat check is nonetheless doing work on that same pin kind. (Contrast
    /// `targeting::player_is_legal_target`, which DOES take a source — because targeting
    /// exclusions are source-relative, CR 702.11c, and choice legality is not.)
    #[test]
    fn a_command_zone_pin_answers_a_real_restored_boards_prompt() {
        let state = load_dellian_dump();

        // ── reach guards, all read off the loaded board ──
        let emblem = state
            .objects
            .get(&EMBLEM)
            .expect("reach-guard: dump B carries the emblem object");
        assert_eq!(
            emblem.zone,
            crate::types::zones::Zone::Command,
            "reach-guard: CR 114.2 puts the emblem in the command zone"
        );
        let emblem_incarnation = emblem.incarnation;
        let WaitingFor::TriggerTargetSelection {
            source_id: Some(prompt_source),
            ..
        } = &state.waiting_for
        else {
            panic!(
                "reach-guard: the dump ships at the emblem's target prompt; got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(
            *prompt_source, EMBLEM,
            "reach-guard: it is the EMBLEM's prompt that is up"
        );

        let src = object_decision_source(&state, EMBLEM).expect("the emblem object exists");
        // The control that makes this row non-vacuous: the shipped battlefield-only
        // `resolve_source` does NOT match this source, so an accept can only come from the
        // CR 114.2 disjunct.
        assert_eq!(
            crate::analysis::decision_template::resolve_source(&src, &state),
            None,
            "CR 608.2b: `resolve_source` is battlefield-only and must stay so"
        );

        let template = |source: YieldTarget| DecisionTemplate {
            owner: P0,
            decisions: vec![PinnedDecision::Targets {
                slot: DecisionSlot {
                    source: source.clone(),
                    index: 0,
                },
                targets: vec![TargetPin::Player(P1)],
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::UntilLethal,
            },
            key: DecisionGroupKey::from_sources(&[source], DecisionKind::LoopChoice),
        };
        let prompt = state.waiting_for.clone();

        // ── ACCEPT: the command-zone pin answers the prompt on the real board ──
        let mut work = state.clone();
        inject_pinned_answer(&mut work, Some(&template(src.clone())), 0, &prompt)
            .expect("CR 114.2: the emblem's own pin must answer the prompt it raised");
        assert_ne!(
            work.waiting_for, prompt,
            "the prompt was actually consumed, not silently skipped"
        );
        assert!(
            !matches!(
                &work.waiting_for,
                WaitingFor::TriggerTargetSelection {
                    source_id: Some(id),
                    ..
                } if *id == EMBLEM
            ),
            "the emblem's target prompt is answered; got {:?}",
            work.waiting_for
        );

        // ── CR 400.7: a pin latched to a stale incarnation must NOT answer it ──
        let stale = YieldTarget::ThisObject {
            source_id: EMBLEM,
            incarnation: Some(emblem_incarnation + 1),
            trigger_description: None,
        };
        let mut stale_work = state.clone();
        assert!(
            inject_pinned_answer(&mut stale_work, Some(&template(stale)), 0, &prompt).is_err(),
            "CR 400.7: a stale-incarnation pin hands back to manual play"
        );

        // ── fail-closed: a pin naming a DIFFERENT object never answers this prompt ──
        let other = state
            .stack
            .iter()
            .map(|e| e.source_id)
            .find(|id| *id != EMBLEM)
            .expect("the 152-deep stack carries other sources");
        let other_src = object_decision_source(&state, other).expect("that source exists");
        let mut other_work = state.clone();
        assert!(
            inject_pinned_answer(&mut other_work, Some(&template(other_src)), 0, &prompt).is_err(),
            "a pin for another source does not answer the emblem's prompt"
        );
    }

    // ───────────────────────── 5d U2 — the shape-(B) mint ─────────────────────────

    use crate::types::ability::ResolvedAbility;

    /// A 3-seat board plus one battlefield source object, shared by every U2 mint row so a
    /// row's verdict cannot come from board differences.
    fn u2_board() -> (GameState, ObjectId) {
        let mut state = GameScenario::new_n_player(3, 7).build().state().clone();
        let src = place(&mut state, 930, crate::types::zones::Zone::Battlefield);
        (state, src)
    }

    /// A proposer-controlled OPTIONAL, NO-TARGET triggered ability — shape (B)'s own shape.
    ///
    /// `target_choice_timing: Resolution` is what makes it shape (B), and it is the class's
    /// real rules shape rather than a test convenience: CR 601.2c announces only DECLARED
    /// targets, so Braids, Conjurer Adept — "At the beginning of each player's upkeep, that
    /// player may put an artifact, creature, or land card from their hand onto the
    /// battlefield." (CR 503.1a + CR 608.2d; text verified against Scryfall) — chooses its
    /// subject AT RESOLUTION and surfaces zero announcement slots.
    ///
    /// The `Effect::PutCounter` fixtures below are a SYNTHETIC STAND-IN, never Braids'
    /// printed effect: they exercise the same CLASS (per-player-upkeep optional,
    /// resolution-time subject, zero announcement slots) on an effect this mint's
    /// allow-list admits. The recipient branch they ride is EFFECT-AGNOSTIC (see
    /// `a_may_slot_is_minted_only_for_the_seat_the_cr_603_5_gate_will_ask`), which is what
    /// makes the substitution sound rather than a convenience. Measured on `u2_board`:
    /// `build_target_slots` returns `Ok(0)` for every fixture below (each row asserts the
    /// consequence through its own matched positive), so each really reaches the shape-(B)
    /// arm rather than falling out at an upstream conjunct.
    fn shape_b(src: ObjectId, effect: crate::types::ability::Effect) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(effect, vec![], src, P0);
        ability.optional = true;
        ability.target_choice_timing = crate::types::ability::TargetChoiceTiming::Resolution;
        ability
    }

    fn shape_b_entry(id: u64, src: ObjectId, ability: ResolvedAbility) -> StackEntry {
        StackEntry {
            id: ObjectId(id),
            source_id: src,
            controller: P0,
            kind: StackEntryKind::TriggeredAbility {
                source_id: src,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
            },
        }
    }

    /// `Effect::PutCounter` on a `ScopedPlayer`-scoped creature filter — inside D4.3's
    /// six-arm scope filter, and the exact filter shape
    /// `filter_uses_relative_controller_scoped` keys on.
    fn scoped_put_counter() -> crate::types::ability::Effect {
        use crate::types::ability::{
            ControllerRef, Effect, QuantityExpr, TargetFilter, TypedFilter,
        };
        Effect::PutCounter {
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![crate::types::ability::TypeFilter::Creature],
                controller: Some(ControllerRef::ScopedPlayer),
                properties: vec![],
            }),
            counter_type: crate::types::counter::CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
        }
    }

    /// R23, conjuncts 1–2 — **CR 603.5: the `may` pin binds the prompt's RECIPIENT, not only
    /// the entry's OWNER.**
    ///
    /// Asserted AT THE MINT SEAM, deliberately: an offer-level negative would be satisfied by
    /// D4.3's scope filter on many boards regardless of this guard (the
    /// upstream-conjunct-dominates trap). The mint has no such upstream.
    ///
    /// `entry.controller == proposer` bounds who OWNS the entry; it does not bound who the
    /// resolver ASKS. `optional_prompt_player`'s last branch is EFFECT-AGNOSTIC — it fires on
    /// `ability.scoped_player` plus a `ScopedPlayer`-scoped `target_filter()` (CR 503.1a +
    /// CR 608.2d, the Braids, Conjurer Adept class — whose printed effect puts an artifact,
    /// creature, or land card from hand onto the battlefield, NOT a counter; `PutCounter`
    /// here is a synthetic stand-in for the class, which that branch admits precisely
    /// BECAUSE it is effect-agnostic) — so an allow-listed `PutCounter` reaches
    /// it. Without the conjunct, P0's pin would be spendable as P1's CR 603.5 choice.
    ///
    /// MATCHED POSITIVE, on the same instrument and differing in exactly `scoped_player`: it
    /// proves the fixture reaches the mint at all, so the negative is keyed to the recipient
    /// axis and not to one of the four upstream conjuncts.
    ///
    /// REVERT-PROBE: delete `&& effects::optional_prompt_player(state, ability) == proposer`
    /// from the `may` mint ⇒ the scoped-player entry publishes a `MayChoice` slot ⇒ the
    /// negative arm FLIPS TO FAIL.
    #[test]
    fn a_may_slot_is_minted_only_for_the_seat_the_cr_603_5_gate_will_ask() {
        let (mut state, src) = u2_board();

        // ── negative: the gate will ask P1, not the proposer ──
        let mut scoped = shape_b(src, scoped_put_counter());
        scoped.scoped_player = Some(P1);
        assert_eq!(
            crate::game::effects::optional_prompt_player(&state, &scoped),
            P1,
            "reach-guard: the recipient authority must really route this entry to the OTHER \
             seat, or the negative below is about nothing"
        );
        let negative = shape_b_entry(940, src, scoped);
        assert!(
            entry_publishes_pin_slots(&state, &negative, P0).is_none(),
            "CR 603.5: a `may` the resolver will ask ANOTHER seat publishes no pin slot"
        );

        // ── matched positive: byte-identical except `scoped_player` ──
        let unscoped = shape_b(src, scoped_put_counter());
        assert_eq!(
            crate::game::effects::optional_prompt_player(&state, &unscoped),
            P0,
            "reach-guard: with no scoped player the gate asks the controller = proposer"
        );
        let positive = shape_b_entry(941, src, unscoped);
        let published = entry_publishes_pin_slots(&state, &positive, P0)
            .expect("the matched positive must reach the mint and publish");
        assert!(
            published.may.is_some(),
            "the matched positive publishes the CR 603.5 gate"
        );
        assert!(
            published.target.is_none(),
            "shape (B): announcing it surfaces no CR 601.2c choice, so no target slot"
        );
        assert!(
            published.legal_targets.is_empty(),
            "no target slot carries no legal set"
        );

        // The published pair also reaches the point mint as ONE `MayChoice` point.
        state.stack.push_back(positive);
        let points = bounded_cycle_pin_slots(&state, P0);
        assert_eq!(
            points.len(),
            1,
            "shape (B) publishes exactly the may point: {points:?}"
        );
        assert_eq!(
            points[0].kind,
            crate::analysis::decision_template::DecisionPointKind::MayChoice
        );
        assert_eq!(
            points[0].slot.index, 1,
            "index 1 is the may slot in BOTH shapes"
        );
    }

    /// R23, conjunct 3 — **the PRODUCER census, so a new producer is a COUNTED event.**
    ///
    /// The struck form of this conjunct pinned `optional_prompt_player`'s own call-site count,
    /// which is trivially stable at 2 and moves neither when the guard is deleted nor when an
    /// unguarded producer is added — this plan's own "verify the seam, not the line" defect,
    /// committed. What actually bounds the mint conjunct's reach is how many things PRODUCE
    /// `WaitingFor::OptionalEffectChoice`: the conjunct is a fail-closed pre-filter on ONE of
    /// them, and soundness over the others is discharged at the consumption point.
    ///
    /// The five production producers are named individually, and exactly one of them is inside
    /// the CR 603.5 gate that consults the recipient authority. If a sixth appears, this row
    /// fails and whoever added it must decide where its recipient is bound.
    ///
    /// ⚠ **ADJUDICATED IN U4, NOT RELAXED.** The census moved `34 ⇒ 37`. The PRODUCER half is
    /// unchanged at **5** and its per-file list is byte-identical (only one line NUMBER moved,
    /// `game/engine.rs:10433 ⇒ :10493`, because U4's arm sits above it) — that half is what this
    /// row's claim is about, and it did not move. The `+1` READER is `game/engine.rs`'s new
    /// `OptionalEffectChoice` arm in `inject_pinned_answer`, i.e. the CONSUMPTION point this
    /// doc already names as where soundness over the other four producers is discharged; the
    /// `+2` are U4's own `#[cfg(test)]` fixtures. A new READER is the benign case — adjudicate
    /// it, do not relax the assert.
    ///
    /// ⚠ **RE-ADJUDICATED IN THE 5d LOW-FIX, NOT RELAXED.** One line NUMBER moved again,
    /// `game/engine.rs:10493 ⇒ :10500`, on the same terms as U4's shift above. Cause: the
    /// LOW-fix added a net **+7 DOC lines** above that producer (the mint's corrected
    /// board-not-prompt contract, and the Braids, Conjurer Adept Oracle-text correction) —
    /// comments only, not one executable line. The producer itself is BYTE-IDENTICAL (the
    /// `return Ok(Some(WaitingFor::OptionalEffectChoice` head, diffed against `HEAD`), the
    /// total stays **37** and the partition stays **5/7/25**, and the other four entries are
    /// unchanged. The two companion asserts above run FIRST and both fired GREEN on the run
    /// that caught this — which is the evidence that the SET did not move and only this
    /// entry's coordinate did. A line-number-only shift is the benign case; a changed
    /// producer set is not, and stays a counted event.
    ///
    /// ⚠ **RE-ADJUDICATED ON THE REBASE ONTO UPSTREAM #6842 (`8121fd1c6`), NOT RELAXED.**
    /// The row fired again; the PRODUCER COUNT IS STILL **5** and no sixth producer exists.
    /// Four of five coordinates shifted and one did not:
    /// `game/effects/mod.rs:5896/5973/8927 ⇒ :5918/5995/8949` (uniform **+22**, lines that
    /// commit adds above them in that file), `game/engine.rs:10500 ⇒ :10589` (**+89**, same
    /// cause), and `game/effects/scoped_library_search.rs:452` **UNMOVED**.
    /// Evidence this is a coordinate shift and not a set change: each of the five was re-read
    /// at its new coordinate and diffed against the pre-rebase tree (`chain3-prefold-backup`)
    /// at its old one — all five are BYTE-IDENTICAL, same files, same order, and the one
    /// entry at an unchanged coordinate is byte-identical in place, which a gained-or-lost
    /// producer could not produce. Same set, new line numbers ⇒ benign, re-baselined here.
    /// NOTE for the record: this rebase did NOT add a CR 603.5 producer. An earlier report of
    /// mine said upstream had added one; that was wrong — the row fired on coordinates.
    ///
    /// ⚠ **RE-ADJUDICATED ON THE REBASE ONTO UPSTREAM #6851 (`96e41b3ab`), NOT RELAXED.**
    /// The row fired in CI but not locally, because CI builds the MERGE ref (branch + main)
    /// while the branch was still based on `e12447f4f`. The PRODUCER COUNT IS STILL **5**, no
    /// sixth producer exists, and this time only ONE coordinate moved:
    /// `game/engine.rs:10589 ⇒ :10640` (**+51**), with `game/effects/mod.rs:5918/5995/8949`
    /// and `game/effects/scoped_library_search.rs:452` all **UNMOVED**.
    /// Evidence this is a coordinate shift and not a set change, three independent ways:
    /// (1) all five producers were re-read at their new coordinates and diffed against the
    /// pre-rebase tree at their old ones — **byte-identical**, same files, same order, with a
    /// negative control confirming the diff instrument discriminates (the new tree at the OLD
    /// coordinate `:10589` is a bare `}`, not the producer); (2) the +51 is fully accounted
    /// for by #6851's own insertions ABOVE this producer in the same file — measured net
    /// `+51` from `git diff -U0 e12447f4f 96e41b3ab`, so predicted `10589+51 = 10640` equals
    /// the observed coordinate exactly, and #6851's whole-file delta is also `+51`, i.e. it
    /// adds nothing below; (3) the total stays **37** and the partition stays **5/7/25**, so
    /// neither a producer nor a reader was gained or lost. Same set, one new line number ⇒
    /// benign, re-baselined here.
    #[test]
    fn the_cr_603_5_prompt_census_is_pinned_so_a_sixth_producer_is_a_counted_event() {
        /// Every `.rs` under the crate's `src`, and the `#[cfg(test)]`-attributed
        /// column-0 `mod … {` … column-0 `}` spans inside it. A whole file whose stem
        /// ends `_tests` is test-only (its parent declares it under `#[cfg(test)]`).
        fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("readable source dir") {
                let path = entry.expect("readable dir entry").path();
                if path.is_dir() {
                    rs_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        fn cfg_test_spans(lines: &[&str]) -> Vec<(usize, usize)> {
            let mut spans = Vec::new();
            let mut i = 0;
            while i < lines.len() {
                if lines[i].trim() == "#[cfg(test)]" {
                    let mut j = i + 1;
                    while j < lines.len()
                        && (lines[j].trim_start().starts_with("#[") || lines[j].trim().is_empty())
                    {
                        j += 1;
                    }
                    let is_mod = j < lines.len()
                        && lines[j].starts_with(['m', 'p'])
                        && lines[j].contains("mod ")
                        && lines[j].trim_end().ends_with('{');
                    if is_mod {
                        let mut k = j + 1;
                        while k < lines.len() && lines[k] != "}" {
                            k += 1;
                        }
                        spans.push((j, k));
                        i = k;
                    }
                }
                i += 1;
            }
            spans
        }

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rs_files(&root, &mut files);
        files.sort();
        assert!(files.len() > 100, "reach-guard: the walker found the crate");

        // The needle is ASSEMBLED so this row's own source cannot be counted by its own
        // instrument. `..` excludes multi-line READ destructures whose rest-pattern sits on
        // a later line — the inflation the raw grep suffers from.
        let needle = format!("WaitingFor::{}Choice {{", "OptionalEffect");
        let (mut producers, mut readers, mut in_test) = (Vec::new(), Vec::new(), 0usize);
        for path in &files {
            let text = std::fs::read_to_string(path).expect("readable source file");
            let lines: Vec<&str> = text.lines().collect();
            let spans = cfg_test_spans(&lines);
            let rel = path
                .strip_prefix(&root)
                .expect("under src")
                .display()
                .to_string();
            let test_file = rel.trim_end_matches(".rs").ends_with("_tests");
            for (n, line) in lines.iter().enumerate() {
                if !line.contains(&needle) || line.contains("..") {
                    continue;
                }
                if test_file || spans.iter().any(|(a, b)| (*a..=*b).contains(&n)) {
                    in_test += 1;
                } else if line.contains("waiting_for = ") || line.contains("Ok(Some(") {
                    producers.push(format!("{rel}:{}", n + 1));
                } else {
                    readers.push(format!("{rel}:{}", n + 1));
                }
            }
        }

        assert_eq!(
            producers.len() + readers.len() + in_test,
            37,
            "CR 603.5 prompt census drifted. A new PRODUCER must have its recipient bound \
             somewhere — the mint's conjunct (a) covers exactly ONE of them. A new READER is \
             the benign case (U4's own consumption arm was one): adjudicate it in this doc and \
             name the site, do not merely move the number.\n\
             producers={producers:#?}\nreaders={readers:#?}"
        );
        assert_eq!(
            (producers.len(), readers.len(), in_test),
            (5, 7, 25),
            "the partition, not just the total: five PRODUCTION producers, seven PRODUCTION \
             readers (they read `state.waiting_for` and never write it — the seventh is U4's \
             `inject_pinned_answer` arm), 25 `#[cfg(test)]` lines.\nproducers={producers:#?}\n\
             readers={readers:#?}"
        );
        assert_eq!(
            producers,
            vec![
                // DRIFT LOG for these three, newest last. Every entry is pure line movement
                // with the producer re-read and sha256-compared at its new coordinate; none has
                // ever been a real sixth producer.
                //   #6842 (8121fd1c6): `:5896/:5973/:8927 ⇒ :5918/:5995/:8949`, uniform +22.
                //   #6933: `engine.rs :10640 ⇒ :11427` (that entry, below).
                //   #6955 (c9daf66e3): `:8949 ⇒ :8970`, +21 == that commit's insertion count,
                //     and the other two did NOT move, which located the insertion below them.
                //   #6961 (2ead7aab1) + v0.44.0: `:5918/:5995/:8970 ⇒ :5996/:6073/:9048`,
                //     uniform +78 above all three (whole-file delta +153/-15).
                //
                // ⚠ THIS ROW FAILS IN CI BEFORE IT FAILS LOCALLY, and that is not a bug in the
                // row. CI checks out `refs/pull/<n>/merge` — this branch merged with CURRENT
                // `main` — so an upstream insertion above a producer reds it in CI while the
                // branch tree stays green, until the branch merges that upstream. Diagnose by
                // rebuilding the merge tree (`git merge-tree --write-tree HEAD upstream/main`)
                // and comparing coordinates there, NOT by editing pins to match a local tree.
                //
                // Five drifts, all upstream, zero true positives. The pin stays line-exact
                // because that is what makes a NEW mint a counted event; a function +
                // content-hash anchor would end the drift class while keeping that property,
                // and is offered as a follow-up rather than taken unannounced mid-review.
                "game/effects/mod.rs:5996".to_string(),
                "game/effects/mod.rs:6073".to_string(),
                "game/effects/mod.rs:9048".to_string(),
                // UNMOVED across the rebase, and that is itself evidence the SET did not
                // move: a census that had gained or lost a producer would not leave this
                // entry both byte-identical AND at the same coordinate.
                "game/effects/scoped_library_search.rs:452".to_string(),
                // 5d LOW-fix: `:10493 ⇒ :10500`, a doc-only line shift (+7 comment lines
                // above); producer byte-identical, total 37 and partition 5/7/25 untouched.
                // Rebase onto #6842: `:10500 ⇒ :10589`, on the same terms — that commit adds
                // lines above this producer too. Producer byte-identical.
                // Rebase onto #6851 (96e41b3ab): `:10589 ⇒ :10640`, again on the same terms.
                // The +51 is exactly #6851's measured net insertion above this line (and its
                // whole-file delta is also +51, so it adds nothing below). The OTHER FOUR
                // entries did not move at all this time — a census that had gained or lost a
                // producer could not leave four entries byte-identical AND in place.
                //
                // Fold of upstream #6933 (409956671, merged by the maintainer as d1a5270a4):
                // `:10640 ⇒ :11427`, +787. engine.rs's whole-file delta over the same range is
                // +1134, so 787 lands above this producer and 347 below — consistent with a
                // file that grew around it rather than one that gained a mint. Identity
                // re-established at the new coordinate rather than assumed: the line is
                // byte-identical by sha256 to `ea1b0ac19:engine.rs:10640`, and it is still
                // inside `begin_pending_trigger_target_selection` (fn opens at :11278), which
                // is the producer this row NAMES below. The old coordinate now holds
                // copy-target-slot code that mints nothing. The OTHER FOUR entries did not
                // move, which is the same set-preservation evidence as the previous rebases.
                "game/engine.rs:11427".to_string(),
            ],
            "the five production producers, NAMED: the CR 603.5 gate in `resolve_chain_body` \
             plus the two repeated-optional-payment drivers, the per-player acceptance cursor \
             in `scoped_library_search`, and `begin_pending_trigger_target_selection`'s \
             ANNOUNCEMENT-time modal prompt. Four of the five choose `player` WITHOUT \
             consulting the recipient authority, which is exactly why the mint conjunct is a \
             fail-closed pre-filter and not a soundness proof"
        );

        // Exactly ONE of them routes through the recipient authority: the CR 603.5 gate.
        let effects_src = std::fs::read_to_string(root.join("game/effects/mod.rs"))
            .expect("readable effects module");
        let authority = format!("{}_prompt_player", "optional");
        assert_eq!(
            effects_src.matches(&authority).count(),
            2,
            "one definition + exactly one call — the CR 603.5 gate's `let prompt_player = ..`. \
             A second call inside `effects/mod.rs` means a second producer started consulting \
             the authority and this row's partition needs re-deriving"
        );
    }

    /// R25 — **a stored `may` auto-choice is a SECOND authority on the same CR 603.5
    /// question, and the mint must refuse to it.**
    ///
    /// Without the conjunct the pin is minted, then the gate consumes the stored choice and
    /// **returns before setting any prompt** — so `inject_pinned_answer` is never entered and
    /// the fail-closed `_ => Err(RecastAbort)` arm the design leans on cannot fire on a prompt
    /// that is never raised. The declared `Take` would be silently replaced by the stored
    /// `Decline`. `MayTriggerAutoChoiceKey`/`Record` are `Serialize + Deserialize`, so a real
    /// dump can carry one.
    ///
    /// MATCHED POSITIVE, differing ONLY in the seeded record, so no upstream conjunct can
    /// dominate.
    ///
    /// REVERT-PROBE: delete the `ability.may_trigger_origin.as_ref().is_none_or(…is_none())`
    /// conjunct from the `may` mint ⇒ the seeded entry publishes a `MayChoice` slot ⇒ the
    /// negative arm FLIPS TO FAIL.
    #[test]
    fn a_stored_may_auto_choice_is_a_second_authority_the_mint_refuses_to() {
        use crate::types::ability::{
            Effect, QuantityExpr, TargetFilter, TriggerBaseSetInstanceRef,
            TriggerDefinitionOccurrenceRef, TriggerDefinitionRef,
        };
        use crate::types::game_state::{AutoMayChoice, MayTriggerAutoChoiceKey, MayTriggerOrigin};
        use crate::types::identifiers::ObjectIncarnationRef;

        let (mut state, src) = u2_board();
        // The production shape: `triggers.rs` mints `Definition { definition_ref }` from the
        // source's own incarnation plus the printed occurrence — built here identically.
        let origin = MayTriggerOrigin::Definition {
            definition_ref: TriggerDefinitionRef {
                source: ObjectIncarnationRef::of(src, 3),
                occurrence: TriggerDefinitionOccurrenceRef::Printed {
                    base_set: TriggerBaseSetInstanceRef::INITIAL,
                    printed_index: 0,
                },
            },
        };
        let with_origin = |src: ObjectId| {
            let mut ability = shape_b(
                src,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            );
            ability.may_trigger_origin = Some(origin.clone());
            ability
        };

        // ── matched positive: no stored record ⇒ the gate WILL prompt ⇒ mint publishes ──
        let entry = shape_b_entry(950, src, with_origin(src));
        assert!(
            state.may_trigger_auto_choices.is_empty(),
            "reach-guard: the positive arm runs with NO stored record"
        );
        assert!(
            entry_publishes_pin_slots(&state, &entry, P0)
                .expect("the positive arm publishes")
                .may
                .is_some(),
            "with no stored answer the CR 603.5 gate really asks, so the pin is spendable"
        );

        // ── negative: the SAME board with one record seeded ──
        state.set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: P0,
                source_id: src,
                origin: origin.clone(),
            },
            AutoMayChoice::Decline,
        );
        assert_eq!(
            state.may_trigger_auto_choice(&MayTriggerAutoChoiceKey {
                player: P0,
                source_id: src,
                origin,
            }),
            Some(AutoMayChoice::Decline),
            "reach-guard: the mint's key must be the key the seed stored, or the negative \
             passes for the wrong reason"
        );
        assert!(
            entry_publishes_pin_slots(&state, &entry, P0).is_none(),
            "CR 603.5: a stored auto-choice already answers this may, so a minted pin would \
             be silently unused — refuse it at the mint. Shape (B) has no other slot, so the \
             whole entry publishes nothing"
        );
    }

    /// R30 — **one published `MayChoice` slot stands for exactly ONE CR 603.5 prompt.**
    ///
    /// CR 732.2a requires the shortcut to describe *the* sequence of choices; a schema point
    /// is a choice SURFACE, so a slot that answers one prompt while the resolution opens N is
    /// a schema that under-describes its own sequence. Production suppresses the single
    /// up-front CR 603.5 gate for three `repeat_for` shapes and re-fires optionality PER
    /// ITERATION (CR 608.2c + CR 608.2d) instead. The mint asks production's own three
    /// predicates rather than re-deriving them — re-deriving is the drift defect one symbol
    /// over.
    ///
    /// THREE ARMS, one per predicate, each with its matched positive on the same instrument:
    /// * **(a) kind-driven — the reachable one.** `has_kind_driven_repeat` matches on
    ///   `repeat_for` and on NOTHING else (no `Effect` restriction), so an allow-listed
    ///   optional `PutCounter` of that shape reaches it. **(a′)** differs only in
    ///   `repeat_for: None`.
    /// * **(b) member-driven — live for an allow-listed effect.** `Effect::Token` with
    ///   `attach_to: Some(ParentTarget)` (Asinine Antics' shape, named in
    ///   `effect_parent_ref_slots`' own doc) is inside the allow list and reaches
    ///   `effect_iterates_over_parent_target`. **(b′)** differs in exactly the predicate's
    ///   own deciding leaf: `attach_to: Some(LastCreated)` is ALSO a context ref, so the head
    ///   filter is still `owner`, the shape is still (B) and the `repeat_for` is still
    ///   `ObjectCount` — only `filter_refs_parent_target` flips. Without (b′) a blanket
    ///   "refuse every `ObjectCount`" would pass, which is coarser than production and a mint
    ///   cost.
    /// * **(c) repeated optional payment — DISCLOSED as not independently reachable.** It
    ///   requires `Effect::PayCost`, which D4.3's scope filter refuses at conjunct (6), so no
    ///   certifiable offer carries such an entry. The arm asserts the mint's refusal only and
    ///   claims NO closed hole; it ships so the mint asks the same three questions production
    ///   asks.
    ///
    /// REVERT-PROBE: delete the three sub-conjuncts from the `may` mint ⇒ (a) and (b) FLIP TO
    /// FAIL while (a′)/(b′) stay green — the pairs discriminate the conjunct, not the fixture.
    #[test]
    fn one_published_may_slot_stands_for_exactly_one_cr_603_5_prompt() {
        use crate::types::ability::{
            AbilityCondition, AbilityCost, Effect, PtValue, QuantityExpr, QuantityRef, TargetFilter,
        };

        let (state, src) = u2_board();
        let publishes_may = |ability: ResolvedAbility, id: u64| -> bool {
            let entry = shape_b_entry(id, src, ability);
            entry_publishes_pin_slots(&state, &entry, P0)
                .and_then(|p| p.may)
                .is_some()
        };

        // ── (a) kind-driven, and (a′) its matched positive ──
        let kind_driven = || {
            let mut a = shape_b(src, scoped_put_counter());
            a.repeat_for = Some(QuantityExpr::Ref {
                qty: QuantityRef::DistinctCounterKindsAmong {
                    filter: TargetFilter::Controller,
                },
            });
            a
        };
        assert!(
            crate::game::effects::has_kind_driven_repeat(&kind_driven()),
            "reach-guard: production's own predicate must say TRUE for arm (a)'s fixture"
        );
        assert!(
            !publishes_may(kind_driven(), 960),
            "(a) CR 608.2c/608.2d: a `DistinctCounterKindsAmong` repeat fires ONE prompt PER \
             ITERATION, so a single slot would under-describe the CR 732.2a sequence"
        );
        let mut kind_positive = kind_driven();
        kind_positive.repeat_for = None;
        assert!(
            publishes_may(kind_positive, 961),
            "(a′) byte-identical except `repeat_for: None` ⇒ published, so (a) keys on the \
             repeat axis and not on `optional`, the recipient or the auto-choice conjunct"
        );

        // ── (b) member-driven, and (b′) the one-leaf matched positive ──
        let token_attached = |attach: TargetFilter| {
            let mut a = shape_b(
                src,
                Effect::Token {
                    name: "Cursed Role".to_string(),
                    power: PtValue::Fixed(1),
                    toughness: PtValue::Fixed(1),
                    types: vec!["Enchantment".to_string()],
                    colors: vec![],
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: TargetFilter::Controller,
                    attach_to: Some(attach),
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
            );
            a.repeat_for = Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Controller,
                },
            });
            a
        };
        assert!(
            crate::game::effects::has_member_driven_repeat_after_hydration(
                &state,
                &token_attached(TargetFilter::ParentTarget)
            ),
            "reach-guard: the `ParentTarget` fixture must really reach \
             `effect_iterates_over_parent_target`"
        );
        assert!(
            !publishes_may(token_attached(TargetFilter::ParentTarget), 962),
            "(b) CR 608.2c/608.2d: an `ObjectCount` repeat over a parent-target ref fires one \
             prompt per iterated member"
        );
        assert!(
            !crate::game::effects::has_member_driven_repeat_after_hydration(
                &state,
                &token_attached(TargetFilter::LastCreated)
            ),
            "reach-guard: `LastCreated` is also a context ref, so (b′) differs from (b) in \
             the predicate's own deciding leaf and in nothing else"
        );
        assert!(
            publishes_may(token_attached(TargetFilter::LastCreated), 963),
            "(b′) a blanket `refuse every ObjectCount` would be coarser than production and \
             would fail here"
        );

        // ── (c) repeated optional payment — refusal asserted, reach DISCLOSED as closed ──
        let repeated_payment = || {
            let mut a = shape_b(
                src,
                Effect::PayCost {
                    cost: AbilityCost::Mana {
                        cost: crate::types::mana::ManaCost::Cost {
                            shards: vec![],
                            generic: 1,
                        },
                    },
                    scale: None,
                    payer: TargetFilter::Controller,
                },
            );
            a.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
            let mut sub = ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                src,
                P0,
            );
            sub.condition = Some(AbilityCondition::WhenYouDo);
            a.sub_ability = Some(Box::new(sub));
            a
        };
        assert!(
            crate::game::effects::is_repeated_optional_payment(&repeated_payment()),
            "reach-guard: production's own predicate must say TRUE for arm (c)'s fixture"
        );
        assert!(
            !publishes_may(repeated_payment(), 964),
            "(c) CR 603.12a: the payment process offers its `may` PER iteration. This arm \
             asserts the mint's refusal only — `Effect::PayCost` is outside D4.3's six-arm \
             allow list, so no certifiable offer carries such an entry and NO closed hole is \
             claimed here"
        );
    }

    // ────────── 5d U4 — the `OptionalEffectChoice` arm and its two TOTAL head guards ──────────

    /// Life the shared U4 fixture's suspended optional ability gains when its CR 603.5 choice
    /// is TAKEN. Named rather than inlined so every "the pin was APPLIED" assertion below is
    /// keyed to the fixture instead of to a literal.
    const U4_MAY_LIFE: i32 = 5;

    /// A board parked on a REAL CR 603.5 resolution-time prompt. `asked` is the seat the
    /// resolver is asking, and the suspended optional ability is THAT seat's, so taking the
    /// choice gains `asked` exactly `U4_MAY_LIFE` life.
    ///
    /// The life delta is the observable that separates "the pinned `DecideOptionalEffect` was
    /// dispatched" from "the injector returned `Ok(())` having done nothing": an empty board
    /// would answer `Ok(())` just as happily.
    ///
    /// The pinned source is a BATTLEFIELD object because `resolve_source` is battlefield-only
    /// (CR 400.7 incarnation binding) — on any other zone `slot_source_prompted` would refuse
    /// every arm below for a reason none of them is about.
    fn u4_may_board(asked: PlayerId) -> (GameState, ObjectId) {
        use crate::types::ability::{Effect, QuantityExpr, TargetFilter};
        let mut state = GameScenario::new_n_player(3, 7).build().state().clone();
        let src = place(&mut state, 970, Zone::Battlefield);
        let mut optional = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: U4_MAY_LIFE },
                player: TargetFilter::Controller,
            },
            vec![],
            src,
            asked,
        );
        optional.optional = true;
        state.push_optional_effect_frame(crate::types::resolution::OptionalEffectFrame {
            ability: Box::new(optional),
            trigger_event: None,
            trigger_match_count: None,
        });
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: asked,
            source_id: src,
            description: None,
            may_trigger_key: None,
        };
        (state, src)
    }

    /// A template carrying exactly one CR 603.5 `MayChoice` pin for `src` (slot index 1 — the
    /// may slot in both mint shapes), declared by `owner`.
    fn u4_may_template(
        src: ObjectId,
        owner: PlayerId,
        take: crate::analysis::decision_template::MayChoiceOption,
    ) -> DecisionTemplate {
        let source = this_object(src);
        DecisionTemplate {
            owner,
            decisions: vec![PinnedDecision::MayChoice {
                slot: DecisionSlot {
                    source: source.clone(),
                    index: 1,
                },
                take,
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(1),
            },
            key: DecisionGroupKey::from_sources(&[source], DecisionKind::LoopChoice),
        }
    }

    /// R23, conjunct 4 — **CR 603.5 + CR 732.2a: a `may` pin answers only the seat the PROMPT
    /// names.**
    ///
    /// The mint's recipient conjunct (U2) is a PREDICTION over one of five
    /// `WaitingFor::OptionalEffectChoice` producers — only one of them consults
    /// `optional_prompt_player` — so it is partial by construction. This guard reads the
    /// recipient OFF THE PROMPT, which is total over all five and over any sixth.
    ///
    /// MATCHED POSITIVE, same instrument, same template, differing in exactly one fixture
    /// parameter (the seat the prompt names — and, coherently, the controller of the suspended
    /// optional ability, since the resolver asks that ability's controller): the pinned
    /// `DecideOptionalEffect` is dispatched and `U4_MAY_LIFE` life is gained.
    ///
    /// Two reach-guards keep the negative off the arm's other refusals: the pin's slot really
    /// does match the prompted source on the NEGATIVE's own board (so the `find_map` is not the
    /// refuser), and the beat guard's cursor is absent on both boards.
    ///
    /// The POSITIVE is asserted FIRST, deliberately: under the revert-probe below one run then
    /// shows the positive PASSING and the negative FAILING, which is what proves the pair
    /// discriminates the SEAT rather than the fixture.
    ///
    /// REVERT-PROBE: delete `if *player != template.owner { return Err(RecastAbort); }` ⇒ the
    /// negative arm FLIPS TO FAIL (it returns `Ok(())` and P1 gains the life P0's pin bought).
    #[test]
    fn a_may_pin_answers_only_the_seat_the_prompt_names() {
        use crate::analysis::decision_template::MayChoiceOption;

        // ── MATCHED POSITIVE: the template against ITS OWNER's own prompt ──
        let (mut asks_owner, src) = u4_may_board(P0);
        let template = u4_may_template(src, P0, MayChoiceOption::Take);
        let own_prompt = asks_owner.waiting_for.clone();
        let p0_before = life(&asks_owner, P0);
        inject_pinned_answer(&mut asks_owner, Some(&template), 0, &own_prompt)
            .expect("CR 603.5: the owner's own pin answers the owner's own choice");
        assert_eq!(
            life(&asks_owner, P0),
            p0_before + U4_MAY_LIFE,
            "the pinned `DecideOptionalEffect {{ accept: true }}` was really APPLIED — an \
             `Ok(())` that dispatched nothing would leave this unchanged"
        );
        assert_ne!(
            asks_owner.waiting_for, own_prompt,
            "the prompt was consumed, not silently skipped"
        );

        // ── NEGATIVE: same template (owner P0), but the prompt asks P1 ──
        let (mut asks_other, other_src) = u4_may_board(P1);
        assert_eq!(
            other_src, src,
            "both boards mint the same pinned source object"
        );
        let prompt = asks_other.waiting_for.clone();
        assert!(
            slot_source_prompted(&asks_other, &this_object(src), src),
            "reach-guard: the pin's slot MATCHES the prompted source on this very board, so a \
             refusal below cannot be the `find_map`'s"
        );
        assert!(
            asks_other.pending_trigger.is_none(),
            "reach-guard: no construction cursor, so the BEAT guard cannot be the refuser"
        );
        let p1_before = life(&asks_other, P1);
        assert!(
            inject_pinned_answer(&mut asks_other, Some(&template), 0, &prompt).is_err(),
            "CR 603.5: a pin owned by the proposer must not answer ANOTHER seat's choice"
        );
        assert_eq!(
            life(&asks_other, P1),
            p1_before,
            "the refusal is fail-closed: nothing was dispatched as P1"
        );
        assert_eq!(
            asks_other.waiting_for, prompt,
            "the other seat's prompt is still standing, for a human to answer"
        );
    }

    /// R23, conjunct 5 — **CR 603.5 vs CR 603.3c + CR 700.2b: the pin binds the BEAT as well as
    /// the seat.**
    ///
    /// A `MayChoice` pin answers the RESOLUTION-time question (CR 603.5). The engine also asks a
    /// same-`source_id` ANNOUNCEMENT-time question while a trigger is still mid-construction
    /// (the optional-modal gate, CR 603.3c / CR 700.2b), and `slot_source_prompted` cannot
    /// separate the two: it matches the SOURCE OBJECT and both prompts carry it. That is why the
    /// pair below uses the SAME `source_id` in the cursor as in the prompt — a differing-source
    /// fixture would be refused by the slot lookup instead and the row would report the wrong
    /// guard.
    ///
    /// Both arms hold the seat guard SATISFIED (`player == template.owner == P0`), asserted
    /// below, so conjunct 4's guard cannot be what decides either arm.
    ///
    /// (5-pos) is asserted FIRST so that under the revert-probe ONE run shows the positive
    /// passing and the negative failing — the evidence that the pair discriminates the CURSOR
    /// and not the fixture.
    ///
    /// REVERT-PROBE: delete `if work.pending_trigger.is_some() { return Err(RecastAbort); }` ⇒
    /// (5-neg) FLIPS TO FAIL while (5-pos) and conjuncts 1–4 stay green.
    #[test]
    fn a_may_pin_never_answers_the_announcement_time_question() {
        use crate::analysis::decision_template::MayChoiceOption;

        let template = |src: ObjectId| u4_may_template(src, P0, MayChoiceOption::Take);

        // ── (5-pos): the same board with NO construction cursor ──
        let (mut cursor_clear, src) = u4_may_board(P0);
        assert!(cursor_clear.pending_trigger.is_none());
        let pos_prompt = cursor_clear.waiting_for.clone();
        let pos_before = life(&cursor_clear, P0);
        inject_pinned_answer(&mut cursor_clear, Some(&template(src)), 0, &pos_prompt).expect(
            "(5-pos) CR 603.5: with no cursor the pin answers its own resolution-time question",
        );
        assert_eq!(
            life(&cursor_clear, P0),
            pos_before + U4_MAY_LIFE,
            "(5-pos) the pinned choice was applied"
        );

        // ── (5-neg): a LIVE construction cursor on the SAME source ──
        let (mut cursor_live, neg_src) = u4_may_board(P0);
        assert_eq!(neg_src, src, "the pair is built from one fixture");
        let mut mid_construction = ResolvedAbility::new(
            crate::types::ability::Effect::GainLife {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                player: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            src,
            P0,
        );
        // The production shape that raises a same-source ANNOUNCEMENT-time prompt: an optional
        // modal trigger, asked before its modes are chosen (CR 603.3c / CR 700.2b).
        mid_construction.optional = true;
        cursor_live.pending_trigger = Some(Box::new(super::triggers::PendingTrigger {
            source_id: src,
            controller: P0,
            condition: None,
            ability: Box::new(mid_construction),
            timestamp: 0,
            target_constraints: vec![],
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        }));
        let prompt = cursor_live.waiting_for.clone();
        assert_eq!(
            prompt, pos_prompt,
            "(5-neg) differs from (5-pos) in `work.pending_trigger` and in NOTHING else the \
             injector reads — the prompts are equal"
        );
        let WaitingFor::OptionalEffectChoice {
            player: asked,
            source_id: prompt_source,
            ..
        } = &prompt
        else {
            panic!("the fixture parks on a CR 603.5 prompt; got {prompt:?}");
        };
        assert_eq!(
            *asked,
            template(src).owner,
            "reach-guard: the SEAT guard is satisfied on both arms, so conjunct 4 cannot be \
             what decides this pair"
        );
        assert_eq!(
            cursor_live
                .pending_trigger
                .as_ref()
                .map(|t| t.source_id)
                .expect("(5-neg) carries a cursor"),
            *prompt_source,
            "the cursor names the SAME source as the prompt — that identity is what makes this \
             arm non-vacuous, because `slot_source_prompted` matches on exactly that object"
        );
        let before = life(&cursor_live, P0);
        assert!(
            inject_pinned_answer(&mut cursor_live, Some(&template(src)), 0, &prompt).is_err(),
            "CR 603.5 vs CR 603.3c: with a live construction cursor the prompt in hand may be \
             the ANNOUNCEMENT-time question the pin does not answer ⇒ fail-closed"
        );
        assert_eq!(
            life(&cursor_live, P0),
            before,
            "(5-neg) fail-closed: no `DecideOptionalEffect` was dispatched"
        );
        assert_eq!(
            cursor_live.waiting_for, prompt,
            "(5-neg) the prompt still stands"
        );
    }

    /// The arm's own leaf, covered in BOTH directions — **CR 603.5: a "may" is binary, and the
    /// pin says WHICH.**
    ///
    /// The injector's `accept` flag is one equality test against `MayChoiceOption`'s take
    /// variant — the single place the typed pin becomes the engine's boolean. A
    /// single-direction row would pass just as happily against the INVERTED mapping, which is
    /// why both options are driven on one fixture. (That comparison is deliberately NOT quoted
    /// verbatim here: a textual revert-probe whose needle also matched this doc line would
    /// silently no-op — the tripwire this row's own probe hit on its first run.)
    ///
    /// `Decline` is separated from "nothing was dispatched" by the prompt: a declined optional
    /// effect CONSUMES the prompt (the frame resolves its decline branch) while a refusal leaves
    /// it standing. Both facts are asserted, so neither arm can pass by inaction.
    #[test]
    fn a_may_pin_dispatches_the_option_it_names_in_both_directions() {
        use crate::analysis::decision_template::MayChoiceOption;

        for take in [MayChoiceOption::Take, MayChoiceOption::Decline] {
            let (mut board, src) = u4_may_board(P0);
            let prompt = board.waiting_for.clone();
            let before = life(&board, P0);
            inject_pinned_answer(
                &mut board,
                Some(&u4_may_template(src, P0, take)),
                0,
                &prompt,
            )
            .expect("both options are legal answers to a CR 603.5 prompt");
            assert_ne!(
                board.waiting_for, prompt,
                "{take:?}: the prompt is ANSWERED either way — that is what separates a \
                 `Decline` from a fail-closed refusal"
            );
            assert_eq!(
                life(&board, P0) - before,
                match take {
                    MayChoiceOption::Take => U4_MAY_LIFE,
                    MayChoiceOption::Decline => 0,
                },
                "{take:?}: the pin's own option decides `accept`, so an inverted mapping fails \
                 on one of these two arms"
            );
        }
    }

    /// A live `LoopShortcut` offer with an EMPTY schema, proposed by P0 on `state`.
    fn u4_park_on_offer(state: &mut GameState) {
        use crate::analysis::decision_template::ShortcutDecisionSchema;
        use crate::analysis::loop_check::{LoopCertificate, WinKind};
        use crate::analysis::resource::BoardDelta;
        state.waiting_for = WaitingFor::LoopShortcut {
            proposer: P0,
            predicted_winner: None,
            certificate: LoopCertificate {
                unbounded: vec![],
                win_kind: WinKind::LethalDamage,
                mandatory: false,
                residual_board_delta: BoardDelta::default(),
                per_cycle: None,
            },
            schema: ShortcutDecisionSchema::default(),
        };
    }

    /// R28 arm (b) — **the DRIVE seam cannot see a `template.owner` the engine never bound; the
    /// DECLARE firewall is what makes the drive's seat guard meaningful.**
    ///
    /// ⚠ **(b1) ASSERTS A MEASURED BREACH, NOT A DESIRED BEHAVIOUR.** `template.owner` arrives
    /// verbatim from the client, and `inject_pinned_answer` holds the template but not the
    /// offer — so when an attacker sets `owner` to the seat the prompt names, the seat guard
    /// compares that value against itself and passes. Measured on this tree: the injector
    /// returns `Ok(())` and dispatches the PROPOSER's pinned choice as the OTHER seat's
    /// `GameAction::DecideOptionalEffect` (P1 gains `U4_MAY_LIFE`). That is the whole reason the
    /// binding lives at declare (`handle_declare_shortcut`) and at consumption
    /// (`apply_confirmed_shortcut`), one layer above this one.
    ///
    /// ⚠ **PLAN DEVIATION, DISCLOSED:** §6 R28(b) predicts the drive seam refuses this pair
    /// (*"must still `RecastAbort`"*). It does not, and cannot — the same cell's own analysis
    /// says so two sentences later (*"under the round-33 design alone it returns `Ok(())`"*).
    /// The arm ships keyed to the measurement, with (b2) supplying the refusal the row is
    /// really about. If a future change closes the drive seam (e.g. by threading the
    /// engine-issued proposer into the injector), (b1) FLIPS and must be re-keyed onto the new
    /// refusal rather than deleted.
    ///
    /// **(b2) THE REFUSAL, AT THE SEAM THAT HAS THE ENGINE-ISSUED COMPARAND.** The identical
    /// hostile template declared against a live `LoopShortcut { proposer: P0 }` is refused into
    /// the manual handback — no `ShortcutProposal` is built — so the (b1) configuration is
    /// unreachable in production. **MATCHED POSITIVE:** byte-identical except `owner`, which
    /// opens APNAP; without it, "refused" would be indistinguishable from "this constructed
    /// offer refuses everything".
    ///
    /// REVERT-PROBE: delete `if template.as_ref().is_some_and(|t| t.owner != offer.proposer)`
    /// from `handle_declare_shortcut` ⇒ **(b2) FLIPS TO FAIL** (the hostile declaration builds a
    /// proposal and APNAP opens), while **(b1) MUST NOT FLIP** — the injector reads no firewall.
    /// The pair therefore proves the two halves measure two different seams rather than one
    /// seam asserted twice. Arm (b) and arm (c) (`apply_confirmed_shortcut`, U2, integration)
    /// take OPPOSITE reverts by design: (b) is about the drive-side comparand, (c) about the
    /// restore ingress that never reaches declare.
    #[test]
    fn r28_b_the_drive_seat_guard_compares_a_client_supplied_owner_against_itself() {
        use crate::analysis::decision_template::{IterationCount, MayChoiceOption};

        // ── (b1) DRIVE seam: prompt player P1, template owner P1 (the attacker's choice) ──
        let (mut board, src) = u4_may_board(P1);
        let hostile = u4_may_template(src, P1, MayChoiceOption::Take);
        let prompt = board.waiting_for.clone();
        let p1_before = life(&board, P1);
        let outcome = inject_pinned_answer(&mut board, Some(&hostile), 0, &prompt);
        assert!(
            outcome.is_ok(),
            "(b1) MEASURED: with `owner` set to the prompt's own seat the drive guard has \
             nothing to compare — it passes. Got {outcome:?}"
        );
        assert_eq!(
            life(&board, P1),
            p1_before + U4_MAY_LIFE,
            "(b1) and the proposer's pinned value was really dispatched AS P1 — this is the \
             breach the declare-time binding closes, measured rather than argued"
        );

        // ── (b2) DECLARE seam: the same template, refused by the engine-issued comparand ──
        for owner in [P1, P0] {
            let (mut state, offer_src) = u4_may_board(P0);
            assert_eq!(offer_src, src, "one fixture feeds both halves");
            u4_park_on_offer(&mut state);
            apply_action(
                &mut state,
                P0,
                GameAction::DeclareShortcut {
                    count: IterationCount::Fixed(1),
                    template: Some(u4_may_template(src, owner, MayChoiceOption::Take)),
                },
                None,
            )
            .expect("the declaration is dispatched either way — refusal is a HANDBACK");

            if owner == P1 {
                assert!(
                    matches!(state.waiting_for, WaitingFor::Priority { .. }),
                    "(b2) CR 732.2a + CR 603.5 + CR 800.4a: a declaration whose `owner` is not \
                     the engine-issued proposer hands priority back; got {:?}",
                    state.waiting_for
                );
                assert!(
                    !matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
                    "(b2) no `ShortcutProposal` carrying the hostile owner may exist — that is \
                     what makes (b1)'s configuration production-unreachable"
                );
            } else {
                let WaitingFor::RespondToShortcut { proposal, .. } = &state.waiting_for else {
                    panic!(
                        "(b2) matched positive: the honest declaration must open APNAP, so the \
                         refusal above is keyed to `owner` and not to this constructed offer \
                         refusing everything; got {:?}",
                        state.waiting_for
                    );
                };
                assert_eq!(
                    proposal.template.as_ref().map(|t| t.owner),
                    Some(P0),
                    "(b2) the proposal that IS built carries the engine-bound owner"
                );
            }
        }
    }
}

/// FIX-1 interruptibility (memory: combo-interruptibility-acceptance-criterion) — the Kilo loop's
/// CR 732.2a offer must FLIP off when the loop is defused. Driven from the REAL 4p dump through the
/// public `apply()` boundary (recording live), then the offer is re-derived at the private
/// `try_offer_object_growth_shortcut` seam (the plan's sanctioned private-fn revert-probe form).
#[cfg(test)]
mod kilo_interruptibility_tests {
    use super::*;
    use crate::analysis::decision_template::{PinnedDecision, TargetPin};
    use crate::types::ability::TargetRef;
    use crate::types::game_state::{ManaChoice, PayCostKind, YieldTarget};
    use crate::types::mana::{ManaColor, ManaType};

    const P0: PlayerId = PlayerId(0);
    const KILO: ObjectId = ObjectId(402);
    const FREED: ObjectId = ObjectId(403);
    const RELIC: ObjectId = ObjectId(404);
    const PENTAD: ObjectId = ObjectId(405);
    const RELIC_TAP_MANA: usize = 1;
    const FREED_UNTAP: usize = 1;

    fn load_migrated_dump() -> GameState {
        use crate::types::game_state::PersistedGameState;
        use std::io::Read;
        let gz: &[u8] = include_bytes!("../../tests/fixtures/kilo_freed_relic_pentad_4p.json.gz");
        let mut json = String::new();
        flate2::read::GzDecoder::new(gz)
            .read_to_string(&mut json)
            .expect("fixture inflates");
        let envelope: serde_json::Value = serde_json::from_str(&json).expect("envelope parses");
        // Route through the REAL production restore chokepoint so the FIX-3 migration hook
        // (`migrate_transient_loop_sequence`) drops the dump's 6 stale pinless steps on load —
        // exactly as the integration helper does. Deserializing directly would bypass the hook,
        // leaving the stale prefix so the live drive yields an 8-step (not 2-step) sequence.
        // Decoding AS `PersistedGameState` (rather than decoding a bare `GameState` and
        // wrapping it) additionally routes the dump through
        // `reject_legacy_raw_prompt_authority` + `decode_persisted_resolution_state`.
        // `.expect(..)`, not `?`: `into_game_state` returns `GameState`, not `Result`.
        serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
            .expect("gameState deserializes through the production decoder")
            .into_game_state()
    }

    fn beat_actor(state: &GameState) -> PlayerId {
        match &state.waiting_for {
            WaitingFor::Priority { player }
            | WaitingFor::PayCost { player, .. }
            | WaitingFor::ChooseManaColor { player, .. }
            | WaitingFor::ProliferateChoice { player, .. } => *player,
            WaitingFor::LoopShortcut { proposer, .. } => *proposer,
            other => panic!("unexpected beat: {other:?}"),
        }
    }

    /// Drive ONE full live cycle via the public boundary, recording the pinned period.
    fn drive_one_live_cycle(state: &mut GameState) {
        apply(
            state,
            P0,
            GameAction::ActivateAbility {
                source_id: RELIC,
                ability_index: RELIC_TAP_MANA,
            },
        )
        .expect("activate Relic mana ability");
        let mut freed_activated = false;
        for _ in 0..200 {
            let actor = beat_actor(state);
            match state.waiting_for.clone() {
                WaitingFor::LoopShortcut { .. } => return,
                WaitingFor::PayCost {
                    kind: PayCostKind::TapCreatures { .. },
                    ..
                } => {
                    apply(state, actor, GameAction::SelectCards { cards: vec![KILO] })
                        .expect("tap Kilo");
                }
                WaitingFor::ChooseManaColor { .. } => {
                    apply(
                        state,
                        actor,
                        GameAction::ChooseManaColor {
                            choice: ManaChoice::SingleColor(ManaType::Blue),
                            count: 1,
                        },
                    )
                    .expect("choose Blue");
                }
                WaitingFor::ProliferateChoice { .. } => {
                    apply(
                        state,
                        actor,
                        GameAction::SelectTargets {
                            targets: vec![TargetRef::Object(PENTAD)],
                        },
                    )
                    .expect("proliferate Pentad");
                }
                WaitingFor::Priority { .. } => {
                    if state.stack.is_empty() {
                        if freed_activated {
                            return;
                        }
                        freed_activated = true;
                        apply(
                            state,
                            P0,
                            GameAction::ActivateAbility {
                                source_id: FREED,
                                ability_index: FREED_UNTAP,
                            },
                        )
                        .expect("activate Freed untap");
                    } else {
                        apply(state, actor, GameAction::PassPriority).expect("pass priority");
                    }
                }
                other => panic!("unexpected beat: {other:?}"),
            }
        }
        panic!("drive did not settle");
    }

    /// Matched pair: with the loop intact the offer re-derives (`Some`); removing Freed (Kilo can
    /// no longer untap, the cycle is no longer mana-neutral) means the recorded `Activate 403#1`
    /// step's ability definition can no longer be resolved (its object is gone), so `try_offer`
    /// aborts at the pre-drive ability-def resolution ⇒ `None`. Pass-vs-defuse FLIPS the outcome.
    #[test]
    fn freed_removed_defuses_the_offer() {
        let mut driven = load_migrated_dump();
        drive_one_live_cycle(&mut driven);
        assert_eq!(
            driven.last_loop_action_sequence.len(),
            2,
            "the live cycle recorded the clean 2-step pinned period"
        );

        // Re-derive the empty-stack priority window the offer fires from (the recorded period is
        // intact; the board is a valid loop state — Kilo untapped, mana-neutral).
        let mut intact = driven.clone();
        intact.waiting_for = WaitingFor::Priority { player: P0 };
        assert!(intact.stack.is_empty(), "settled to an empty stack");
        assert!(
            try_offer_object_growth_shortcut(&intact).is_some(),
            "undefused: the intact loop re-derives the CR 732.2a offer"
        );

        // Defuse: remove Freed AFTER recording. The re-drive can no longer re-find/re-activate it.
        let mut defused = intact.clone();
        defused.objects.remove(&FREED);
        defused.battlefield.retain(|id| *id != FREED);
        assert!(
            try_offer_object_growth_shortcut(&defused).is_none(),
            "defused (Freed removed): the re-drive aborts ⇒ NO offer — the outcome flips"
        );
    }

    /// Reset a driven state (which settles at `LoopShortcut`) back to the empty-stack priority
    /// window the offer re-derives from, so `try_offer_object_growth_shortcut` can be probed
    /// directly (the plan's sanctioned private-fn revert-probe form). The board is the valid
    /// post-cycle loop state (Kilo untapped, mana-neutral).
    fn at_priority_window(mut state: GameState) -> GameState {
        state.waiting_for = WaitingFor::Priority { player: P0 };
        assert!(
            state.stack.is_empty(),
            "the driven cycle settled to an empty stack"
        );
        state
    }

    /// Hostile fixture — two-legendary identity binding (memory: verify-the-seam-not-the-line).
    /// The tap-cost pin stores the EXACT tapped `ObjectId` (`TargetPin::ByIdentity`), so with two
    /// legal untapped legendary creatures on the board the detection re-drive must re-bind to the
    /// RECORDED Kilo (402), NOT the decoy. Positive: record tapping Kilo ⇒ offer. Revert-probe
    /// (FLIP, run in-test): repoint ONLY the tap-cost pin's identity to the decoy (an equally-legal
    /// legendary) on the SAME board + recording ⇒ the re-drive taps the decoy, whose becomes-tapped
    /// proliferate trigger (source = decoy) has NO matching pin (the proliferate pin is keyed to
    /// Kilo 402) ⇒ `RecastAbort` ⇒ NO offer. If replay ignored the pin identity (re-bound to "any
    /// legal legendary" or always Kilo) this mutation would NOT change the outcome — so the flip
    /// proves the recorded identity is load-bearing.
    #[test]
    fn tap_pin_rebinds_to_recorded_legendary_not_a_decoy() {
        let mut state = load_migrated_dump();

        // Add a SECOND untapped legendary creature P0 controls (a Kilo clone with a fresh id) so
        // the Relic tap cost has two legal choices the identity binding must disambiguate.
        let decoy_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut decoy = state.objects[&KILO].clone();
        decoy.id = decoy_id;
        // Distinct name: CR 704.5j (the legend rule) would otherwise force a ChooseLegend SBA
        // between two same-named legends — we want two co-existing legal legendary tap targets.
        decoy.name = "Decoy Legend".to_string();
        decoy.base_name = "Decoy Legend".to_string();
        decoy.attachments = Vec::new(); // the clone is NOT the Freed-enchanted creature
        decoy.tapped = false;
        state.objects.insert(decoy_id, decoy);
        state.battlefield.push_back(decoy_id);

        drive_one_live_cycle(&mut state);
        assert_eq!(
            state.last_loop_action_sequence.len(),
            2,
            "reach-guard: the live cycle recorded the clean 2-step pinned period"
        );

        // Positive: the recorded ByIdentity(Kilo 402) tap pin re-binds to Kilo on replay ⇒ offer.
        let intact = at_priority_window(state.clone());
        assert!(
            try_offer_object_growth_shortcut(&intact).is_some(),
            "two legal legendaries present + recorded Kilo ⇒ the offer fires"
        );

        // Revert-probe (FLIP): repoint ONLY the tap-cost pin (its slot source resolves to Relic
        // 404) to the decoy. Board, recording, and the proliferate pin (keyed to Kilo 402) are all
        // unchanged.
        let mut repointed = intact.clone();
        let mut mutated = false;
        for step in repointed.last_loop_action_sequence.iter_mut() {
            for pin in step.pins.iter_mut() {
                if let PinnedDecision::Targets { slot, targets } = pin {
                    if matches!(&slot.source, YieldTarget::ThisObject { source_id, .. } if *source_id == RELIC)
                    {
                        *targets = vec![TargetPin::ByIdentity(YieldTarget::ThisObject {
                            source_id: decoy_id,
                            incarnation: None,
                            trigger_description: None,
                        })];
                        mutated = true;
                    }
                }
            }
        }
        assert!(
            mutated,
            "reach-guard: the tap-cost pin (slot source Relic) was found + repointed"
        );
        assert!(
            try_offer_object_growth_shortcut(&repointed).is_none(),
            "repointing the tap pin to the decoy FLIPS the offer OFF ⇒ recorded identity is load-bearing"
        );
    }

    /// Hostile fixture — wrong-color drive. The `ManaColor` pin latches the color the player
    /// produced (Blue, to pay Freed's `{U}`, CR 608.2d). Positive: Blue ⇒ mana-neutral cycle ⇒
    /// offer. Revert-probe (FLIP, run in-test): relatch the color to Red on the SAME recording ⇒
    /// the re-drive produces Red, Freed's `{U}` untap is unpayable ⇒ the second step aborts ⇒ NO
    /// offer. The latched color value is load-bearing.
    #[test]
    fn mana_color_pin_replays_recorded_color() {
        let mut state = load_migrated_dump();
        drive_one_live_cycle(&mut state);
        let state = at_priority_window(state);

        // Positive: the latched Blue color pays Freed's {U} ⇒ offer.
        assert!(
            try_offer_object_growth_shortcut(&state).is_some(),
            "the recorded Blue mana-color pin completes the mana-neutral cycle ⇒ offer"
        );

        // Revert-probe (FLIP): relatch the color to Red.
        let mut wrong = state.clone();
        let mut mutated = false;
        for step in wrong.last_loop_action_sequence.iter_mut() {
            for pin in step.pins.iter_mut() {
                if let PinnedDecision::ManaColor { color, .. } = pin {
                    *color = ManaColor::Red;
                    mutated = true;
                }
            }
        }
        assert!(
            mutated,
            "reach-guard: the ManaColor pin was found + relatched"
        );
        assert!(
            try_offer_object_growth_shortcut(&wrong).is_none(),
            "a Red mana-color pin cannot pay Freed's {{U}} ⇒ the drive aborts ⇒ NO offer"
        );
    }

    /// Synthetic positive/negative drive-replay reach-guard (plan §7 unit c). The SAME recorded
    /// 2-step period is driven WITH pins (offer) and WITHOUT (abort). The `len()==2` anchor holds
    /// in BOTH variants, so the negative's None is a drive-abort at the unpinned
    /// `PayCost{TapCreatures}`, NOT a vacuous "no sequence to drive" upstream short-circuit
    /// (memory: discriminator-vacuous-if-upstream-conjunct-dominates).
    #[test]
    fn drive_replay_requires_the_recorded_pins() {
        let mut state = load_migrated_dump();
        drive_one_live_cycle(&mut state);
        let state = at_priority_window(state);

        // Anchor (holds in BOTH variants): the recorded 2-step period is present.
        assert_eq!(
            state.last_loop_action_sequence.len(),
            2,
            "reach-guard anchor: the recorded period exists ⇒ any None is a drive-abort, not a missing seq"
        );

        // Positive: the recorded pins drive the replay to completion ⇒ offer.
        assert!(
            try_offer_object_growth_shortcut(&state).is_some(),
            "with the recorded pins the replay completes ⇒ offer"
        );

        // Negative: strip the pins from the SAME period ⇒ the replay hits the unpinned tap cost ⇒
        // abort ⇒ NO offer. The anchor proves the None is the drive-abort, not an empty sequence.
        let mut unpinned = state.clone();
        for step in unpinned.last_loop_action_sequence.iter_mut() {
            step.pins.clear();
        }
        assert_eq!(
            unpinned.last_loop_action_sequence.len(),
            2,
            "reach-guard anchor: the period is still present in the negative variant"
        );
        assert!(
            try_offer_object_growth_shortcut(&unpinned).is_none(),
            "without the pins the drive aborts at the unpinned tap cost ⇒ NO offer"
        );
    }

    /// [LOW-1] declined-axis ∞ lifecycle — characterization/regression guard (memory:
    /// combo-interruptibility-acceptance-criterion). A declined `Counters`/`Life` axis leaves its
    /// ∞ capability marker in `unbounded_resources` intentionally (CR 732.2b never forces a
    /// shortcut). This test guards the MEASURED retirement path (a) documented at the boundary
    /// seam: the empty-stack offer hook `try_offer_object_growth_shortcut` (engine.rs:472) is NOT
    /// gated by existing ∞ marks, so a later genuine re-detection RE-OFFERS the loop and can
    /// re-collapse the declined axis once the observer is gone.
    ///
    /// DISCRIMINATING LEG (the re-offer assertion): with a pre-existing declined ∞ mark injected
    /// for P0, the offer STILL fires. If a future regression ∞-gated the offer hook (e.g. to
    /// suppress re-offering a declined axis), this flips to `None`. Positive control / reach-guard:
    /// the SAME state WITHOUT the mark also offers (proving the mark is what the assertion isolates,
    /// and the recorded 2-step period is intact — a `None` would be a drive-abort, not a missing
    /// sequence).
    #[test]
    fn declined_infinity_mark_does_not_suppress_reoffer() {
        use crate::analysis::resource::ResourceAxis;

        let mut driven = load_migrated_dump();
        drive_one_live_cycle(&mut driven);
        let base = at_priority_window(driven);

        // Reach-guard anchor: the recorded period is present (a `None` below is a real gating
        // decision, never an empty-sequence artifact).
        assert_eq!(
            base.last_loop_action_sequence.len(),
            2,
            "reach-guard: the live cycle recorded the clean 2-step pinned period"
        );
        // Positive control: without any ∞ mark the intact loop re-derives the offer.
        assert!(
            try_offer_object_growth_shortcut(&base).is_some(),
            "positive control: the intact loop offers when no ∞ mark is present"
        );

        // Inject a pre-existing DECLINED ∞ axis for P0 (as if an earlier boundary declined the life
        // axis and left it ∞-marked for manual play). The offer hook reads `waiting_for` + stack +
        // `samples()` + `last_loop_action_sequence` — never `unbounded_resources` — so the mark
        // must NOT suppress the re-offer.
        let mut marked = base.clone();
        marked.mark_unbounded_loop(P0, &[ResourceAxis::Life(P0)]);
        assert!(
            marked
                .unbounded_resources
                .get(&P0)
                .is_some_and(|axes| axes.contains(&ResourceAxis::Life(P0))),
            "reach-guard: the declined ∞ Life mark is present on the probed state"
        );
        assert!(
            try_offer_object_growth_shortcut(&marked).is_some(),
            "the empty-stack offer hook is NOT ∞-gated: a persisted declined ∞ axis does not \
             suppress a genuine re-detection re-offering the loop (CR 732.2a / CR 732.2b)"
        );
    }

    /// Plant ONE extra `Targets` pin into the recorded period's first step, on a slot that
    /// no prompt in the replay answers. That is what makes these three rows isolate the
    /// OFFER-BUILDER: the drive never consults the planted pin, while
    /// `pinned_decisions_to_points` — which builds its points from exactly
    /// `build_recast_template(&seq[0]).decisions` — always does.
    fn plant_offer_pin(state: &mut GameState, pin: TargetPin) -> GameState {
        let mut planted = state.clone();
        let step = planted
            .last_loop_action_sequence
            .first_mut()
            .expect("reach-guard: the recorded period has a first step to plant into");
        step.pins.push(PinnedDecision::Targets {
            slot: crate::analysis::decision_template::DecisionSlot {
                source: YieldTarget::ThisObject {
                    source_id: KILO,
                    incarnation: None,
                    trigger_description: None,
                },
                // An index no live prompt publishes, so only the point-builder reads it.
                index: 99,
            },
            targets: vec![pin],
        });
        planted
    }

    /// R4b — CR 732.2a: *"a sequence of game choices ... that may be legally taken based on
    /// the current game state"*. If a pinned target no longer resolves, there IS no such
    /// sequence, so the offer must be WITHDRAWN — not published with a short legal set.
    ///
    /// Before the fix, `pinned_decisions_to_points` `filter_map`ped the unresolvable pin out
    /// of `legal_targets` while keeping `min_targets = targets.len()`, publishing a point
    /// that no legal declaration could satisfy: the player is offered a shortcut they cannot
    /// take, and the failure surfaces later as `IllegalPinValue` instead of as "no offer".
    ///
    /// MATCHED PAIR on one board, one variable — the planted pin's identity:
    ///   * live object  ⇒ the point resolves ⇒ the offer FIRES (the reach-guard: it proves
    ///     the plant itself does not break the drive, so the negative arm's `None` is the
    ///     withdrawal and not a broken fixture);
    ///   * absent object ⇒ the offer is WITHDRAWN.
    ///
    /// REVERT-PROBE: restore the `filter_map` (drop the `?`) ⇒ the negative arm publishes an
    /// undeclarable point instead of withdrawing ⇒ FAILS. Reachable TODAY and independent of
    /// item 1: this arm's pin is `ByIdentity`, and no player legality is involved.
    #[test]
    fn an_unresolvable_identity_pin_withdraws_the_offer() {
        let mut driven = load_migrated_dump();
        drive_one_live_cycle(&mut driven);
        let base = at_priority_window(driven);

        let live = plant_offer_pin(
            &mut base.clone(),
            TargetPin::ByIdentity(YieldTarget::ThisObject {
                source_id: KILO,
                incarnation: None,
                trigger_description: None,
            }),
        );
        assert!(
            try_offer_object_growth_shortcut(&live).is_some(),
            "reach-guard: a planted pin that RESOLVES leaves the offer intact, so the \
             negative arm below is the withdrawal and not the plant"
        );

        let mut dangling = plant_offer_pin(
            &mut base.clone(),
            TargetPin::ByIdentity(YieldTarget::ThisObject {
                source_id: ObjectId(999_999),
                incarnation: None,
                trigger_description: None,
            }),
        );
        assert!(
            !dangling.objects.contains_key(&ObjectId(999_999)),
            "setup: the planted identity must genuinely be absent from the board"
        );
        assert!(
            try_offer_object_growth_shortcut(&dangling).is_none(),
            "CR 732.2a: a pin that cannot resolve withdraws the offer"
        );
        // The withdrawal is the whole point: nothing was published to be declared.
        assert!(matches!(dangling.waiting_for, WaitingFor::Priority { .. }));
        dangling.last_loop_action_sequence.clear();
    }

    /// R4d — the same withdrawal, on the OTHER end of the invariant: a `TargetPin::Player`
    /// aimed at a seat that has left the game (CR 800.4 + CR 102.1) no longer resolves, so
    /// the offer is withdrawn rather than ratifying its own pin.
    ///
    /// This is the offer-builder half of the pair whose MINT half is
    /// `effects::proliferate`'s R4a row: the two ends of the invariant that a Player pin
    /// must never reach materialization validated only against a legal set derived from the
    /// pins themselves. `pinned_decisions_to_points` derives its legal sets FROM the pins,
    /// so on this route the seat's existence check inside `resolve_target` is the only
    /// authority there is.
    ///
    /// MATCHED PAIR, one variable (`is_eliminated`): live seat ⇒ offer fires; departed seat
    /// ⇒ offer withdrawn. REVERT-PROBE: drop the existence conjunct in
    /// `players::player_exists_for_choice` ⇒ the departed seat resolves ⇒ the offer is
    /// published ⇒ FAILS.
    #[test]
    fn a_departed_player_pin_withdraws_the_offer() {
        let mut driven = load_migrated_dump();
        drive_one_live_cycle(&mut driven);
        let base = at_priority_window(driven);
        let victim = PlayerId(1);

        let live = plant_offer_pin(&mut base.clone(), TargetPin::Player(victim));
        assert!(
            !live.players[1].is_eliminated,
            "reach-guard: the seat starts in the game"
        );
        assert!(
            try_offer_object_growth_shortcut(&live).is_some(),
            "reach-guard: a Player pin on a LIVE seat leaves the offer intact"
        );

        let mut departed = live.clone();
        departed.players[1].is_eliminated = true;
        assert!(
            try_offer_object_growth_shortcut(&departed).is_none(),
            "a Player pin aimed at a departed seat withdraws the offer (CR 732.2a)"
        );
    }

    /// R2b — the CR 115.10a boundary, enforced at the OFFER level rather than explained in a
    /// comment. A shrouded seat (CR 702.18a) is un-TARGETable, and this row proves the
    /// offer-builder still publishes it as a CHOICE: the point carries it in
    /// `legal_targets`, and the offer FIRES.
    ///
    /// This is the enforcement half of the pair whose explanation half is the site-4 comment
    /// and whose seam-level half is
    /// `analysis::decision_template::tests::a_shrouded_seat_is_untargetable_yet_still_
    /// choosable_at_the_pin_recheck`. Without it, re-introducing target-scoped conjuncts at
    /// site 4 would silently restore the over-veto at the one seam that publishes an offer.
    ///
    /// REVERT-PROBE: route `resolve_target`'s `TargetPin::Player` arm through
    /// `targeting::player_is_legal_target` ⇒ the shrouded seat stops resolving ⇒ the offer is
    /// WITHDRAWN ⇒ both assertions FAIL. The paired positive that keeps this from passing on
    /// an un-shrouded board is the shroud reach-guard asserted first.
    #[test]
    fn a_shrouded_player_pin_is_still_published_by_the_offer_builder() {
        use crate::types::statics::StaticMode;

        let mut driven = load_migrated_dump();
        drive_one_live_cycle(&mut driven);
        let mut base = at_priority_window(driven);
        let victim = PlayerId(1);

        // P1 gains shroud, through the single TCE construction authority — the same route
        // a resolved "target player gains shroud until end of turn" effect takes.
        base.add_transient_continuous_effect(
            KILO,
            P0,
            crate::types::ability::Duration::UntilEndOfTurn,
            crate::types::ability::TargetFilter::SpecificPlayer { id: victim },
            vec![
                crate::types::ability::ContinuousModification::AddStaticMode {
                    mode: StaticMode::Shroud,
                },
            ],
            None,
        );
        crate::game::layers::flush_layers(&mut base);

        // Reach-guard, and the paired positive: the shroud actually bites at the TARGET
        // seam. Without this the row would pass on a board with no shroud at all.
        assert!(
            crate::game::static_abilities::player_cannot_be_targeted_by(&base, victim, KILO, P0),
            "CR 702.18a: the planted shroud must make the seat un-targetable"
        );

        let planted = plant_offer_pin(&mut base, TargetPin::Player(victim));
        let (_, schema) = try_offer_object_growth_shortcut(&planted)
            .expect("CR 115.10a: a shrouded seat is still CHOOSABLE, so the offer fires");
        assert!(
            schema.points.iter().any(|point| matches!(
                &point.kind,
                crate::analysis::decision_template::DecisionPointKind::Targets { legal_targets, .. }
                    if legal_targets.contains(&TargetRef::Player(victim))
            )),
            "the published point carries the shrouded seat: exclusion belongs to the TARGET \
             seam, not to this one"
        );
    }
}

/// FIX ROUND 1 (MED-2) — a named negative row per [`try_offer_bounded_cycle_shortcut`] conjunct
/// that no tracked test was exercising.
///
/// The reviewer measured all three by disabling them on the PRE-ROW tree: step (2)
/// `ProposerIsNotActivePlayer` and step (5) `AdvantageOnlyCycle` could each be deleted with the
/// whole suite still green, and only `DrivingSequenceNotEmpty` was asserted by name anywhere.
/// A conjunct no row can name is a conjunct nobody notices losing.
///
/// ⚠ The pass COUNT that used to appear here ("4167 passed / 0 failed") is deleted rather than
/// re-dressed, because it shipped with no runner and no filter recorded beside it and a bare
/// count means nothing without both (fix round 2, LOW-2: a count in this file named a shape it
/// did not have). It is also not reproducible on this tree BY DESIGN — the rows below now exist,
/// so deleting either conjunct today flips its named row, which is the entire point. Each row's
/// own REVERT-PROBE line is the reproducible claim; run it with
/// `cargo test -p phase-engine --lib -- game::engine::bounded_offer_conjunct_tests::` (module filter on
/// the `engine` lib target).
///
/// # Why these are UNIT rows on a synthetic ring
///
/// Each row must reach ONE conjunct and refuse there, which means holding every earlier conjunct
/// satisfied on purpose. A ring is the input the certification step reads, and
/// `GameState::normalize_for_loop` is `pub(crate)`, so an integration test cannot build one. The
/// refusal is asserted BY REASON (`BoundedOfferRefusal`), never as a bare "no offer": a row that
/// only observes absence silently stops testing its own conjunct the moment an EARLIER one
/// starts refusing first, which is the domination trap the enum exists to close.
#[cfg(test)]
mod bounded_offer_conjunct_tests {
    use super::{try_offer_bounded_cycle_shortcut, BoundedOfferRefusal};
    use crate::game::scenario::GameScenario;
    use crate::types::game_state::{GameState, LoopDetectionMode, WaitingFor};
    use crate::types::player::PlayerId;

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    /// A 2-player board parked at `Priority{P0}` (P0 active) whose retained ring encodes a
    /// period seen twice: `frames` successive normalized snapshots, each mutated by `shape`.
    ///
    /// `2k + 1 = 3` frames at `k = 1` is the smallest ring `ring_delta_signature` will certify,
    /// and every frame shares `turn_number` / `phase` / `extra_phases`, so the CR 703.1
    /// turn-position conjunct passes and this fixture is not silently testing that instead.
    fn ring_state(frames: usize, shape: impl Fn(&mut GameState, usize)) -> GameState {
        let mut scenario = GameScenario::new_n_player(2, 7);
        // A stocked library is load-bearing, not scenery: the period this fixture encodes IS a
        // library delta, and an empty library makes every frame identical ⇒ a zero per-period
        // vector ⇒ `ring_delta_signature` returns `None` and every row below refuses at
        // `NoCertification` instead of at the conjunct it is about.
        let names: Vec<String> = (0..40).map(|i| format!("Filler {i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        scenario.with_library_top(P0, &refs);
        scenario.with_library_top(P1, &refs);
        let mut runner = scenario.build();
        let mut state = runner.state_mut().clone();
        state.loop_detection = LoopDetectionMode::Interactive;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.active_player = P0;
        state.last_loop_action_sequence.clear();
        for i in 0..frames {
            let mut frame = state.clone();
            shape(&mut frame, i);
            // Both halves built exactly as `record_loop_detect_sample` builds them.
            state
                .loop_detect_ring
                .push_back(std::sync::Arc::new(crate::types::LoopDetectSample {
                    normalized: frame.normalize_for_loop(),
                    live: frame.loop_detect_live_sample(),
                }));
        }
        state
    }

    /// R20 — RELIEF-PATH BUDGET STARVATION IS COVERAGE, NEVER SOUNDNESS.
    ///
    /// CR 732.2a. The named regression row for the branch NOT taken (hoist only the primary
    /// and let the budget absorb the relief path). A board whose non-exempt stack carries more
    /// in-scope chain LINKS than the cap can pay for must REFUSE — no certificate consumed, no
    /// offer, no `WaitingFor::LoopShortcut` write — and the refusal must be attributable to
    /// the budget rather than to any upstream conjunct.
    ///
    /// THE BOUND IS PER-LINK, NOT PER-ENTRY, which is why the last entry is CHAINED: the
    /// classifier recurses into `sub_ability`, so an N-link chain charges up to N. A per-entry
    /// regression passes a per-entry row and fails this one.
    ///
    /// THREE CONJUNCTS, because `spent == cap` alone proves consumption, not BINDINGNESS:
    /// (i) the SAME board offers once the budget stops binding (`ProbeCap::RaisedTwiceLinks`,
    /// board-derived so no arbitrary raise is representable) — a matched positive on the
    /// constructed board, not on a proxy; (ii) at the shipped cap the refusal is
    /// `UnspecifiedChoiceWindow` with `denied == true` at `spent == PROBE_BUDGET`; (iii) a
    /// PRE-CHARGE refusal on the same board reads a clean meter, which is the control that
    /// keeps `denied` meaningful.
    ///
    /// (ii)'s VARIANT is reachable only because this board's basis-A match comes from the
    /// EQUALITY disjunct: the `||` short-circuits, the charging cover call never runs, and
    /// exhaustion lands one gate later in `stack_choices_are_all_specified`. That precondition
    /// is carried as an EXECUTABLE reach-guard, not as prose, so a fixture drift to the cover
    /// path fails loudly instead of silently flipping the variant.
    ///
    /// REVERT-PROBE: delete `probe_resolution`'s `try_charge_one` arm ⇒ the exhausted budget
    /// falls through to the clone-and-resolve ⇒ the over-budget board OFFERS ⇒ FLIPS.
    #[test]
    fn r20_an_over_budget_relief_path_refuses_instead_of_certifying() {
        use crate::analysis::resource::{loop_states_equal_modulo_resources, PROBE_BUDGET};
        use crate::game::engine::{try_offer_bounded_cycle_shortcut_metered, ProbeCap};

        // One more entry than the cap can pay for, with the last one CHAINED so the population
        // is links rather than entries.
        let entries = PROBE_BUDGET as usize + 1;
        let state = equality_ring_with_stack(entries, true);

        // ── reach-guards, all three before any verdict ──────────────────────────────────
        assert!(
            state.loop_detect_ring.len() >= 2,
            "REACH-GUARD: a ring-starved board refuses at the ring gate with a clean meter and \
             this row would pass for the wrong reason"
        );
        assert_eq!(
            state.stack.len(),
            entries,
            "REACH-GUARD: the non-exempt population must exceed the cap ({PROBE_BUDGET})"
        );
        let prior = &state.loop_detect_ring[state.loop_detect_ring.len() - 2].normalized;
        assert!(
            loop_states_equal_modulo_resources(prior, &state),
            "REACH-GUARD (the (ii) precondition, executable rather than prose): this board must \
             match basis A through the EQUALITY disjunct. On a cover-matched board the charging \
             cover call exhausts FIRST and the refusal is `NoCertification` with the same meter"
        );

        // ── (i) THE SAME BOARD OFFERS once the budget stops binding ─────────────────────
        let (raised, raised_meter) =
            try_offer_bounded_cycle_shortcut_metered(&state, false, ProbeCap::RaisedTwiceLinks);
        assert!(
            raised.is_ok(),
            "(i) with the cap raised to 2x the board's own link count the SAME board must \
             certify and offer — this is what pins the budget as the binding refusal below, \
             rather than some upstream conjunct. Got {raised:?}, meter {raised_meter:?}"
        );
        // Keyed to the POPULATION counter, not to `spent`, and deliberately: `spent` measures
        // CHARGING, so a revert-probe that deletes the charge arm would abort this conjunct
        // first and mask the flip belonging to (ii). `conjunct6_asks` measures what the gate
        // actually examined, which is the evidence (i) is here to give.
        assert!(
            !raised_meter.denied && raised_meter.conjunct6_asks >= entries as u32,
            "(i) the raised arm must have examined the whole non-exempt population without \
             denial; meter {raised_meter:?}"
        );

        // ── (ii) AT THE SHIPPED CAP: refuse, exhausted, and no offer ────────────────────
        let (refused, meter) =
            try_offer_bounded_cycle_shortcut_metered(&state, false, ProbeCap::Shipped);
        assert!(
            matches!(refused, Err(BoundedOfferRefusal::UnspecifiedChoiceWindow)),
            "(ii) exhaustion reads `Prompted`, so the step-6 predicate goes false and the \
             refusal is an unspecified window. Got {refused:?}, meter {meter:?}"
        );
        assert!(
            meter.denied && meter.spent == PROBE_BUDGET,
            "(ii) the cap must be CONSUMED and the denial latched — that pair is the \
             exhaustion witness the refusal variant alone cannot carry. meter {meter:?}"
        );

        // ── (iii) THE CLEAN-METER CONTROL, keyed to a PRE-CHARGE refusal on the SAME board ─
        let mut not_at_priority = state.clone();
        not_at_priority.waiting_for = WaitingFor::DiscardToHandSize {
            player: P0,
            count: 1,
            cards: Vec::new(),
        };
        let (control, control_meter) =
            try_offer_bounded_cycle_shortcut_metered(&not_at_priority, false, ProbeCap::Shipped);
        assert!(
            matches!(control, Err(BoundedOfferRefusal::NotAtPriority)),
            "(iii) the control arm must refuse UPSTREAM of the first charge; got {control:?}"
        );
        assert!(
            !control_meter.denied && control_meter.spent == 0,
            "(iii) a pre-charge refusal reads a clean meter BY CONTROL-FLOW POSITION — without \
             this control, `denied` would look like a property of refusals in general \
             rather than of exhaustion. meter {control_meter:?}"
        );
    }

    /// R33 arm (a′2) — THE SELECTION SITE: AN EQUALITY-CERTIFIED CANDIDATE CARRIES NO FROZEN
    /// EXEMPTION, EVEN THOUGH ITS WINDOW HAS ONE AVAILABLE.
    ///
    /// CR 732.2a + CR 608.1. Arms (a)/(b)/(a′1) prove the CONSTRUCTOR keys the subtraction to
    /// the certificate value. This arm proves §3 D2's step 4b actually SELECTS the right
    /// value: it is the only arm that fails on the round-39 shape (one `BoardCovered`
    /// certificate for the whole `equality || cover` disjunction), which every other arm in
    /// the row passes unchanged.
    ///
    /// The fixture is CONSTRUCTED, not fixture-mined: `drain_ring`'s frames are board-equal at
    /// every index, so `loop_states_equal_modulo_resources` matches and — by the mutual
    /// exclusivity of the two disjuncts (equality = constant depth, cover = strictly growing
    /// depth) — `stack_covers` cannot. One stack entry is seeded IDENTICALLY into `current`
    /// and into both halves of every retained frame, which is what makes the window carry a
    /// non-empty observed-frozen set: the exemption is genuinely AVAILABLE here, and the row
    /// is about it being genuinely WITHDRAWN.
    ///
    /// REVERT-PROBE 3 (the round's signature probe): delete step 4b and let conjunct (6)
    /// consume step 3's `touch_cover` directly ⇒ this row FLIPS while (a)/(b)/(a′1)/(c) all
    /// still pass — which is exactly why the arm exists.
    #[test]
    fn r33_equality_certified_candidate_carries_no_frozen_exemption() {
        use crate::analysis::resource::{certified_period_touch, PeriodCertification};
        use crate::game::engine::{try_offer_bounded_cycle_shortcut_metered, ProbeCap};

        let state = equality_ring_with_stack(1, false);

        // REACH-GUARD 1: the exemption is genuinely AVAILABLE on this candidate's window —
        // otherwise "withdrawn" is indistinguishable from "there was nothing to withdraw".
        let live: Vec<&GameState> = state.loop_detect_ring.iter().map(|f| &f.live).collect();
        let window = &live[live.len() - 2..];
        let available = certified_period_touch(window, &state, PeriodCertification::BoardCovered);
        assert!(
            !available.frozen_ids.is_empty(),
            "REACH-GUARD: the constructed window must carry a non-empty observed-frozen set"
        );

        let (outcome, meter) =
            try_offer_bounded_cycle_shortcut_metered(&state, false, ProbeCap::Shipped);

        // REACH-GUARD 2: the EQUALITY arm is the one that matched. Without this the row could
        // pass on a candidate that never certified, or one that took the cover disjunct.
        assert_eq!(
            meter.certification,
            Some(PeriodCertification::BoardEqualOnly),
            "REACH-GUARD: the constructed pair must certify through the EQUALITY disjunct, \
             which is the case this arm is about; outcome {outcome:?}"
        );

        // THE RULING: equality supplies P2 but not P4, so the period carried forward exempts
        // NOTHING — observable as conjunct (6) skipping nothing while it really does run.
        assert_eq!(
            meter.conjunct6_frozen_skips, 0,
            "(a′2) step 4b must REBUILD the equality candidate's period with the subtraction \
             withdrawn; a non-zero skip count means the `BoardCovered` touch from step 3 was \
             carried into conjunct (6). meter {meter:?}"
        );
        assert!(
            meter.conjunct6_asks > 0,
            "REACH-GUARD against a vacuous skip count: conjunct (6) must actually have RUN, \
             else `skips == 0` is trivially true. meter {meter:?}"
        );
    }

    /// Mill `victim` by one card per retained frame — a constant per-frame library delta, which
    /// is a period observed twice at `frames >= 3`.
    fn mill_ring(victim: PlayerId, frames: usize) -> GameState {
        ring_state(frames, move |frame, i| {
            let player = frame
                .players
                .iter_mut()
                .find(|p| p.id == victim)
                .expect("seat exists");
            for _ in 0..i {
                player.library.pop_back();
            }
        })
    }

    /// Drain `victim` by one life per retained frame — a constant per-frame LIFE delta.
    ///
    /// The counterpart to [`mill_ring`], and the difference is exactly the one basis A turns on:
    /// library size is BOARD (`loop_states_equal_modulo_resources` compares it), while life is a
    /// PROJECTED resource (`project_out_resources` removes it). So a mill ring can only certify
    /// through basis B's `ring_delta_signature`, whereas a drain ring's frames are board-EQUAL
    /// at every index and certify through basis A's first disjunct.
    ///
    /// ⚠ The REASON basis A refuses a mill ring is not uniform across the ring, and an earlier
    /// revision of this doc claimed it was ("a mill ring's frames are board-UNEQUAL"). MEASURED
    /// in fix round 3 (LOW-5) and recorded at
    /// [`a_zero_span_certifying_pair_never_publishes_a_zero_width_period`]: `mill_ring`'s frames
    /// are board-unequal at every index EXCEPT the OLDEST, which pops zero cards and IS
    /// board-equal (`span = 2`, `eq = true`) — that one is refused by `net_progress_for` on its
    /// zero δ instead. Basis A still certifies nothing on a mill ring; only the per-index reason
    /// differs.
    ///
    /// Frame `i` sits `frames - i` life ABOVE the live state, so the newest frame is exactly one
    /// period ahead of it and every older frame one more — i.e. the live state is the far end of
    /// the period, which is the orientation `ResourceVector::delta(prior, current)` reads.
    fn drain_ring(victim: PlayerId, frames: usize) -> GameState {
        ring_state(frames, move |frame, i| {
            let player = frame
                .players
                .iter_mut()
                .find(|p| p.id == victim)
                .expect("seat exists");
            player.life += (frames - i) as i32;
        })
    }

    /// A [`drain_ring`] whose stack carries `links` in-scope chain links, seeded IDENTICALLY
    /// into `current` and into both halves of every retained frame.
    ///
    /// Two properties come out of that one construction, which is why both rows share it:
    /// board equality survives (so basis A matches on its FIRST disjunct), and every entry
    /// sits at the SAME INDEX in every window frame (so the window carries a non-empty
    /// observed-frozen set — the thing the certificate is allowed to subtract, or not).
    ///
    /// Each entry is a MANDATORY, choice-free `LoseLife`, so conjunct (6) can accept it and a
    /// refusal is never attributable to an unspecified choice. `chained` gives the LAST entry
    /// a `sub_ability`, which the classifier recurses into: that is what makes the budget a
    /// per-LINK bound rather than a per-ENTRY one, and what a per-entry regression would miss.
    fn equality_ring_with_stack(entries: usize, chained: bool) -> GameState {
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility};
        use crate::types::game_state::{StackEntry, StackEntryKind};
        use crate::types::identifiers::{CardId, ObjectId};
        use crate::types::LoopDetectSample;
        use std::sync::Arc;

        let mut state = drain_ring(P1, 3);
        let src = ObjectId(941);
        let mut source = crate::game::game_object::GameObject::new(
            src,
            CardId(0),
            P0,
            "Frozen Ticker".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        source.incarnation = 3;
        let lose_one = || {
            ResolvedAbility::new(
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: None,
                },
                vec![],
                src,
                P0,
            )
        };
        let built: Vec<StackEntry> = (0..entries)
            .map(|i| {
                let mut ability = lose_one();
                if chained && i + 1 == entries {
                    ability.sub_ability = Some(Box::new(lose_one()));
                }
                StackEntry {
                    id: ObjectId(951 + i as u64),
                    source_id: src,
                    controller: P0,
                    kind: StackEntryKind::TriggeredAbility {
                        source_id: src,
                        ability: Box::new(ability),
                        condition: None,
                        trigger_event: None,
                        description: None,
                        source_name: String::new(),
                        subject_match_count: None,
                        die_result: None,
                    },
                }
            })
            .collect();
        let inject = |g: &mut GameState| {
            g.objects.insert(src, source.clone());
            g.battlefield.push_back(src);
            for e in &built {
                g.stack.push_back(e.clone());
            }
        };
        inject(&mut state);
        let frames: std::collections::VecDeque<Arc<LoopDetectSample>> = state
            .loop_detect_ring
            .iter()
            .map(|s| {
                let mut normalized = s.normalized.clone();
                let mut live = s.live.clone();
                inject(&mut normalized);
                inject(&mut live);
                Arc::new(LoopDetectSample { normalized, live })
            })
            .collect();
        state.loop_detect_ring = frames;
        state
    }

    /// STEP (2) `ProposerIsNotActivePlayer`. CR 732.2a lets the player with priority propose;
    /// this conjunct additionally requires that player to be the ACTIVE one, because the ring
    /// sampler only samples at `Priority{active_player}` — which is what establishes the
    /// proposer held priority at every certified frame.
    ///
    /// REVERT-PROBE: delete the conjunct ⇒ arm ⓑ stops returning `ProposerIsNotActivePlayer`
    /// and falls through to a later conjunct (or an offer) ⇒ FAILS.
    #[test]
    fn a_non_active_priority_holder_mints_no_bounded_offer() {
        let mut state = mill_ring(P1, 3);

        // ⓐ REACH-GUARD / positive control: the SAME ring certifies for the active player, so
        //   ⓑ's refusal is attributable to the seat and not to an unsatisfied earlier conjunct.
        let armed = try_offer_bounded_cycle_shortcut(&state, false);
        assert_ne!(
            armed,
            Err(BoundedOfferRefusal::ProposerIsNotActivePlayer),
            "the active player must NOT be refused by step (2); got {armed:?}"
        );
        assert_ne!(
            armed,
            Err(BoundedOfferRefusal::NoCertification),
            "REACH-GUARD: the ring must actually certify, else ⓑ never reaches step (2); \
             got {armed:?}"
        );

        // ⓑ one field reassigned: priority moves to the non-active seat.
        state.waiting_for = WaitingFor::Priority { player: P1 };
        assert_eq!(
            try_offer_bounded_cycle_shortcut(&state, false),
            Err(BoundedOfferRefusal::ProposerIsNotActivePlayer),
            "CR 732.2a: the ring sampler gates on `Priority{{active_player}}`, so a proposer \
             who is not the active player did not hold priority at the certified frames"
        );
    }

    /// STEP (5) `AdvantageOnlyCycle`. CR 732.2a: this producer's whole claim is that it measured
    /// a CR 704 threshold INSIDE the loop and divided the headroom by the per-period magnitude.
    /// An `Advantage` cycle drives nobody toward such a threshold, so it has no bound to state
    /// and belongs to Path C's revocable-infinity mark instead.
    ///
    /// The pair is a SELF-mill against an OPPONENT-mill, which is exactly the discrimination
    /// `classify_win_kind` makes: `Decking` requires "an unbounded downward library delta on a
    /// player other than the loop's controller", so a controller milling themselves falls
    /// through to `Advantage`. Without this conjunct that self-mill takes Path D and is offered
    /// a bound — `elimination_bounds` narrows on `narrow(p.library.len(), -library_delta[p])`
    /// for the PROPOSER too, so it happily produces one.
    ///
    /// REVERT-PROBE: delete the conjunct ⇒ arm ⓑ stops returning `AdvantageOnlyCycle` ⇒ FAILS.
    #[test]
    fn a_self_mill_advantage_cycle_mints_no_bounded_offer() {
        // ⓐ POSITIVE CONTROL: the SAME shape aimed at the OPPONENT is `Decking`, not
        //   `Advantage`, so step (5) must let it through. Without this arm ⓑ would pass for a
        //   fixture that simply never certifies.
        let opponent_mill = try_offer_bounded_cycle_shortcut(&mill_ring(P1, 3), false);
        assert_ne!(
            opponent_mill,
            Err(BoundedOfferRefusal::AdvantageOnlyCycle),
            "CR 104.3c: milling an OPPONENT is a win kind, not an advantage engine; got \
             {opponent_mill:?}"
        );
        assert_ne!(
            opponent_mill,
            Err(BoundedOfferRefusal::NoCertification),
            "REACH-GUARD: the mill ring must certify, else neither arm reaches step (5); got \
             {opponent_mill:?}"
        );

        // ⓑ the same period, victim = the proposer.
        assert_eq!(
            try_offer_bounded_cycle_shortcut(&mill_ring(P0, 3), false),
            Err(BoundedOfferRefusal::AdvantageOnlyCycle),
            "CR 732.2a: a cycle that drives nobody toward a CR 704 threshold has no bound to \
             state, so it belongs to Path C's revocable-infinity mark, not to this seam"
        );
    }

    /// STEP (7) `NoNarrowedLegalCount`, LOWER end. `elimination_bounds` returning 0 states that
    /// no repetition is legal at all — a seat is already AT the CR 704 threshold's last legal
    /// step — and `1..MAX_SHORTCUT_CYCLES` refuses it rather than minting a `Fixed(0)` offer
    /// whose acceptance would commit nothing while spending the CR 732.2b window.
    ///
    /// ⚠ SCOPE, stated because the reviewer's probe targeted the OTHER end. Widening the check
    /// to `1..=MAX_SHORTCUT_CYCLES` flips nothing in the tracked suite, and that is not an
    /// oversight: the upper end is DOMINATED by step (5). A bound of exactly
    /// `MAX_SHORTCUT_CYCLES` means no axis narrowed, i.e. the period drives no living seat
    /// toward any CR 704 threshold, which is precisely what `classify_win_kind` reports as
    /// `Advantage` — so such a cycle has already been refused two conjuncts earlier. This row
    /// therefore covers the reachable end and names the reason the other is unreachable rather
    /// than leaving it as an untested branch of unknown status.
    ///
    /// REVERT-PROBE: change the range to `0..MAX_SHORTCUT_CYCLES` ⇒ arm ⓑ mints an offer ⇒ FAILS.
    #[test]
    fn a_bound_of_zero_mints_no_bounded_offer() {
        // ⓐ POSITIVE CONTROL: a full library certifies and narrows to a legal count.
        let healthy = try_offer_bounded_cycle_shortcut(&mill_ring(P1, 3), false);
        assert!(
            healthy.is_ok(),
            "REACH-GUARD: the un-narrowed fixture must OFFER, else ⓑ's refusal could come from \
             any earlier conjunct; got {healthy:?}"
        );

        // ⓑ the same ring, with the victim's library already empty at the offer beat: CR 104.3c
        //   headroom 0 ⇒ `0 / 1 == 0` ⇒ no legal repetition count.
        let mut state = mill_ring(P1, 3);
        state
            .players
            .iter_mut()
            .find(|p| p.id == P1)
            .expect("seat exists")
            .library
            .clear();
        assert_eq!(
            try_offer_bounded_cycle_shortcut(&state, false),
            Err(BoundedOfferRefusal::NoNarrowedLegalCount),
            "CR 104.3c: with zero cards left there is no legal repetition, and a `Fixed(0)` \
             offer would spend the CR 732.2b response window to commit nothing"
        );
    }

    /// FIX ROUND 2 — basis A's `span >= 1` fail-closed guard, which shipped in fix round 1 with
    /// no row of its own.
    ///
    /// The basis-A walk is `.rev()`, so the FIRST candidate it tries is `ring.last()` — the
    /// sample `pass_priority_once_with_pipeline` recorded at this very beat, whose span from the
    /// current state is 0. In a production trajectory that pair carries a zero δ and dies on
    /// `net_progress_for`, but nothing structural forces that: this fixture's newest retained
    /// frame is board-equal to the live state and its δ IS net progress (the reach-guards below
    /// assert exactly that, so the guard is provably reached rather than assumed to be).
    ///
    /// The fixture is a [`drain_ring`], NOT a [`mill_ring`], and the difference is load-bearing:
    /// library size is BOARD, so basis A certifies NOTHING on a mill ring, every mill-ring row in
    /// this module is really exercising basis B, and a guard inside the basis-A walk is
    /// unreachable from that fixture.
    ///
    /// ⚠ Basis A is a DISJUNCTION, and the board-equality reach-guard below — which does FAIL on
    /// `mill_ring(P1, 3)` — measures only its FIRST half. Fix round 3 (LOW-5) MEASURED the second,
    /// `loop_states_cover_modulo_growth_pinned`, rather than leaving the wider claim resting on
    /// the narrower evidence: a probe evaluating both disjuncts plus `net_progress_for` at every
    /// `(prior, live)` pair of `mill_ring(P1, 3)` (temporary `#[test]` in this module, run with
    /// `cargo test -p phase-engine --lib -- game::engine::bounded_offer_conjunct_tests:: --nocapture`,
    /// then reverted) reports `cover = false` at ALL THREE ring indices. Both halves refuse, so
    /// the conclusion holds.
    ///
    /// The probe also corrects the REASON at one index, which the board-inequality phrasing had
    /// wrong: `mill_ring`'s OLDEST frame pops zero cards, so it IS board-equal to the live state
    /// (`span = 2`, `eq = true`) and is refused by `net_progress_for` on its zero δ, not by board
    /// inequality. Newest-first, the `.rev()` walk therefore refuses `span = 0` at `span >= 1`,
    /// `span = 1` on `eq`/`cover` both false, and `span = 2` on net progress. (`drain_ring(P1, 3)`
    /// under the same probe: `eq = true` and `net = true` at every index, `cover = false`
    /// throughout — so it certifies through the FIRST disjunct at `span = 1`, which is the pair
    /// this row's guard is about.)
    ///
    /// A published `frames_per_period: 0` would mean "one repetition spans no retained frames",
    /// which `drive_one_shortcut_cycle`'s delimiter cannot honour — `frames_this_cycle >= 0`
    /// holds before a single beat is driven, so the first settle beat would complete a "cycle"
    /// that moved nothing, and `materialize_fixed_shortcut`'s conformance check would then drop
    /// every one of them. The bounded offer would be minted, accepted, and commit nothing.
    ///
    /// REVERT-PROBE: delete `span >= 1 &&` from the basis-A closure ⇒ the span-0 pair certifies
    /// first and the published `frames_per_period` is 0 ⇒ this row FAILS.
    #[test]
    fn a_zero_span_certifying_pair_never_publishes_a_zero_width_period() {
        use crate::analysis::resource::{loop_states_equal_modulo_resources, ResourceVector};

        let state = drain_ring(P1, 3);

        // ── REACH-GUARDS: the span-0 pair really is a certifying candidate on this fixture, so
        //    the guard is what refuses it. Both halves of basis A's first disjunct, asserted on
        //    the exact pair the `.rev()` walk reaches first.
        let newest = state
            .loop_detect_ring
            .back()
            .expect("the fixture builds a ring")
            .normalized
            .clone();
        assert!(
            loop_states_equal_modulo_resources(&newest, &state),
            "REACH-GUARD: the newest retained frame must be board-equal to the live state, else \
             the span-0 pair fails the board predicate and the guard is never the refuser"
        );
        let span_zero_delta = ResourceVector::delta(
            &ResourceVector::snapshot(&newest),
            &ResourceVector::snapshot(&state),
        );
        assert!(
            span_zero_delta.net_progress_for(P0),
            "REACH-GUARD: the span-0 pair must carry net progress, else `net_progress_for` \
             refuses it first and this row would pass without the guard existing; δ \
             {span_zero_delta:?}"
        );

        let offer = try_offer_bounded_cycle_shortcut(&state, false)
            .expect("REACH-GUARD: the fixture must OFFER, else nothing publishes a period");
        let WaitingFor::LoopShortcut { certificate, .. } = &offer else {
            panic!("a bounded offer is a LoopShortcut window; got {offer:?}")
        };
        let per_cycle = certificate
            .per_cycle
            .as_ref()
            .expect("a bounded offer publishes its per-period signature");
        assert!(
            per_cycle.frames_per_period >= 1,
            "CR 732.2a: a repetition spans at least one retained ring frame — a published 0 is a \
             delimiter no drive can honour; got {}",
            per_cycle.frames_per_period
        );
    }

    // ───────────────────────────────────────────────────────────────────────────────────
    // The two CARRIER rows. Both are about WHICH object identifies a retained beat, an axis
    // that has exactly one behavioural surface (`PeriodVerdicts::frame_ix`) and one source
    // surface (this file), so each ships with one arm on each.
    // ───────────────────────────────────────────────────────────────────────────────────

    /// Symbol-anchored extent of a column-0 `fn` in THIS file, as `(head, end)` line indices —
    /// the §6 R8 self-census extractor: signature line to the first column-0 `}`.
    #[cfg(test)]
    fn engine_fn_extent(lines: &[&str], signature: &str) -> (usize, usize) {
        let head = lines
            .iter()
            .position(|l| l.starts_with(signature))
            .unwrap_or_else(|| panic!("extractor found no column-0 `{signature}`"));
        let end = lines[head..]
            .iter()
            .position(|l| *l == "}")
            .map(|i| head + i)
            .unwrap_or_else(|| panic!("`{signature}` has no column-0 closing brace"));
        assert!(
            end - head > 5,
            "`{signature}` extent {head}-{end} is degenerate — the extractor is not keyed"
        );
        (head, end)
    }

    /// Code lines (comments excluded, per R8's ruling: a comment reads nothing) of an extent
    /// that contain `needle`, as absolute line indices.
    #[cfg(test)]
    fn engine_code_hits(lines: &[&str], extent: (usize, usize), needle: &str) -> Vec<usize> {
        (extent.0..=extent.1)
            .filter(|i| !lines[*i].trim_start().starts_with("//"))
            .filter(|i| lines[*i].contains(needle))
            .collect()
    }

    /// `Arc::as_ptr` BEAT IDENTITY IS THE SAMPLE, NOT ONE OF ITS HALVES — the U0 ruling that
    /// had no falsifier until `FrameIx` existed.
    ///
    /// CR 732.2a. U0 split each retained ring element into a CR 104.4b comparand
    /// (`normalized`) and a CR 732.2a evaluable (`live`), and left `drive_one_shortcut_cycle`'s
    /// ring-advance detector on `Arc::as_ptr` — the SAMPLE's allocation — with the ruling that
    /// it must never be re-based onto a field address. U0 could not assert that: nothing at
    /// that step distinguished the two halves as identities. U3's verdict door does.
    ///
    /// ARM 1, BEHAVIOURAL — the door's identity domain is the LIVE half and ONLY it.
    /// [`crate::analysis::resource::PeriodVerdicts::frame_ix`] resolves by `std::ptr::eq`
    /// against the very table `verdict` indexes, so a beat identified by the `normalized`
    /// half is a DIFFERENT identity from the one every period-touch consumer keys on. The two
    /// field addresses are asserted distinct first, which is what makes the choice load-bearing
    /// rather than a distinction without a difference.
    ///
    /// ARM 2, STRUCTURAL — the detector still reads the sample. Two sites, both
    /// `loop_detect_ring.back().map(std::sync::Arc::as_ptr)`, and NO raw-pointer field
    /// address anywhere in the extent.
    ///
    /// REVERT-PROBE: re-base either site to `.map(|s| &s.normalized as *const _)` ⇒ arm 2's
    /// site count goes 2 → 1 AND its `as *const` count goes 0 → 1 ⇒ FLIPS. (Arm 1 is
    /// deliberately NOT reachable by that edit — it pins the property the edit would violate,
    /// so the two arms partition "is the rule still true" from "is the code still obeying it".)
    #[test]
    fn arc_as_ptr_beat_identity_is_the_sample_not_one_of_its_halves() {
        use crate::analysis::resource::PeriodVerdicts;

        // ── ARM 1: behavioural ───────────────────────────────────────────────────────────
        let state = ring_state(3, |frame, i| {
            frame.turn_number += i as u32;
        });
        assert_eq!(
            state.loop_detect_ring.len(),
            3,
            "REACH-GUARD: the ring must carry samples, else the universals below are vacuous"
        );
        // Built exactly as `bounded_cycle_offer` builds its `ring_live` — the CR 732.2a
        // evaluable half. The binding is named differently ON PURPOSE: a verbatim copy of the
        // production line makes the carrier revert-probe (a whole-line replace of that line)
        // match twice and silently no-op, which is how the probe for the sibling row below
        // failed to apply on its first run.
        let evaluable: Vec<&GameState> = state.loop_detect_ring.iter().map(|f| &f.live).collect();
        let verdicts =
            PeriodVerdicts::for_period_with_cap(&evaluable, &state, P0, 0, super::CapAuthority(()));
        for (i, sample) in state.loop_detect_ring.iter().enumerate() {
            assert!(
                !std::ptr::eq(&sample.live, &sample.normalized),
                "sample {i}: the two halves must be DISTINCT addresses, else `beat identity \
                 is the sample, not a half` is a distinction without a difference"
            );
            assert!(
                verdicts.frame_ix(&sample.live).is_some(),
                "sample {i}: the verdict door's frame table IS the live half — this is the \
                 identity every period-touch consumer keys on"
            );
            assert!(
                verdicts.frame_ix(&sample.normalized).is_none(),
                "sample {i}: the comparand half is NOT in the door's domain. A beat identity \
                 based on `&s.normalized` would therefore name an object no `FrameIx` can \
                 ever resolve — that is the concrete harm the U0 ruling forbids"
            );
        }

        // ── ARM 2: structural ────────────────────────────────────────────────────────────
        let src = include_str!("engine.rs");
        let lines: Vec<&str> = src.lines().collect();
        let extent = engine_fn_extent(&lines, "fn drive_one_shortcut_cycle(");
        // Needles ASSEMBLED at runtime so this test's own source cannot be counted by its own
        // instrument.
        let sample_identity = format!("loop_detect_ring.back().map(std::sync::Arc::{}ptr)", "as_");
        let field_address = format!("as {}const", '*');
        assert_eq!(
            engine_code_hits(&lines, extent, &sample_identity).len(),
            2,
            "the ring-advance detector must read the SAMPLE's allocation at both its before \
             and after sites, in {}-{}",
            extent.0 + 1,
            extent.1 + 1
        );
        assert_eq!(
            engine_code_hits(&lines, extent, &field_address).len(),
            0,
            "no raw-pointer FIELD address may appear in the detector's extent — that is the \
             re-basing the U0 ruling forbids"
        );
        // POSITIVE CONTROL against a dead grep, same extractor and same filter.
        assert!(
            !engine_code_hits(&lines, extent, "frames_this_cycle").is_empty(),
            "the instrument must be able to find a token that IS there"
        );
        assert!(
            engine_code_hits(&lines, extent, "certified_period_touch").is_empty(),
            "…and must not find one that is not"
        );
    }

    /// R27 (a2), STRUCTURAL HALF — THE PERIOD-TOUCH WINDOW IS CARRIED BY THE `live` HALF.
    ///
    /// CR 732.2a + CR 104.4b. The behavioural half
    /// (`analysis::resource::tests::r27_a2_every_announced_pair_carries_an_unnormalized_evaluation_board`)
    /// builds its own window, so it cannot flip on an edit to the MINT's carrier. This arm is
    /// that edit's detector: `bounded_cycle_offer` builds exactly two ring vecs — the CR 104.4b
    /// comparand from `&f.normalized` and the CR 732.2a evaluable from `&f.live` — and every
    /// `certified_period_touch` window inside `certified_bounded_cycle_offer` is sliced from
    /// the evaluable one.
    ///
    /// REVERT-PROBE: point `ring_live` at `&f.normalized` (rounds 13–33's carrier) ⇒ the
    /// `&f.live` count goes 1 → 0 ⇒ FLIPS.
    #[test]
    fn the_period_touch_window_is_carried_by_the_live_half() {
        let src = include_str!("engine.rs");
        let lines: Vec<&str> = src.lines().collect();
        let mint = engine_fn_extent(&lines, "fn bounded_cycle_offer(");
        let live_needle = format!("&f.{}", "live");
        let norm_needle = format!("&f.{}", "normalized");

        let live_hits = engine_code_hits(&lines, mint, &live_needle);
        let norm_hits = engine_code_hits(&lines, mint, &norm_needle);
        assert_eq!(
            live_hits.len(),
            1,
            "exactly ONE evaluable ring vec is built, in {}-{}",
            mint.0 + 1,
            mint.1 + 1
        );
        assert_eq!(norm_hits.len(), 1, "…and exactly one comparand ring vec");
        assert!(
            lines[live_hits[0]].contains("ring_live"),
            "the evaluable half must be the one bound to `ring_live`; line {} reads `{}`",
            live_hits[0] + 1,
            lines[live_hits[0]].trim()
        );

        let certified = engine_fn_extent(&lines, "fn certified_bounded_cycle_offer<'a>(");
        let touch_needle = format!("certified_period{}touch(", '_');
        let touch_sites = engine_code_hits(&lines, certified, &touch_needle);
        assert!(
            touch_sites.len() >= 2,
            "REACH-GUARD: the certification step must actually call the period touch; found \
             {} sites",
            touch_sites.len()
        );
        for site in &touch_sites {
            assert!(
                lines[*site].contains("window"),
                "every period-touch call must be handed a WINDOW, never a raw ring; line {} \
                 reads `{}`",
                site + 1,
                lines[*site].trim()
            );
        }
        let window_bindings = engine_code_hits(&lines, certified, "let window");
        assert!(
            !window_bindings.is_empty(),
            "REACH-GUARD: the windows must be bound inside this extent"
        );
        for binding in &window_bindings {
            assert!(
                lines[*binding].contains("ring_live"),
                "every window is sliced from the EVALUABLE ring; line {} reads `{}`",
                binding + 1,
                lines[*binding].trim()
            );
        }
    }
}
