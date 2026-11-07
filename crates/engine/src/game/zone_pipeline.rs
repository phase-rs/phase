//! Unified zone-change pipeline.
//!
//! Layer discipline (PLAN §2): `zones.rs` keeps every guard that must hold
//! unconditionally (CR 111.8 token guard, CR 614.1d ETB block, CR 400.7 cleanup,
//! `GameEvent::ZoneChanged` emission); this module owns the "would"-semantics
//! layer (CR 614.1 / 614.6 replacement consult, CR 616.1 choices, CR 614.1c
//! enters-with seeding) plus the CR 303.4f aura-host choice.

use crate::game::game_object::ObjectIncarnationRefExt;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    AdditionalCostInstancePayment, AttachCardinality, AttachSelection, CastTimingPermission,
    CostPaidObjectSnapshot, DigRestSplitScope, Duration, Effect, EffectKind, ExileConcealment,
    KickerVariant, LibraryPosition, ResolvedAbility, StaticDefinition, TargetFilter, TargetRef,
};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    BatchCompletion, BatchDeliveryStage, EnteringAuraAuthority, ExileLinkKind, GameState,
    LibraryPlacementContinuation, LiminalEntryKind, LogicalZoneChangeGroup, MassLibraryOrderBatch,
    MassLibraryOrderMember, MergedCardComponentRoute, PendingBatchDeliveries,
    PendingBatchZoneChangeCause, PendingBatchZoneMoveRequest, PendingComponentDeliveryParent,
    PendingCounterPostAction, PendingLiminalEntryResume, PendingZoneChangeDelivery,
    PostReplacementDrainOwner, StagedArrangementStatus, StagedBatchDelivery, StagedBatchMember,
    StagedBatchMemberState, StagedDestroyOperation, StagedDestroyOuterApproval,
    StagedLibraryArrangement, StagedMemberOperation, StagedMemberOutcome, WaitingFor,
    ZoneDeliveryExileTracking, ZoneMoveCompletion,
};
use std::collections::HashSet;

use crate::types::format::EntryOwnership;
use crate::types::identifiers::{LogicalZoneChangeGroupId, ObjectId, ObjectIncarnationRef};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{AppliedReplacementKey, ProposedEvent};
use crate::types::zones::{EtbTapState, Zone};

use crate::game::effects::change_zone::shuffle_library;
use crate::game::game_object::{AttachTarget, GameObject};
use crate::types::ability::FaceDownProfile;
use crate::types::game_state::LogicalZoneChangeTerminalOutcome;

/// Why this zone change is happening. Determines pipeline engagement (PLAN §3)
/// and is carried onto `ProposedEvent::ZoneChange.cause` / `ZoneChangeRecord`.
///
/// The non-exempt variants run the full pipeline (replacement consult + CR 616.1
/// ordering); the exempt variants are pipeline-internal and skip the replacement
/// consult. Each exempt variant carries its CR citation so adding one is a
/// reviewable diff (PLAN §3 "exemptions are data, not a second function").
pub enum ZoneChangeCause {
    /// Resolving effect or ability instruction. `source` feeds
    /// `ProposedEvent::ZoneChange.cause`.
    Effect { source: ObjectId },
    /// Cost payment (delve exile, "as an additional cost" discards/exiles).
    Cost { source: ObjectId },
    /// CR 608.2n / CR 608.3: post-resolution default move of the spell object
    /// itself (stack.rs). Full pipeline.
    SpellResolutionDefault,
    /// CR 704: state-based action (sba.rs aura/equipment misattach drops,
    /// planeswalker loyalty, etc.). Full pipeline.
    StateBasedAction,
    /// CR 903.9a / CR 903.9b: owner-elected commander return to the command
    /// zone. Mechanically a return-to-zone move, but a named CR class — full
    /// pipeline, NOT exempt.
    CommanderRuleReturn,
    /// CR 121.1: drawing a card — "A player draws a card by putting the top card
    /// of their library into their hand." Drawing IS a Library → Hand zone
    /// change, so it runs the full pipeline (the inner `Moved` consult fires for
    /// any def that scopes to a Hand-destination move). Carries no source object:
    /// the draw-step draw (CR 504.1) is a turn-based action with no causing
    /// object, and effect-driven draws attribute their `Moved` redirects to the
    /// REPLACEMENT's source (see `track_exiled_by_source` flow in delivery), not
    /// to the draw cause — so sourcelessness is correct. NOT exempt.
    ///
    /// `seed_applied` carries the OUTER `ReplacementEvent::Draw` pass's applied
    /// `ReplacementId` set so the inner `Moved` consult does not re-fire a def
    /// that already fired at draw level (CR 614.5: a replacement gets one
    /// opportunity to affect an event "or any modified events that may replace
    /// that event"). This payload lives on the variant — not on `ZoneMoveRequest`
    /// — because `Draw` is the only producer; every other cause would carry a
    /// dead empty set. Built only by [`ZoneMoveRequest::draw`].
    Draw {
        seed_applied: HashSet<AppliedReplacementKey>,
    },
    // ---- exempt causes: pipeline-internal, replacement consult skipped ----
    /// CR 601.2a: "the player first moves that card ... to the stack" — part of
    /// the casting process, not a discrete replaceable event.
    CastingToStack { source: ObjectId },
    /// CR 103.5: pregame opening draws and mulligan returns.
    PregameProcedure,
    /// CR 800.4a: owner left the game; all objects they own leave the game.
    PlayerLeftGame,
    /// CR 730.3d + CR 903.9b-c: a merged permanent's physical components are
    /// delivered by the same pausable replacement-aware batch as other
    /// simultaneous moves. The special delivery shape preserves the component
    /// event's `from: None` observability without exempting it from replacement
    /// consultation.
    MergedComponentRouting,
    /// Debug/admin tooling (engine_debug.rs). Loud by construction.
    DebugCommand,
}

impl ZoneChangeCause {
    /// CR-exempt causes skip the `replace_event` consult (the "would"-semantics
    /// layer) and go straight to delivery. Each is a game *procedure* or a
    /// non-replaceable rules action, not a discrete event that effects watch:
    ///
    /// - `CastingToStack` (CR 601.2a): part of the casting process; no Moved
    ///   replacement targets stack entry.
    /// - `PregameProcedure` (CR 103.5): pregame draws / mulligan shuffles and
    ///   bottom-of-library returns happen before any effect exists to replace.
    /// - `PlayerLeftGame` (CR 800.4a): "This is not a state-based action"; all
    ///   objects the player owns leave the game as a single rules action.
    /// - `DebugCommand`: operator intent is "force the state" (no replacement
    ///   consult; the CR 306.5b / CR 310.4b characteristic counters are still
    ///   installed on a battlefield arrival).
    ///
    /// The unconditional primitive guards (CR 111.8 token, CR 614.1d ETB block,
    /// CR 400.7 cleanup) still run in `zones.rs` delivery for every cause — the
    /// exemption is only of the replacement consult, never of the rules that
    /// must hold for any move (PLAN §2 / §3).
    // Exhaustive match, no wildcard: adding a `ZoneChangeCause` variant must
    // force an explicit consult/exempt decision here (with its CR citation
    // above), not silently inherit a default.
    fn is_exempt(&self) -> bool {
        match self {
            ZoneChangeCause::Effect { .. }
            | ZoneChangeCause::Cost { .. }
            | ZoneChangeCause::SpellResolutionDefault
            | ZoneChangeCause::StateBasedAction
            | ZoneChangeCause::CommanderRuleReturn
            // CR 121.1: drawing is a replaceable Library → Hand zone change; it
            // MUST consult `Moved` defs (e.g. a future "cards you would draw are
            // put into exile instead" redirect).
            | ZoneChangeCause::Draw { .. } => false,
            ZoneChangeCause::CastingToStack { .. }
            | ZoneChangeCause::PregameProcedure
            | ZoneChangeCause::PlayerLeftGame
            | ZoneChangeCause::DebugCommand => true,
            // CR 730.3d + CR 903.9c: component moves inherit the original
            // event's applied replacements, then consult any component-specific
            // replacement (including CR 903.9b) through the normal pipeline.
            ZoneChangeCause::MergedComponentRouting => false,
        }
    }
}

/// Destination modifiers — the union of what the pipeline copies need to seed
/// onto the proposed `ZoneChange` before the replacement consult.
#[derive(Default)]
pub struct EntryMods {
    /// CR 614.1c effect seed. Reuses the three-state `EtbTapState`
    /// (`Unspecified` / `Tapped` / `Untapped`) rather than a bool, matching the
    /// pipeline carrier `ProposedEvent::ZoneChange.enter_tapped` and preserving
    /// the Unspecified-vs-Untapped distinction at the request boundary.
    pub enter_tapped: EtbTapState,
    /// CR 508.4: A creature put onto the battlefield attacking joins combat
    /// without being declared as an attacker.
    pub enters_attacking: bool,
    /// CR 712.14a. Genuinely two-valued (enters showing back face or not) — no
    /// Unspecified third state to preserve, unlike `enter_tapped`.
    pub enter_transformed: bool,
    /// CR 110.2a controller override ("enters under your control").
    pub controller_override: Option<PlayerId>,
    /// CR 122.1 + CR 614.1c effect-driven enter-with counters.
    pub enter_with_counters: Vec<(CounterType, u32)>,
    /// CR 708.2a + CR 708.3 face-down entry profile.
    pub face_down_profile: Option<FaceDownProfile>,
    /// CR 608.2c: whether this entry is the producer a following demonstrative
    /// anaphor refers back to. `Silent` unless a producer opted in.
    pub chain_referent: crate::types::zones::ChainReferentIntent,
    /// CR 303.4f pre-resolved aura host.
    pub attach_to: Option<AttachTarget>,
    /// CR 608.2c: the player performing the instruction that moves the object;
    /// seeded onto `ProposedEvent::ZoneChange.performed_by` so delivery records
    /// who exiled it (CR 406.6). `None` when no player performs the move. Under
    /// `EntryOwnership::ActorOwns` delivery also reads it as the player who
    /// becomes the card's owner ([`entry_receiver`]).
    pub performed_by: Option<PlayerId>,
}

/// Exile-link context carried through the delivery tail. Replaces the old
/// `track_exiled_by_source: bool` (no-bool rule): duration-bound links and
/// `exiled_by_source` bookkeeping always travel together, so they fold into one
/// struct that also rides in `DeliveryCtx`.
#[derive(Default)]
pub struct ExileLinkSpec {
    /// A host-lifetime duration — `Some(d)` with `d.ends_when_host_leaves_play()`
    /// — installs a return-on-source-leave link; other durations / `None` fall
    /// back to `tracking`.
    pub duration: Option<Duration>,
    /// Resolved controller for a monarch-bounded link. `Some` is captured when
    /// the originating ability resolves; `None` means that duration cannot
    /// create a monarch link.
    pub controller: Option<PlayerId>,
    /// `TrackBySource` records an "exiled with" link; `None` records nothing
    /// unless `duration` requires it.
    pub tracking: ZoneDeliveryExileTracking,
}

/// A request to move a single object through the zone-change pipeline.
///
/// `from` is read from the object's current zone inside `move_object` (every
/// pipeline copy except change_zone already did this).
pub struct ZoneMoveRequest {
    pub object_id: ObjectId,
    pub to: Zone,
    pub cause: ZoneChangeCause,
    pub mods: EntryMods,
    /// Library placement; `None` = zone default. Reuses the existing
    /// `LibraryPosition` enum (`move_to_library_position` is its documented
    /// executor) rather than a parallel index convention.
    pub placement: Option<LibraryPosition>,
    /// Exile-link context (duration-bound returns + exiled-by-source tracking).
    pub exile_links: ExileLinkSpec,
    /// CR 614.5: replacement definitions already applied to the event or
    /// modified event from which this physical-card move was derived.
    pub replacement_applied: HashSet<AppliedReplacementKey>,
    /// CR 406.3: Mark the object face down if this delivery actually settles in
    /// exile. This is deliberately a delivery modifier, not an effect epilogue:
    /// a later batch member may park on CR 616.1 while earlier members must
    /// already be hidden.
    pub face_down_in_exile: ExileConcealment,
}

impl ZoneMoveRequest {
    fn into_pending(self) -> PendingBatchZoneMoveRequest {
        let cause = match self.cause {
            ZoneChangeCause::Effect { source } => PendingBatchZoneChangeCause::Effect { source },
            ZoneChangeCause::Cost { source } => PendingBatchZoneChangeCause::Cost { source },
            ZoneChangeCause::SpellResolutionDefault => {
                PendingBatchZoneChangeCause::SpellResolutionDefault
            }
            ZoneChangeCause::StateBasedAction => PendingBatchZoneChangeCause::StateBasedAction,
            ZoneChangeCause::CommanderRuleReturn => {
                PendingBatchZoneChangeCause::CommanderRuleReturn
            }
            ZoneChangeCause::Draw { seed_applied } => {
                PendingBatchZoneChangeCause::Draw { seed_applied }
            }
            ZoneChangeCause::CastingToStack { source } => {
                PendingBatchZoneChangeCause::CastingToStack { source }
            }
            ZoneChangeCause::PregameProcedure => PendingBatchZoneChangeCause::PregameProcedure,
            ZoneChangeCause::PlayerLeftGame => PendingBatchZoneChangeCause::PlayerLeftGame,
            ZoneChangeCause::MergedComponentRouting => {
                PendingBatchZoneChangeCause::MergedComponentRouting
            }
            ZoneChangeCause::DebugCommand => PendingBatchZoneChangeCause::DebugCommand,
        };
        PendingBatchZoneMoveRequest {
            object_id: self.object_id,
            destination: self.to,
            cause,
            enter_tapped: self.mods.enter_tapped,
            enters_attacking: self.mods.enters_attacking,
            enter_transformed: self.mods.enter_transformed,
            controller_override: self.mods.controller_override,
            enter_with_counters: self.mods.enter_with_counters,
            face_down_profile: self.mods.face_down_profile,
            chain_referent: self.mods.chain_referent,
            attach_to: self.mods.attach_to,
            performed_by: self.mods.performed_by,
            library_placement: self.placement,
            exile_duration: self.exile_links.duration,
            exile_controller: self.exile_links.controller,
            exile_tracking: self.exile_links.tracking,
            replacement_applied: self.replacement_applied,
            face_down_in_exile: self.face_down_in_exile,
        }
    }

    /// Borrowed twin of [`Self::into_pending`] for staged entry, which retains
    /// the live requests while freezing their exact serialized context.
    fn to_pending(&self) -> PendingBatchZoneMoveRequest {
        let cause = match &self.cause {
            ZoneChangeCause::Effect { source } => {
                PendingBatchZoneChangeCause::Effect { source: *source }
            }
            ZoneChangeCause::Cost { source } => {
                PendingBatchZoneChangeCause::Cost { source: *source }
            }
            ZoneChangeCause::SpellResolutionDefault => {
                PendingBatchZoneChangeCause::SpellResolutionDefault
            }
            ZoneChangeCause::StateBasedAction => PendingBatchZoneChangeCause::StateBasedAction,
            ZoneChangeCause::CommanderRuleReturn => {
                PendingBatchZoneChangeCause::CommanderRuleReturn
            }
            ZoneChangeCause::Draw { seed_applied } => PendingBatchZoneChangeCause::Draw {
                seed_applied: seed_applied.clone(),
            },
            ZoneChangeCause::CastingToStack { source } => {
                PendingBatchZoneChangeCause::CastingToStack { source: *source }
            }
            ZoneChangeCause::PregameProcedure => PendingBatchZoneChangeCause::PregameProcedure,
            ZoneChangeCause::PlayerLeftGame => PendingBatchZoneChangeCause::PlayerLeftGame,
            ZoneChangeCause::MergedComponentRouting => {
                PendingBatchZoneChangeCause::MergedComponentRouting
            }
            ZoneChangeCause::DebugCommand => PendingBatchZoneChangeCause::DebugCommand,
        };
        PendingBatchZoneMoveRequest {
            object_id: self.object_id,
            destination: self.to,
            cause,
            enter_tapped: self.mods.enter_tapped,
            enters_attacking: self.mods.enters_attacking,
            enter_transformed: self.mods.enter_transformed,
            controller_override: self.mods.controller_override,
            enter_with_counters: self.mods.enter_with_counters.clone(),
            face_down_profile: self.mods.face_down_profile.clone(),
            chain_referent: self.mods.chain_referent,
            attach_to: self.mods.attach_to,
            performed_by: self.mods.performed_by,
            library_placement: self.placement.clone(),
            exile_duration: self.exile_links.duration.clone(),
            exile_controller: self.exile_links.controller,
            exile_tracking: self.exile_links.tracking,
            replacement_applied: self.replacement_applied.clone(),
            face_down_in_exile: self.face_down_in_exile,
        }
    }

    fn from_pending(pending: PendingBatchZoneMoveRequest) -> Self {
        let cause = match pending.cause {
            PendingBatchZoneChangeCause::Effect { source } => ZoneChangeCause::Effect { source },
            PendingBatchZoneChangeCause::Cost { source } => ZoneChangeCause::Cost { source },
            PendingBatchZoneChangeCause::SpellResolutionDefault => {
                ZoneChangeCause::SpellResolutionDefault
            }
            PendingBatchZoneChangeCause::StateBasedAction => ZoneChangeCause::StateBasedAction,
            PendingBatchZoneChangeCause::CommanderRuleReturn => {
                ZoneChangeCause::CommanderRuleReturn
            }
            PendingBatchZoneChangeCause::Draw { seed_applied } => {
                ZoneChangeCause::Draw { seed_applied }
            }
            PendingBatchZoneChangeCause::CastingToStack { source } => {
                ZoneChangeCause::CastingToStack { source }
            }
            PendingBatchZoneChangeCause::PregameProcedure => ZoneChangeCause::PregameProcedure,
            PendingBatchZoneChangeCause::PlayerLeftGame => ZoneChangeCause::PlayerLeftGame,
            PendingBatchZoneChangeCause::MergedComponentRouting => {
                ZoneChangeCause::MergedComponentRouting
            }
            PendingBatchZoneChangeCause::DebugCommand => ZoneChangeCause::DebugCommand,
        };
        Self {
            object_id: pending.object_id,
            to: pending.destination,
            cause,
            mods: EntryMods {
                enter_tapped: pending.enter_tapped,
                enters_attacking: pending.enters_attacking,
                enter_transformed: pending.enter_transformed,
                controller_override: pending.controller_override,
                enter_with_counters: pending.enter_with_counters,
                face_down_profile: pending.face_down_profile,
                chain_referent: pending.chain_referent,
                attach_to: pending.attach_to,
                performed_by: pending.performed_by,
            },
            placement: pending.library_placement,
            exile_links: ExileLinkSpec {
                duration: pending.exile_duration,
                controller: pending.exile_controller,
                tracking: pending.exile_tracking,
            },
            replacement_applied: pending.replacement_applied,
            face_down_in_exile: pending.face_down_in_exile,
        }
    }

    /// Effect- or ability-driven move with no destination modifiers.
    pub fn effect(object_id: ObjectId, to: Zone, source: ObjectId) -> Self {
        Self {
            object_id,
            to,
            cause: ZoneChangeCause::Effect { source },
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: HashSet::new(),
            face_down_in_exile: ExileConcealment::Public,
        }
    }

    /// Cost-payment move (delve exile, additional-cost discard/exile).
    pub fn cost(object_id: ObjectId, to: Zone, source: ObjectId) -> Self {
        Self {
            object_id,
            to,
            cause: ZoneChangeCause::Cost { source },
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: HashSet::new(),
            face_down_in_exile: ExileConcealment::Public,
        }
    }

    /// CR 608.2n / CR 608.3e: post-resolution default move of the spell object
    /// itself (instant/sorcery → graveyard, fizzled/countered-on-resolution
    /// spell, prevented permanent spell → graveyard). The spell moves itself,
    /// so there is no external source — `move_object` anchors attribution on the
    /// object for the (inert, non-battlefield) entry bookkeeping.
    pub fn spell_resolution_default(object_id: ObjectId, to: Zone) -> Self {
        Self {
            object_id,
            to,
            cause: ZoneChangeCause::SpellResolutionDefault,
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: HashSet::new(),
            face_down_in_exile: ExileConcealment::Public,
        }
    }

    /// CR 704: state-based action zone change with no destination modifiers.
    pub fn state_based_action(object_id: ObjectId, to: Zone) -> Self {
        Self {
            object_id,
            to,
            cause: ZoneChangeCause::StateBasedAction,
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: HashSet::new(),
            face_down_in_exile: ExileConcealment::Public,
        }
    }

    /// CR 121.1 + CR 504.1: drawing a card moves the top card of the library
    /// into the owner's hand. Like [`Self::spell_resolution_default`], this is a
    /// sourceless move that STILL consults the pipeline (Draw is non-exempt) —
    /// the draw-step draw (CR 504.1) is a turn-based action with no causing
    /// object, and an effect-driven draw's `Moved` redirect is attributed to the
    /// REPLACEMENT's source, not the draw cause. `seed_applied` carries the
    /// outer `ReplacementEvent::Draw` pass's applied set so the inner `Moved`
    /// consult does not double-apply a def that already fired at draw level
    /// (CR 614.5, PLAN Risk #5). `drawer` is the player whose hand receives the
    /// card (CR 121.1).
    pub fn draw(
        object_id: ObjectId,
        drawer: PlayerId,
        seed_applied: HashSet<AppliedReplacementKey>,
    ) -> Self {
        Self {
            object_id,
            to: Zone::Hand,
            cause: ZoneChangeCause::Draw {
                seed_applied: seed_applied.clone(),
            },
            mods: EntryMods {
                performed_by: Some(drawer),
                ..EntryMods::default()
            },
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: seed_applied,
            face_down_in_exile: ExileConcealment::Public,
        }
    }

    /// CR 601.2a: casting moves the card from where it is to the stack — part
    /// of the casting process, exempt from the replacement consult. `caster` is
    /// the player who performs the move.
    pub fn casting_to_stack(object_id: ObjectId, source: ObjectId, caster: PlayerId) -> Self {
        Self {
            object_id,
            to: Zone::Stack,
            cause: ZoneChangeCause::CastingToStack { source },
            mods: EntryMods {
                performed_by: Some(caster),
                ..EntryMods::default()
            },
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: HashSet::new(),
            face_down_in_exile: ExileConcealment::Public,
        }
    }

    /// CR 103.5: pregame procedure (opening-draw / mulligan shuffle, bottom-of-
    /// library returns, opening-hand actions) — exempt from the replacement
    /// consult. `placement` is honored so mulligan bottoming reuses the
    /// library-placement arm.
    pub fn pregame(object_id: ObjectId, to: Zone) -> Self {
        Self {
            object_id,
            to,
            cause: ZoneChangeCause::PregameProcedure,
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: HashSet::new(),
            face_down_in_exile: ExileConcealment::Public,
        }
    }

    /// CR 800.4a: a player left the game; objects they own leave the game (are
    /// exiled). "This is not a state-based action" — exempt from the consult.
    pub fn player_left_game(object_id: ObjectId, to: Zone) -> Self {
        Self {
            object_id,
            to,
            cause: ZoneChangeCause::PlayerLeftGame,
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: HashSet::new(),
            face_down_in_exile: ExileConcealment::Public,
        }
    }

    /// CR 730.3d + CR 903.9b-c: Route one absorbed component through the
    /// replacement pipeline. The delivery recognizes its split marker and
    /// preserves `ZoneChanged { from: None }`, so this is not an independent
    /// battlefield exit even though its would-move event is replaceable.
    pub(crate) fn merged_component(object_id: ObjectId, to: Zone) -> Self {
        Self {
            object_id,
            to,
            cause: ZoneChangeCause::MergedComponentRouting,
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: HashSet::new(),
            face_down_in_exile: ExileConcealment::Public,
        }
    }

    /// Debug/admin tooling forcing a zone change — exempt from the consult.
    pub fn debug(object_id: ObjectId, to: Zone) -> Self {
        Self {
            object_id,
            to,
            cause: ZoneChangeCause::DebugCommand,
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: HashSet::new(),
            face_down_in_exile: ExileConcealment::Public,
        }
    }

    /// CR 614.1: enters tapped.
    pub fn tapped(mut self) -> Self {
        self.mods.enter_tapped = EtbTapState::Tapped;
        self
    }

    /// CR 712.14a: enters showing its back face.
    pub fn transformed(mut self) -> Self {
        self.mods.enter_transformed = true;
        self
    }

    /// CR 110.2a: enters under the given player's control.
    pub fn under_control_of(mut self, player: PlayerId) -> Self {
        self.mods.controller_override = Some(player);
        self
    }

    /// CR 122.1 + CR 614.1c: enters with the given counters.
    pub fn with_counters(mut self, counters: Vec<(CounterType, u32)>) -> Self {
        self.mods.enter_with_counters = counters;
        self
    }

    /// CR 303.4f: pre-resolved aura host.
    pub fn attached_to(mut self, target: AttachTarget) -> Self {
        self.mods.attach_to = Some(target);
        self
    }

    /// CR 708.2a + CR 708.3: enters the battlefield face down showing the given
    /// profile (morph / manifest vanilla 2/2). The delivery tail snapshots the
    /// real face into `back_face` and applies the profile before the entry, so
    /// callers no longer override characteristics manually after the move.
    pub fn face_down(mut self, profile: FaceDownProfile) -> Self {
        self.mods.face_down_profile = Some(profile);
        self
    }

    /// CR 608.2c: mark this entry as the producer a following demonstrative
    /// anaphor binds to. Opt-in, so an unmarked delivery never touches the
    /// game-lifetime referent slot.
    pub fn publishing_chain_referent(mut self) -> Self {
        self.mods.chain_referent = crate::types::zones::ChainReferentIntent::Publishes;
        self
    }

    /// Library placement override (`LibraryPosition::Top` / `Bottom` /
    /// `NthFromTop`). Only meaningful when `to == Zone::Library`.
    pub fn at_library_position(mut self, position: LibraryPosition) -> Self {
        self.placement = Some(position);
        self
    }

    /// CR 406.3: Conceal this card immediately if the replacement-aware move
    /// delivers it to Exile.
    pub fn face_down_in_exile(mut self) -> Self {
        self.face_down_in_exile = ExileConcealment::FaceDown;
        self
    }

    /// CR 614.5: seed a child/modified move with the replacements already
    /// applied to its originating event.
    pub fn with_replacement_applied(mut self, applied: HashSet<AppliedReplacementKey>) -> Self {
        self.replacement_applied = applied;
        self
    }

    /// CR 608.2c + CR 406.6: name the player performing this move, so a
    /// delivery that settles the object in exile records who exiled it.
    pub fn performed_by(mut self, player: PlayerId) -> Self {
        self.mods.performed_by = Some(player);
        self
    }

    /// CR 121.1 + CR 608.2c: name the player whose hand receives the card, only
    /// when the requested destination is Hand, so a requested Exile never gains a
    /// performer; a Hand request that a `Moved` replacement redirects to Exile
    /// keeps it.
    pub fn hand_taker(self, player: PlayerId) -> Self {
        if self.to == Zone::Hand {
            self.performed_by(player)
        } else {
            self
        }
    }

    /// Record an "exiled with this source" link (CR 614 exile-tracking class).
    pub fn track_exiled_by_source(mut self) -> Self {
        self.exile_links.tracking = ZoneDeliveryExileTracking::TrackBySource;
        self
    }

    /// Install a duration-bound exile link (e.g. `UntilHostLeavesPlay`).
    pub fn exile_for_duration(mut self, duration: Duration) -> Self {
        self.exile_links.duration = Some(duration);
        self
    }

    /// The source object this move is attributed to, if any. Exempt causes that
    /// carry no source return `None`.
    fn source(&self) -> Option<ObjectId> {
        // Exhaustive, no wildcard: a new `ZoneChangeCause` variant must make an
        // explicit source decision (mirrors `is_exempt`'s mandate above) rather
        // than silently inherit `None`.
        match &self.cause {
            ZoneChangeCause::Effect { source }
            | ZoneChangeCause::Cost { source }
            | ZoneChangeCause::CastingToStack { source } => Some(*source),
            // CR 504.1: a draw-step draw is a turn-based action with no causing
            // object; effect-driven draws attribute redirects to the replacement
            // source, not the move cause — so `Draw` is sourceless.
            ZoneChangeCause::Draw { .. }
            | ZoneChangeCause::SpellResolutionDefault
            | ZoneChangeCause::StateBasedAction
            | ZoneChangeCause::CommanderRuleReturn
            | ZoneChangeCause::PregameProcedure
            | ZoneChangeCause::PlayerLeftGame
            | ZoneChangeCause::MergedComponentRouting
            | ZoneChangeCause::DebugCommand => None,
        }
    }
}

/// Proof that a `ZoneChange` event has cleared the replacement consult and is
/// safe to deliver. Mintable in exactly three places, all in this module:
/// (a) after `replace_event` returns `Execute(ZoneChange{..})` inside
/// `move_object`; (b) directly from an exempt-cause request; (c) the
/// `approve_post_replacement` path for outer-wrapper-lowered events.
///
/// MUST NOT derive `Serialize`, `Deserialize`, `Clone`, or `Default` — any of
/// these would mint a token outside the pipeline (deserialization, cloning a
/// stashed token, `Default::default()`) and silently reopen the loophole. A CI
/// grep for derives adjacent to this type backs the review rule.
pub struct ApprovedZoneChange {
    event: ProposedEvent,
    _seal: (),
}

impl ApprovedZoneChange {
    /// The third mint path (PLAN §6.2): seal an event that has already completed
    /// a full replacement pass OUTSIDE this module — the outer Destroy /
    /// Sacrifice / Discard pass lowers into a `ZoneChange` carrying its
    /// `applied: HashSet<AppliedReplacementKey>`. Legal ONLY on `ZoneChange` payloads;
    /// returns `Err(event)` for anything else so the caller can fall back.
    /// Re-proposing such an event through `move_object` would discard `applied`
    /// and double-apply Moved definitions / redo CR 616.1 ordering.
    pub(crate) fn approve_post_replacement(
        event: ProposedEvent,
    ) -> Result<ApprovedZoneChange, Box<ProposedEvent>> {
        if matches!(event, ProposedEvent::ZoneChange { .. }) {
            Ok(ApprovedZoneChange { event, _seal: () })
        } else {
            Err(Box::new(event))
        }
    }

    /// Mint internally once `move_object`'s ZoneChange arm has a post-replacement
    /// (or exempt) event ready to deliver.
    fn seal(event: ProposedEvent) -> ApprovedZoneChange {
        ApprovedZoneChange { event, _seal: () }
    }
}

/// Context threaded into `deliver`: the attributed source, exile-link spec,
/// and the continuation-drain owner. Consumed by the bucket-A
/// `deliver(approved, ctx)` migrations.
///
/// PLAN Open Question #3 (RESOLVED): play/cast provenance is NOT a ctx knob.
/// `played_from_zone` (land-play provenance, CR 305.1) is established by the
/// land-play action and cleared only on battlefield EXIT
/// (`reset_for_battlefield_exit`) — nothing clears it during a battlefield
/// ENTRY, so the former `ctx.played_from_zone` re-stamp preserved a value that
/// was never destroyed (verified against `reset_for_battlefield_entry` and the
/// field's writer set; the capture/restore was a defensive no-op since PR
/// #1119 introduced it). The cast-link family that IS cleared on entry
/// (CR 400.7d: kicker / Gift recipient / additional-cost / convoke / cast-timing
/// memory) is preserved structurally by the delivery itself — see
/// [`CastLinkSnapshot`].
pub(crate) struct DeliveryCtx {
    pub source_id: Option<ObjectId>,
    pub exile_links: ExileLinkSpec,
    /// CR 614.12a: who drains `post_replacement_continuation` after this
    /// delivery (see [`PostReplacementDrainOwner`]).
    pub drain: PostReplacementDrainOwner,
    /// Legacy caller mirror, retained for source compatibility. Terminal delivery
    /// reads only `ApprovedZoneChange.event.library_placement`, including null.
    pub library_placement: Option<LibraryPosition>,
}

/// CR 400.7d + CR 608.3: the cast-link family — information about the spell
/// that became the permanent, which an ability of that permanent may
/// reference ("if it was kicked", convoke history, cast-timing permission).
/// `reset_for_battlefield_entry` (CR 400.7) clears these on entry; the
/// delivery snapshots them from the pre-move STACK object and restores them
/// right after the move, for `Stack → Battlefield` deliveries only.
/// Establishment is exclusive to the cast pathway (`finalize_cast_to_stack`),
/// so the gate makes effect-driven puts (Reanimate class) structurally unable
/// to resurrect stale cast provenance.
struct CastLinkSnapshot {
    cast_from_zone: Option<Zone>,
    cast_controller: Option<PlayerId>,
    cast_timing_permission: Option<CastTimingPermission>,
    kickers_paid: Vec<KickerVariant>,
    /// CR 702.174a: Opponent promised the Gift when this spell was cast.
    gift_recipient: Option<PlayerId>,
    additional_cost_payment_count: u32,
    additional_cost_payments: Vec<AdditionalCostInstancePayment>,
    convoked_creatures: Vec<ObjectId>,
    // CR 400.7d: the object paid as a cost to cast the spell (e.g. the
    // emerge-sacrificed creature) is part of the cast-link family cleared on
    // entry; snapshot and restore it like the other members.
    cast_cost_paid_object: Option<CostPaidObjectSnapshot>,
}

/// Result of a single zone-move attempt through the replacement pipeline.
pub(crate) enum ZoneMoveResult {
    /// Object was moved (or prevented). Continue processing.
    Done,
    /// A replacement effect needs a player choice before continuing.
    NeedsChoice(PlayerId),
    /// An Aura entered via a non-spell effect and needs an enchant-host choice.
    NeedsAuraAttachmentChoice,
}

/// Exact completion information used by logical zone-change owners. The public
/// result surface deliberately remains the established three-way enum; callers
/// that do not own a logical group do not need terminal provenance.
pub(crate) enum ZoneMoveTerminalResult {
    Completed(ZoneMoveCompletion),
    NeedsChoice(PlayerId),
    NeedsAuraAttachmentChoice,
}

impl ZoneMoveTerminalResult {
    pub(crate) fn into_zone_move_result(self) -> ZoneMoveResult {
        match self {
            Self::Completed(_) => ZoneMoveResult::Done,
            Self::NeedsChoice(player) => ZoneMoveResult::NeedsChoice(player),
            Self::NeedsAuraAttachmentChoice => ZoneMoveResult::NeedsAuraAttachmentChoice,
        }
    }
}

/// Derive the only valid completed-delivery classification from an explicit
/// slice and the pre-delivery incarnation. A redirect is still `Moved`; an
/// accepted delivery with no exact `ZoneChanged` record is `Remained`.
pub(crate) fn zone_move_completion_from_delivery(
    member: ObjectIncarnationRef,
    delivery_events: &[GameEvent],
) -> ZoneMoveCompletion {
    PendingZoneChangeDelivery::completion_from_delivery_events(member, delivery_events)
}

pub(crate) enum ZoneDeliveryResult {
    Done,
    NeedsChoice(PlayerId),
}

/// THE single zone-change entry point. Reads `from` from the object's current
/// zone, unpacks `EntryMods` / `ExileLinkSpec`, and runs the proposal through
/// the replacement pipeline + delivery tail.
///
/// `pub(crate)` while `ZoneMoveResult` is `pub(crate)`: every caller lives in the
/// engine crate. (PLAN §1.3 writes `pub fn`; widening to `pub` only matters once
/// a cross-crate consumer exists, which it does not yet.)
pub(crate) fn move_object(
    state: &mut GameState,
    req: ZoneMoveRequest,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    move_object_with_terminal(state, req, events).into_zone_move_result()
}

#[cfg(feature = "test-support")]
pub fn move_object_for_test(
    state: &mut GameState,
    req: ZoneMoveRequest,
    events: &mut Vec<GameEvent>,
) -> bool {
    matches!(
        move_object(state, req, events),
        ZoneMoveResult::NeedsChoice(_)
    )
}

/// Whether `zones::move_to_zone` will reject moving `object` to `to` as one of
/// its two object-level no-ops (below); its other rejections — CR 304.4 /
/// CR 307.4 instants and sorceries, CR 712.14a a card with no back face put
/// onto the battlefield transformed, CR 614.17 "can't enter" statics — are
/// decided at delivery. Its callers (below) run it before they build a
/// proposal, so no replacement is consulted, consumed (`last_effect_count`,
/// CR 616.1 choices) or prompted for a move that never happens.
/// CR 111.8: a token that has left the battlefield can't move to another zone
/// or come back onto the battlefield; it remains in its current zone and ceases
/// to exist at the next SBA check (CR 704.5d). An exact CR 601.2a pending spell
/// plus its announcement placeholder makes the retained-origin representation
/// stack-resident until its delivery.
/// CR 400.6 + CR 603.6d + CR 614.12: a Battlefield -> Battlefield move is not a
/// zone change, so no event puts the permanent onto the battlefield for an
/// "enters with" / "as enters" replacement — including the CR 306.5b /
/// CR 310.4b intrinsic counters — to modify. CR 603.2g + CR 603.6a: and no
/// enters-the-battlefield trigger event occurs.
/// Callers: `move_object_with_terminal` (THE zone-change entry point) and
/// `engine_debug::enter_battlefield_with_etb` (the sandbox consulting entry).
pub(crate) fn zone_move_is_primitive_noop(
    state: &GameState,
    object: &GameObject,
    to: Zone,
) -> bool {
    // CR 111.8: a token outside the battlefield (and not stack-resident) stays put.
    zones::token_is_outside_battlefield_and_stack(state, object)
        // CR 400.6 + CR 603.6d: Battlefield -> Battlefield is no entry event.
        || (object.zone == Zone::Battlefield && to == Zone::Battlefield)
}

pub(crate) fn move_object_with_terminal(
    state: &mut GameState,
    req: ZoneMoveRequest,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveTerminalResult {
    move_batch_member_with_terminal(state, req, None, events)
}

pub(crate) struct BatchMemberDeliveryOwner {
    pub group: LogicalZoneChangeGroupId,
    pub member: ObjectIncarnationRef,
    pub expected_event: Option<ProposedEvent>,
    /// Retained authority installed after the Aura-host consult.
    pub population_devour_snapshot: Option<HashSet<ObjectId>>,
    /// Retained authority installed after the Aura-host consult.
    pub approved_post_replacement: Option<crate::types::game_state::PostReplacementDrainStack>,
    pub inherited_arrangements: Vec<StagedLibraryArrangement>,
}

fn move_batch_member_with_terminal(
    state: &mut GameState,
    req: ZoneMoveRequest,
    owner: Option<BatchMemberDeliveryOwner>,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveTerminalResult {
    let Some(object) = state.objects.get(&req.object_id) else {
        // The object no longer exists (already moved / ceased to exist); nothing
        // to do. The unconditional guards in `zones.rs` would no-op anyway.
        return ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained);
    };
    let from_zone = object.zone;
    let member = ObjectIncarnationRef::from_object(object);

    // CR 111.8 + CR 603.2g (PLAN §8 Risk #11): Hoist the cheap object-level guards that
    // `zones::move_to_zone` enforces unconditionally to BEFORE the replacement
    // consult. The pipeline now runs `replace_event` ahead of the primitive's
    // delivery-time guards, so a replacement could otherwise be "consumed"
    // (`last_effect_count`, CR 616.1 choices) on a move the primitive then
    // rejects as a no-op. These two are pure object-level reads with no game
    // effect, so testing them up front cannot change observable behavior — it
    // only avoids spending a one-shot replacement on a move that never happens.
    if zone_move_is_primitive_noop(state, object, req.to) {
        return ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained);
    }

    // Library-placement arm (W3). A `Some(placement)` request lands the object at
    // a specific library index instead of shuffling it in: a placement instruction
    // is not a shuffle instruction (CR 701.24a defines shuffling as randomizing the
    // library so no player knows its order). The tail's auto-shuffle convention
    // applies only to placement-less library deliveries. (CR 701.24g governs the
    // different case where an effect instructs BOTH a shuffle and a placement
    // simultaneously — the shuffle then happens with the object pinned at the
    // requested position; that case is not this gate.)
    //
    // For EXEMPT causes (pregame opening-hand bottoming, debug top/Nth) the
    // consult is skipped — exactly as the raw `move_to_library_at_index` callers
    // did before migration — and the object is placed directly. The unconditional
    // CR 111.8 token / CR 400.7 cleanup guards live inside the primitive itself.
    //
    // For NON-EXEMPT causes the consult RUNS (W3 completion): a board-wide `Moved`
    // "would be put into a library → ... instead" redirect (none exist in the
    // current pool — behavior-preserving today; re-verify with
    //   rg -o 'destination_zone\(Zone::\w+\)' crates/engine/src | sort | uniq -c
    // ) is honored. The delivered destination decides placement: if the redirect
    // sent the object elsewhere, `deliver_replaced_zone_change` ignores the
    // placement; if it still lands in the library, the object is placed at the
    // requested index and the tail's auto-shuffle is suppressed (CR 701.24a: a
    // placement is not a shuffle).
    //
    // Phase E tranche 2: six production raw library-position callers still bypass
    // this consult by calling `zones::move_to_library_position` /
    // `move_to_library_at_index` directly instead of routing through
    // `move_object`'s placement arm. They are:
    //   - engine_resolution_choices.rs: clash return (~2989)
    //   - engine_resolution_choices.rs: EffectZoneChoice bottom placement (~7260)
    //   - engine_resolution_choices.rs: EffectZoneChoice top/Nth placement (~7272)
    //   - engine_resolution_choices.rs: EffectZoneChoice mixed-source reorder (~7333)
    //   - zone_pipeline.rs: exempt library-placement delivery (~821)
    //   - zone_pipeline.rs: replacement delivery placement (~2353)
    // Migrating each onto this arm is a production no-op today (the only
    // `Moved` definition targeting the library is test-only) but pins the
    // redirect consult for the future. Re-verify the census before lifting:
    //   rg -o 'destination_zone\(Zone::\w+\)' crates/engine/src | sort | uniq -c
    if let Some(position) = req.placement.clone() {
        if req.to == Zone::Library {
            if req.cause.is_exempt() {
                let index = match position {
                    LibraryPosition::Top => Some(0),
                    LibraryPosition::Bottom => None,
                    // CR: `NthFromTop { n }` is 1-based ("second from the top" =>
                    // n=2, index 1); `move_to_library_at_index` is 0-based.
                    LibraryPosition::NthFromTop { n } => Some(n.saturating_sub(1) as usize),
                    // CR 401.7: "beneath the top N cards" is only produced by the
                    // `PutAtLibraryPosition` resolver, which moves directly and never
                    // routes through this rebuilt-tail path. Handled for exhaustiveness:
                    // a literal depth is honored (0-based index), a runtime-resolved
                    // depth cannot be evaluated without the originating ability here.
                    LibraryPosition::BeneathTop { depth } => match depth {
                        crate::types::ability::QuantityExpr::Fixed { value } => {
                            Some(value.max(0) as usize)
                        }
                        _ => None,
                    },
                    // Digital-only Alchemy: `RandomWithinTop` only flows from the
                    // Conjure resolver (`conjure.rs`), which places the card
                    // directly and never routes through this rebuilt-tail path.
                    // Exhaustiveness arm: default placement.
                    LibraryPosition::RandomWithinTop { .. } => None,
                };
                let delivery_start = events.len();
                zones::move_to_library_at_index(state, req.object_id, index, events);
                return ZoneMoveTerminalResult::Completed(zone_move_completion_from_delivery(
                    member,
                    &events[delivery_start..],
                ));
            }
            let source_id = req.source();
            let mut proposed =
                ProposedEvent::zone_change(req.object_id, from_zone, Zone::Library, source_id);
            if let ProposedEvent::ZoneChange {
                applied,
                face_down_in_exile: conceal_in_exile,
                library_placement,
                chain_referent,
                performed_by,
                ..
            } = &mut proposed
            {
                *library_placement = Some(position.clone());
                *chain_referent = req.mods.chain_referent;
                *performed_by = req.mods.performed_by;
                *applied = req.replacement_applied.clone();
                *conceal_in_exile = req.face_down_in_exile;
            }
            return match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    let delivery_start = events.len();
                    match deliver_replaced_batch_member(
                        state,
                        event,
                        source_id,
                        req.exile_links.duration.as_ref(),
                        req.exile_links.controller,
                        matches!(
                            req.exile_links.tracking,
                            ZoneDeliveryExileTracking::TrackBySource
                        ),
                        PostReplacementDrainOwner::DeliveryTail,
                        Some(position),
                        None,
                        owner,
                        events,
                    ) {
                        ZoneDeliveryResult::Done => ZoneMoveTerminalResult::Completed(
                            zone_move_completion_from_delivery(member, &events[delivery_start..]),
                        ),
                        ZoneDeliveryResult::NeedsChoice(player) => {
                            ZoneMoveTerminalResult::NeedsChoice(player)
                        }
                    }
                }
                ReplacementResult::Prevented => {
                    ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Prevented)
                }
                ReplacementResult::NeedsChoice(player) => {
                    // CR 616.1: park at the single unparked origin (mirrors
                    // `execute_zone_move`'s NeedsChoice arm) so the prompt surfaces.
                    state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                    // Retain the legacy wire mirror. The proposed event already
                    // owns the position; resume never restores from this mirror.
                    if let Some(pending) = state.pending_replacement.as_mut() {
                        pending.library_placement = Some(position);
                    }
                    ZoneMoveTerminalResult::NeedsChoice(player)
                }
            };
        }
    }

    let source_id = req.source();
    let exile_links = req.exile_links;
    let track_exiled_by_source = matches!(
        exile_links.tracking,
        ZoneDeliveryExileTracking::TrackBySource
    );

    // CR 121.1 + CR 614.5 (PLAN Risk #5): a draw (Library → Hand) consults the
    // pipeline so a `Moved` def scoped to a Hand-destination move can redirect
    // the drawn card. Drawing never enters the battlefield, so it has none of
    // `execute_zone_move`'s battlefield-entry machinery (ETB counters, aura
    // host, cast-link snapshot, devour) — run the bare consult + delivery here,
    // seeding the proposed event's `applied` set from the OUTER
    // `ReplacementEvent::Draw` pass (the `Draw` variant's `seed_applied`). The
    // dedup guard: a def already in `applied` is skipped at
    // `find_applicable_replacements`' `already_applied(&rid)` gate, so it cannot
    // fire at both the Draw level and this Moved level. The seed lives on the
    // `Draw` cause variant — no other cause produces one.
    if let ZoneChangeCause::Draw { seed_applied } = req.cause {
        let mut proposed = ProposedEvent::zone_change(req.object_id, from_zone, req.to, source_id);
        if let ProposedEvent::ZoneChange {
            applied,
            face_down_in_exile: conceal_in_exile,
            chain_referent,
            performed_by,
            ..
        } = &mut proposed
        {
            *chain_referent = req.mods.chain_referent;
            *performed_by = req.mods.performed_by;
            *applied = req.replacement_applied;
            applied.extend(seed_applied);
            *conceal_in_exile = req.face_down_in_exile;
        }
        return match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                let delivery_start = events.len();
                match deliver_replaced_batch_member(
                    state,
                    event,
                    source_id,
                    exile_links.duration.as_ref(),
                    exile_links.controller,
                    track_exiled_by_source,
                    PostReplacementDrainOwner::DeliveryTail,
                    None,
                    None,
                    owner,
                    events,
                ) {
                    ZoneDeliveryResult::Done => ZoneMoveTerminalResult::Completed(
                        zone_move_completion_from_delivery(member, &events[delivery_start..]),
                    ),
                    ZoneDeliveryResult::NeedsChoice(player) => {
                        ZoneMoveTerminalResult::NeedsChoice(player)
                    }
                }
            }
            ReplacementResult::Prevented => {
                ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Prevented)
            }
            ReplacementResult::NeedsChoice(player) => {
                // CR 616.1: park the surfaced ordering prompt (mirrors the
                // placement / `execute_zone_move` NeedsChoice arms). No
                // production `Moved` def targets a Hand destination today (audit:
                // every destination-unconstrained `Moved` def is `valid_card:
                // SelfRef`-bound to a battlefield host, and the only
                // `valid_card: None` class is destination-gated to Graveyard), so
                // this is unreachable for the current pool — parked for
                // correctness if a future to-Hand redirect surfaces a choice.
                state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                ZoneMoveTerminalResult::NeedsChoice(player)
            }
        };
    }

    // PLAN §3: exempt causes skip the `replace_event` consult and go straight to
    // delivery. The proposed event is sealed directly (no matcher pass) and runs
    // the same delivery tail as a post-replacement event, so the unconditional
    // primitive guards (CR 111.8 / 614.1d / 400.7) still apply. Exempt callers
    // carry default `EntryMods` today; seed any they DO carry so the contract is
    // uniform with the consulting path. The CR 614.1c replacement consult —
    // including the intrinsic enters-with seeding on the proposal — is not
    // applied; the non-debug exempt causes never reach the battlefield, and
    // `DebugCommand` installs only the CR 306.5b / CR 310.4b characteristic
    // counters below.
    if req.cause.is_exempt() {
        // DebugCommand skips the replacement consult and the delivery tail:
        // operator intent is "force the state" for scenario setup, so the
        // delivery tail's battlefield arms must not fire either — CR 614.1c
        // "enters with an additional counter" statics (Kalain class) must not
        // mint counters onto a debug-staged creature, `pending_etb_counters`
        // from delayed triggers must not be consumed, and the CR 614.12a
        // devour snapshot must not be captured. A battlefield arrival still
        // receives its CR 306.5b / CR 310.4b loyalty / defense counters
        // (`install_intrinsic_characteristic_counters`): CR 306.5c / CR 310.4c
        // make them the permanent's loyalty / defense, so omitting them leaves
        // a planeswalker whose loyalty field contradicts its counters. Route
        // through the no-tail primitive, which keeps every unconditional guard
        // (CR 111.8 token, CR 614.1d ETB block, CR 400.7 cleanup, ZoneChanged
        // emission) because those live in `zones::move_to_zone` itself. This
        // also makes DebugCommand non-pausing by construction: no
        // `apply_etb_counters` call means no counter-replacement pause can
        // park a prompt mid-debug-action, so debug callers may discard the
        // (always-`Done`) result. The other exempt causes keep the tail: it is
        // inert for their destinations (pregame exile/hand have no tail arms,
        // pregame library goes through the placement arm, elimination's
        // battlefield departure wants the `mark_layers_full`).
        if matches!(req.cause, ZoneChangeCause::DebugCommand) {
            let delivery_start = events.len();
            zones::move_to_zone(state, req.object_id, req.to, events);
            if req.face_down_in_exile.is_face_down() && req.to == Zone::Exile {
                zones::mark_face_down_in_exile(state, &mut events[delivery_start..], req.object_id);
            }
            // pod-lab loop-3 Q5: debug-staged board setup (GameScenario, the
            // shared seam nearly every engine integration test builds boards
            // through) stays maximally conservative and out of scope for the
            // move_to_zone incremental-flush carve-out below — there is zero
            // gameplay perf benefit to incrementalizing test/debug setup, and
            // the blast radius of a subtle divergence here is the whole test
            // suite. Unconditional, regardless of what move_to_zone's own
            // (now axis-gated) internal decision would otherwise have been.
            let completion = zone_move_completion_from_delivery(member, &events[delivery_start..]);
            // CR 306.5b + CR 306.5c / CR 310.4b + CR 310.4c: a planeswalker or
            // battle that actually arrived on the battlefield gets its printed
            // loyalty / defense counters. An arrival `move_to_zone` refuses (a
            // CR 614.17d "can't enter the battlefield" static, or a CR 304.4 /
            // CR 307.4 instant or sorcery) is not `Moved` and gets nothing; the
            // CR 111.8 and same-zone no-ops already returned above. The completion
            // is computed first, so the added `CounterAdded` events cannot change it.
            if req.to == Zone::Battlefield && completion == ZoneMoveCompletion::Moved {
                install_intrinsic_characteristic_counters(state, req.object_id, events);
            }
            crate::game::layers::mark_layers_full(state);
            return ZoneMoveTerminalResult::Completed(completion);
        }
        let mut proposed = ProposedEvent::zone_change(req.object_id, from_zone, req.to, source_id);
        if let ProposedEvent::ZoneChange {
            enter_transformed,
            enter_tapped,
            enters_attacking,
            controller_override,
            enter_with_counters,
            face_down_profile,
            face_down_in_exile: conceal_in_exile,
            chain_referent,
            performed_by,
            applied,
            ..
        } = &mut proposed
        {
            *performed_by = req.mods.performed_by;
            *enter_transformed = req.mods.enter_transformed;
            if !req.mods.enter_tapped.is_unspecified() {
                *enter_tapped = req.mods.enter_tapped;
            }
            *enters_attacking = req.mods.enters_attacking;
            *controller_override = req.mods.controller_override;
            enter_with_counters.extend(req.mods.enter_with_counters.iter().cloned());
            *face_down_profile = req.mods.face_down_profile.clone().map(Box::new);
            *conceal_in_exile = req.face_down_in_exile;
            *chain_referent = req.mods.chain_referent;
            *applied = req.replacement_applied;
        }
        let approved = ApprovedZoneChange::seal(proposed);
        let delivery_start = events.len();
        return match deliver_batch_member(
            state,
            approved,
            DeliveryCtx {
                source_id,
                exile_links,
                drain: PostReplacementDrainOwner::DeliveryTail,
                // CR 701.24a: exempt LIBRARY placements were already delivered and
                // returned by the placement arm above; any exempt cause reaching
                // this generic delivery has no library placement to honor.
                library_placement: None,
            },
            owner,
            events,
        ) {
            ZoneDeliveryResult::Done => ZoneMoveTerminalResult::Completed(
                zone_move_completion_from_delivery(member, &events[delivery_start..]),
            ),
            ZoneDeliveryResult::NeedsChoice(player) => ZoneMoveTerminalResult::NeedsChoice(player),
        };
    }

    execute_zone_move_with_applied_terminal(
        state,
        req.object_id,
        from_zone,
        req.to,
        // `execute_zone_move` requires a concrete source id. Exempt causes that
        // carry none use the object itself as the attribution anchor, matching
        // the pre-pipeline raw-move behavior (no source recorded for ETB).
        source_id.unwrap_or(req.object_id),
        exile_links.duration.as_ref(),
        req.mods.enter_transformed,
        req.mods.enter_tapped,
        req.mods.enters_attacking,
        req.mods.controller_override,
        &req.mods.enter_with_counters,
        req.mods.face_down_profile.as_ref(),
        req.face_down_in_exile,
        req.mods.chain_referent,
        track_exiled_by_source,
        None,
        None,
        exile_links.controller,
        req.mods.performed_by,
        req.replacement_applied,
        owner,
        events,
    )
}

/// CR 306.5b + CR 310.4b: a planeswalker / battle has the intrinsic "enters
/// with" loyalty / defense counters; CR 306.5c + CR 310.4c: on the battlefield
/// its loyalty / defense IS that counter count, so a forced placement installs
/// the printed number (CR 122.6 + CR 122.6a: the controller puts them).
/// Installed as an accepted addition — no CR 614 consult, so the DebugCommand
/// arm never parks a CR 616.1 choice. Face counters only: CR 306.5b / CR 310.4b
/// are themselves replacement effects (CR 614.1c), installed here only because
/// CR 306.5c / CR 310.4c make their counter count the permanent's loyalty /
/// defense; the other intrinsic enters-with replacements — Ravenous
/// (CR 702.156a) and Saga lore (CR 714.3a) — set no characteristic and stay
/// skipped with the rest of the CR 614 consult.
fn install_intrinsic_characteristic_counters(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    let Some(obj) = state.objects.get(&object_id) else {
        return;
    };
    let controller = obj.controller;
    let counters = crate::game::printed_cards::intrinsic_face_entry_counters(
        obj.printed_loyalty,
        obj.loyalty,
        None,
        obj.defense,
    );
    for (counter_type, count) in counters {
        crate::game::effects::counters::apply_counter_addition(
            state,
            controller,
            object_id,
            counter_type,
            count,
            events,
        );
    }
}

/// Result of a batch zone-move (`move_objects_simultaneously`).
pub(crate) enum BatchMoveResult {
    /// Every requested object and any inline completion tail were delivered.
    Done,
    /// A per-object `Moved` replacement surfaced a CR 616.1 choice while
    /// delivering the batch or an inline completion tail. `state.waiting_for`
    /// is already parked (with the choosing player) and the undelivered tail is
    /// stashed in the active `BatchDelivery` frame, so the caller only needs to
    /// know that it paused — the resume path (`drain_pending_batch_deliveries`)
    /// finishes the batch.
    NeedsChoice,
}

/// Internal batch-loop result carrying the one logical zone-change owner until
/// its true completion. A pause moves that exact owner into
/// `PendingBatchDeliveries`, rather than reconstructing a tail-only group.
enum BatchDeliveryResult {
    Done(Box<LogicalZoneChangeGroup>),
    NeedsChoice,
}

/// Whether a delivery loop began a new child batch or resumed its active owner.
/// This is structural state, not a boolean: a new batch must not overwrite an
/// outer BatchDelivery parent, while a resumed batch must replace its exact
/// owner in place when it pauses again.
#[derive(Clone, Copy)]
enum BatchDeliveryParking {
    NewChild,
    ReparkActive,
}

/// CR 603.10a batch entry: move many objects to one destination through the
/// pipeline as a single simultaneous departure batch (the mill / mass-bounce /
/// SBA pattern). Each object runs through `move_object`, so per-object `Moved`
/// redirects (Rest in Peace / Leyline of the Void class) fire on every one;
/// after the batch completes, its logical owner derives the CR 603.10a
/// co-departure set from exactly the originally announced battlefield members.
/// Nonbattlefield batches such as a mill receive an owner with an empty
/// prospective-member set while still retaining their exact event occurrences.
///
/// On a mid-batch CR 616.1 ordering choice the surfaced prompt is parked and the
/// undelivered tail is stashed in the active `BatchDelivery` frame; the resume
/// path drains it (`drain_pending_batch_deliveries`). The owner crosses that
/// boundary unchanged, so its final settlement covers the whole batch rather
/// than one delivered segment.
pub(crate) fn move_objects_simultaneously(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    move_objects_simultaneously_then(state, reqs, None, events)
}

/// CR 603.10a + CR 616.1: As [`move_objects_simultaneously`], but runs a typed
/// post-loop cleanup ([`BatchCompletion`]) exactly once after every object in the
/// batch has been delivered — whether the batch completes synchronously or is
/// paused mid-pile by a per-card CR 616.1 ordering choice and finished by the
/// drain path. This is the rest-pile entry (surveil graveyard pile + kept-on-top
/// reorder; manifest dread graveyard pile + reveal-marker cleanup): the moves run
/// through the pipeline so each card's `Moved` redirects fire, and the cleanup
/// that used to run inline at the end of the loop now rides on the parked tail so
/// a pause can never run it early or twice. Its return value covers the whole
/// delivery, including an inline completion tail: `Done` means that tail also
/// settled; `NeedsChoice` means a CR 616.1 replacement choice parked it. Callers
/// may therefore restore priority or run their own tail only after `Done`.
pub(crate) fn move_objects_simultaneously_then(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    completion: Option<BatchCompletion>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    move_arranged_batch_then(state, reqs, Vec::new(), completion, None, events)
}

/// CR 401.4: retain the owner's submitted physical-card order through delivery.
pub(crate) fn move_owner_arranged_batch_then(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    arrangements: Vec<StagedLibraryArrangement>,
    completion: Option<BatchCompletion>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    move_arranged_batch_then(state, reqs, arrangements, completion, None, events)
}

pub(crate) fn move_component_batch_then(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    completion: Option<BatchCompletion>,
    component_delivery_parent: Option<PendingComponentDeliveryParent>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    move_arranged_batch_then(
        state,
        reqs,
        Vec::new(),
        completion,
        component_delivery_parent,
        events,
    )
}

fn move_arranged_batch_then(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    arrangements: Vec<StagedLibraryArrangement>,
    completion: Option<BatchCompletion>,
    mut component_delivery_parent: Option<PendingComponentDeliveryParent>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    let event_start = events.len();
    let zone_change_record_start = state.zone_changes_this_turn.len();
    let ids: Vec<ObjectId> = reqs.iter().map(|r| r.object_id).collect();
    let logical_zone_change_group =
        crate::game::triggers::allocate_logical_zone_change_group(state, &ids);
    let group_id = logical_zone_change_group.logical_group_id;
    // CR 730.3a: merged components are a simultaneous positional population
    // whose owner orders the new library objects, including as a linked child
    // of the parent permanent's delivery. Admit that family with a parent;
    // every other component batch stays legacy.
    let component_family = reqs
        .iter()
        .all(|req| matches!(req.cause, ZoneChangeCause::MergedComponentRouting));
    let staged_admitted = staged_batch_admission(state, &reqs)
        && (component_delivery_parent.is_none() || component_family);
    let staged_parent = if staged_admitted {
        component_delivery_parent.take()
    } else {
        None
    };
    match deliver_batch(
        state,
        reqs,
        logical_zone_change_group,
        BatchDeliveryParking::NewChild,
        staged_admitted,
        arrangements,
        staged_parent,
        events,
    ) {
        BatchDeliveryResult::Done(mut logical_zone_change_group) => {
            crate::game::triggers::complete_logical_zone_trigger_collection(
                state,
                &mut logical_zone_change_group,
                &mut events[event_start..],
            )
            .expect("completed batch owns every terminal zone-change outcome");
            crate::game::triggers::mark_logical_zone_events_consumed_before_priority(
                state,
                &logical_zone_change_group,
                &events[event_start..],
                crate::game::triggers::ConsumedTriggerEventScope::AllCollectors,
            );
            // Synchronous completion (the common single-redirect path): run the
            // cleanup now, and surface a pause it raises to the enclosing caller.
            completion.map_or(BatchMoveResult::Done, |mut completion| {
                crate::types::game_state::settle_batch_delivery_outcome(
                    &mut completion,
                    state,
                    &logical_zone_change_group,
                    &[],
                );
                run_batch_completion(state, completion, events)
            })
        }
        BatchDeliveryResult::NeedsChoice => {
            // `deliver_batch` always stashes the exact owner, including when the
            // paused object was the last member and the undelivered tail is empty.
            // Attach the completion to that same owner so its drain can run it
            // once, after logical settlement.
            let pending = state
                .resolution_stack
                .batch_delivery_owner_mut(group_id)
                .expect("new batch retains its exact owner");
            if !staged_admitted {
                pending.component_delivery_parent = component_delivery_parent;
            }
            pending.completion = completion;
            pending.attempted = ids;
            pending.zone_change_record_start = zone_change_record_start;
            pending.deferred_events.extend(events.drain(event_start..));
            BatchMoveResult::NeedsChoice
        }
    }
}

/// CR 603.10a + CR 616.1: Dispatch a [`BatchCompletion`] to its post-loop
/// behavior. The data lives in `types::game_state`; the behavior lives in
/// `engine_resolution_choices` (kept-card placement / reveal-marker cleanup +
/// continuation drain) so this module stays free of resolution semantics.
fn run_batch_completion(
    state: &mut GameState,
    completion: BatchCompletion,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    crate::game::engine_resolution_choices::run_batch_completion(state, completion, events)
}

/// CR 303.4f / CR 616.1 + CR 603.10a: Hang a [`BatchCompletion`] off the current
/// pause so the drain runs it once the paused move resolves. A single-object
/// [`move_object`] pause (an as-enters aura host pick or a replacement-ordering
/// prompt) does not stash a batch tail, so this creates an empty-`remaining`
/// record carrying only the completion; the drain delivers nothing and runs the
/// completion. Used by the reveal-until / dig kept-card sites to defer the
/// rest-pile move when the kept card's battlefield entry pauses.
pub(crate) fn defer_completion_on_pause(state: &mut GameState, completion: BatchCompletion) {
    // The destination is irrelevant for an empty tail (no object re-delivers).
    ensure_batch_record(state, Zone::Graveyard).completion = Some(completion);
}

/// Return the live parked-batch record, creating an empty-tail one only for a
/// standalone paused delivery that needs a [`BatchCompletion`]. A batch pause
/// always arrives here with its original logical owner already stashed.
fn ensure_batch_record(state: &mut GameState, destination: Zone) -> &mut PendingBatchDeliveries {
    if state.active_batch_delivery().is_none() {
        let zone_change_record_start = state.zone_changes_this_turn.len();
        let paused_current = state.pending_zone_change_delivery_from_replacement();
        let announced_members = paused_current
            .iter()
            .map(|delivery| delivery.member.object_id)
            .collect::<Vec<_>>();
        let logical_zone_change_group =
            crate::game::triggers::allocate_logical_zone_change_group(state, &announced_members);
        state.push_batch_delivery(PendingBatchDeliveries {
            component_delivery_parent: None,
            logical_zone_change_group,
            paused_current,
            remaining: Vec::new(),
            destination,
            source_id: None,
            enter_tapped: EtbTapState::Unspecified,
            exile_tracking: ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: None,
            replacement_applied: HashSet::new(),
            requests: Vec::new(),
            attempted: Vec::new(),
            zone_change_record_start,
            deferred_events: Vec::new(),
            staged: None,
        });
    }
    state
        .active_batch_delivery_mut()
        .expect("pending batch record was just initialized")
}

/// CR 603.10a + CR 616.1: shared batch delivery loop. Runs each request through
/// `move_object_with_terminal`; on a pause, parks the prompt and stashes the
/// undelivered tail with each request's exact heterogeneous context. The exact
/// same logical owner flows into the parked record, which settles only after
/// every requested move has a terminal result.
#[allow(clippy::too_many_arguments)]
fn deliver_batch(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    mut logical_zone_change_group: LogicalZoneChangeGroup,
    parking: BatchDeliveryParking,
    staged_admitted: bool,
    arrangements: Vec<StagedLibraryArrangement>,
    component_delivery_parent: Option<PendingComponentDeliveryParent>,
    events: &mut Vec<GameEvent>,
) -> BatchDeliveryResult {
    if staged_admitted && matches!(parking, BatchDeliveryParking::NewChild) {
        return deliver_staged_batch_entry(
            state,
            reqs,
            logical_zone_change_group,
            arrangements,
            component_delivery_parent,
            events,
        );
    }
    let segment_start = events.len();
    let mut queue = reqs.into_iter();
    while let Some(req) = queue.next() {
        let destination = req.to;
        let face_down_in_exile = req.face_down_in_exile;
        let anticipated_pause = anticipated_zone_change_delivery(state, &req);
        let delivery_start = events.len();
        let object_id = req.object_id;
        let owner = anticipated_pause
            .as_ref()
            .map(|paused| BatchMemberDeliveryOwner {
                group: logical_zone_change_group.logical_group_id,
                member: paused.member,
                expected_event: None,
                population_devour_snapshot: None,
                approved_post_replacement: None,
                inherited_arrangements: Vec::new(),
            });
        match move_batch_member_with_terminal(state, req, owner, events) {
            ZoneMoveTerminalResult::Completed(completion) => {
                logical_zone_change_group
                    .record_delivery_completion(object_id, completion)
                    .expect("batch member records its exact terminal outcome");
            }
            ZoneMoveTerminalResult::NeedsChoice(_) => {
                // CR 616.1: `move_object` already parked the surfaced prompt
                // (centralized park at its `replace_event` NeedsChoice arm);
                // stash the rest of the batch so no object strands. The paused
                // object rides in `state.pending_replacement` and is delivered
                // by the resume path.
                let delegated = state
                    .active_batch_delivery()
                    .and_then(|child| child.component_delivery_parent.as_ref())
                    .filter(|parent| {
                        parent.parent_group_id == logical_zone_change_group.logical_group_id
                    });
                let mut paused_current = delegated
                    .map(|parent| {
                        PendingZoneChangeDelivery::new(parent.member, parent.expected_event.clone())
                    })
                    .or_else(|| state.pending_zone_change_delivery_from_replacement())
                    .or_else(|| {
                        anticipated_pause.map(|mut boundary| {
                            boundary.append_delivery_events(&events[delivery_start..]);
                            boundary
                        })
                    })
                    .expect("parked batch zone change must retain an exact boundary");
                paused_current.face_down_in_exile = face_down_in_exile;
                crate::game::triggers::append_and_collect_logical_zone_trigger_segment(
                    state,
                    &mut logical_zone_change_group,
                    &events[segment_start..delivery_start],
                )
                .expect("paused batch retains its exact delivered segment");
                stash_batch_tail(
                    state,
                    logical_zone_change_group,
                    queue.collect(),
                    destination,
                    Some(paused_current),
                    parking,
                );
                return BatchDeliveryResult::NeedsChoice;
            }
            ZoneMoveTerminalResult::NeedsAuraAttachmentChoice => {
                // CR 303.4f: an aura-host choice flows through
                // `WaitingFor::ReturnAsAuraTarget`, not the replacement-choice
                // resume path. Battlefield-entry batches (reveal-until kept
                // delivery, Dig mass put-all) reach this arm when an entering
                // Aura needs a host; stop and stash the tail so the remainder is
                // delivered when the attachment choice resumes the batch.
                //
                // The stashed tail IS drained correctly on resume: the
                // `ReturnAsAuraTarget` handler (engine.rs:3608-3611) and its
                // chain-resume sibling (engine.rs:3572) both call
                // `drain_pending_batch_deliveries` when
                // `active_batch_delivery().is_some()`, so the aura-attachment
                // pause finishes the parked batch the same way the replacement-
                // choice resume does. (Updated for d5a12b8c6, which added the
                // aura-resume drain; the prior note here that the tail would be
                // "silently drained by the NEXT unrelated resume" is no longer
                // accurate.)
                let paused_current = anticipated_pause.map(|mut boundary| {
                    boundary.append_delivery_events(&events[delivery_start..]);
                    boundary.mark_counted();
                    boundary.face_down_in_exile = face_down_in_exile;
                    boundary
                });
                crate::game::triggers::append_and_collect_logical_zone_trigger_segment(
                    state,
                    &mut logical_zone_change_group,
                    &events[segment_start..delivery_start],
                )
                .expect("paused Aura batch retains its exact delivered segment");
                stash_batch_tail(
                    state,
                    logical_zone_change_group,
                    queue.collect(),
                    destination,
                    paused_current,
                    parking,
                );
                return BatchDeliveryResult::NeedsChoice;
            }
        }
    }
    BatchDeliveryResult::Done(Box::new(logical_zone_change_group))
}

// ---------------------------------------------------------------------------
// R4b: retained common approval and private arrangement
// ---------------------------------------------------------------------------

/// CR 616.1: whether `move_arranged_batch_then` admits a request group to the
/// staged common-approval lifecycle.
///
/// This gate admits only Graveyard moves and positioned Library moves; exempt
/// causes remain excluded. Component-family children may enter through
/// `move_arranged_batch_then`; other linked child families retain legacy delivery.
/// This is not the admission policy of `deliver_staged_change_zone_batch`, which
/// also admits Battlefield entries such as Twilight's Call and Devour cohorts.
fn staged_batch_admission(state: &GameState, reqs: &[ZoneMoveRequest]) -> bool {
    !reqs.is_empty()
        && reqs
            .iter()
            .all(|req| state.objects.contains_key(&req.object_id))
        && reqs.iter().all(|req| {
            req.to == Zone::Graveyard || (req.to == Zone::Library && req.placement.is_some())
        })
        && !reqs.iter().any(|req| req.cause.is_exempt())
}

/// CR 400.3: freeze the original membership, pre-consultation
/// incarnations and recorded owners before the first consultation.
fn new_staged_batch(
    state: &GameState,
    reqs: &[ZoneMoveRequest],
    arrangements: Vec<StagedLibraryArrangement>,
) -> StagedBatchDelivery {
    let members = reqs
        .iter()
        .enumerate()
        .map(|(index, req)| {
            let object = state
                .objects
                .get(&req.object_id)
                .expect("admission captured every staged member");
            StagedBatchMember {
                announcement_index: index as u32,
                request: req.to_pending(),
                identity: ObjectIncarnationRef::from_object(object),
                owner: object.owner,
                state: StagedBatchMemberState::Unconsulted,
                approved_event: None,
                pending_outcome: None,
                terminal: None,
                operation: StagedMemberOperation::ZoneMove,
                approved_post_replacement: None,
            }
        })
        .collect();
    StagedBatchDelivery {
        stage: BatchDeliveryStage::Approving,
        members,
        cursor: 0,
        arrangements,
        arrangement_cursor: 0,
        population_devour_snapshot: None,
    }
}

/// Build the one parked owner a staged batch uses for its whole lifecycle.
fn new_staged_owner(
    state: &GameState,
    reqs: &[ZoneMoveRequest],
    logical_zone_change_group: LogicalZoneChangeGroup,
    staged: StagedBatchDelivery,
    component_delivery_parent: Option<PendingComponentDeliveryParent>,
) -> PendingBatchDeliveries {
    let first = reqs.first().expect("admission requires a member");
    let remaining = reqs.iter().map(|req| req.object_id).collect();
    let attempted = reqs.iter().map(|req| req.object_id).collect();
    PendingBatchDeliveries {
        component_delivery_parent,
        logical_zone_change_group,
        paused_current: None,
        remaining,
        destination: first.to,
        source_id: first.source().filter(|&source| source != first.object_id),
        enter_tapped: first.mods.enter_tapped,
        exile_tracking: first.exile_links.tracking,
        library_placement: first.placement.clone(),
        replacement_applied: first.replacement_applied.clone(),
        completion: None,
        requests: reqs.iter().map(|req| req.to_pending()).collect(),
        attempted,
        zone_change_record_start: state.zone_changes_this_turn.len(),
        deferred_events: Vec::new(),
        staged: Some(staged),
    }
}

/// Recompute the legacy compatibility projection from the staged cursor. The
/// staged payload is authoritative; `requests`/`remaining` are exactly the
/// not-yet-consulted members in announcement order.
fn sync_staged_requests(owner: &mut PendingBatchDeliveries) {
    let Some(staged) = owner.staged.as_ref() else {
        return;
    };
    let requests: Vec<PendingBatchZoneMoveRequest> = staged
        .members
        .iter()
        .filter(|member| member.state == StagedBatchMemberState::Unconsulted)
        .map(|member| member.request.clone())
        .collect();
    let remaining: Vec<ObjectId> = staged
        .members
        .iter()
        .filter(|member| member.state == StagedBatchMemberState::Unconsulted)
        .map(|member| member.identity.object_id)
        .collect();
    owner.requests = requests;
    owner.remaining = remaining;
}

/// The exact proposal one admitted member is consulted with. Placement-origin
/// members keep the caller's requested position on the proposal (so the final
/// event records it); every other member seeds the retained request context
/// exactly as the generic pipeline constructor does for non-battlefield moves.
struct PreparedBatchMemberEvent {
    proposed: ProposedEvent,
}

fn prepared_batch_member_event(
    state: &GameState,
    request: &ZoneMoveRequest,
) -> Option<PreparedBatchMemberEvent> {
    let object = state.objects.get(&request.object_id)?;
    let proposed = prepare_zone_move_proposal(
        state,
        request.object_id,
        object.zone,
        request.to,
        request.source().unwrap_or(request.object_id),
        request.mods.enter_transformed,
        request.mods.enter_tapped,
        request.mods.enters_attacking,
        request.mods.controller_override,
        &request.mods.enter_with_counters,
        request.mods.face_down_profile.as_ref(),
        request.face_down_in_exile,
        request.mods.chain_referent,
        request.placement.clone(),
        request.mods.attach_to,
        request.mods.performed_by,
        request.replacement_applied.clone(),
    )?;
    Some(PreparedBatchMemberEvent { proposed })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedStop {
    Paused,
    Arranging,
    Delivered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedConsult {
    Continue,
    Paused,
    AllConsulted,
}

/// Drive the staged lifecycle from its current stage to the next stop. The
/// owner is detached from the resolution stack for this call: delivering while
/// the batch frame were installed would let a delivery child bury it, so the
/// caller parks it only when the run actually pauses.
fn run_staged_batch(
    state: &mut GameState,
    owner: &mut PendingBatchDeliveries,
    events: &mut Vec<GameEvent>,
) -> StagedStop {
    loop {
        let Some(stage) = owner.staged.as_ref().map(|staged| staged.stage) else {
            return StagedStop::Delivered;
        };
        match stage {
            BatchDeliveryStage::Approving => match consult_next_staged_member(state, owner, events)
            {
                StagedConsult::Continue => continue,
                StagedConsult::Paused => return StagedStop::Paused,
                StagedConsult::AllConsulted => {
                    begin_staged_arrangement(state, owner);
                    continue;
                }
            },
            BatchDeliveryStage::Arranging => {
                if publish_next_staged_arrangement(state, owner).is_some() {
                    return StagedStop::Arranging;
                }
                continue;
            }
            BatchDeliveryStage::Delivering => {
                return deliver_staged_members(state, owner, events);
            }
        }
    }
}

/// CR 616.1: consult one member. Approval never delivers: an
/// `Execute` result is retained as the exact final event and the cursor moves
/// on. A `NeedsChoice` parks the exact boundary and the batch frame.
fn consult_next_staged_member(
    state: &mut GameState,
    owner: &mut PendingBatchDeliveries,
    events: &mut Vec<GameEvent>,
) -> StagedConsult {
    let Some(member) = owner
        .staged
        .as_ref()
        .and_then(|staged| staged.members.get(staged.cursor))
        .cloned()
    else {
        return StagedConsult::AllConsulted;
    };
    let destroy_operation = match member.operation {
        StagedMemberOperation::Destroy(operation) => Some(operation),
        StagedMemberOperation::ZoneMove => None,
    };
    match (destroy_operation, member.state) {
        (_, StagedBatchMemberState::Terminal | StagedBatchMemberState::Abandoned) => {
            // CR 800.4a: a later member may have left while an earlier chooser
            // was answering. Its frozen slot is finished, not a live pause.
            advance_staged_cursor(owner, member.announcement_index as usize);
            return StagedConsult::Continue;
        }
        (Some(operation), StagedBatchMemberState::Unconsulted) => {
            return consult_staged_destroy_outer(state, owner, events, member, operation);
        }
        (Some(operation), StagedBatchMemberState::InnerConsulting) => {
            return consult_staged_destroy_inner(state, owner, events, member, operation);
        }
        (_, StagedBatchMemberState::Unconsulted) => {}
        (
            _,
            StagedBatchMemberState::Approved
            | StagedBatchMemberState::OuterConsulting
            | StagedBatchMemberState::InnerConsulting
            | StagedBatchMemberState::PreventedWithChild,
        ) => {
            // A member still consulting has a live child; the drain finalizes it.
            return StagedConsult::Paused;
        }
    }
    let request = ZoneMoveRequest::from_pending(member.request.clone());
    let Some(object) = state.objects.get(&request.object_id) else {
        mark_staged_member_terminal(
            state,
            owner,
            member.announcement_index as usize,
            ZoneMoveCompletion::Remained,
            events,
        );
        return StagedConsult::Continue;
    };
    if zone_move_is_primitive_noop(state, object, request.to) {
        mark_staged_member_terminal(
            state,
            owner,
            member.announcement_index as usize,
            ZoneMoveCompletion::Remained,
            events,
        );
        return StagedConsult::Continue;
    }
    let Some(prepared) = prepared_batch_member_event(state, &request) else {
        mark_staged_member_terminal(
            state,
            owner,
            member.announcement_index as usize,
            ZoneMoveCompletion::Remained,
            events,
        );
        return StagedConsult::Continue;
    };
    let cursor = member.announcement_index as usize;
    set_staged_member_state(owner, cursor, StagedBatchMemberState::InnerConsulting);
    let drain_point = state.resolution_stack.consult_drain_point();
    match replacement::replace_event(state, prepared.proposed.clone(), events) {
        ReplacementResult::Execute(event) => {
            retain_member_drain(state, owner, cursor, drain_point);
            record_staged_member_approval(owner, cursor, event);
            StagedConsult::Continue
        }
        ReplacementResult::Prevented => {
            let live_child = state.pending_replacement.is_some()
                || !matches!(
                    state.waiting_for,
                    WaitingFor::Priority { .. } | WaitingFor::ReplacementChoice { .. }
                );
            if live_child {
                if let Some(member) = owner
                    .staged
                    .as_mut()
                    .and_then(|staged| staged.members.get_mut(cursor))
                {
                    member.state = StagedBatchMemberState::PreventedWithChild;
                    member.pending_outcome = Some(StagedMemberOutcome::Prevented);
                }
                sync_staged_requests(owner);
                return StagedConsult::Paused;
            }
            mark_staged_member_terminal(
                state,
                owner,
                cursor,
                ZoneMoveCompletion::Prevented,
                events,
            );
            StagedConsult::Continue
        }
        ReplacementResult::NeedsChoice(player) => {
            if let Some(pending) = state.pending_replacement.as_mut() {
                if let ProposedEvent::ZoneChange {
                    library_placement: Some(position),
                    ..
                } = &prepared.proposed
                {
                    pending.library_placement = Some(position.clone());
                }
            }
            // The authoritative boundary is the one the replacement parked, not
            // a re-derived copy: an answered pre-entry choice or CR 616.1f
            // re-park may have left a modified proposal on the record.
            let boundary = state
                .pending_zone_change_delivery_from_replacement()
                .filter(|parked| parked.member == member.identity)
                .unwrap_or_else(|| {
                    PendingZoneChangeDelivery::new(member.identity, prepared.proposed.clone())
                });
            owner.paused_current = Some(boundary);
            sync_staged_requests(owner);
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            StagedConsult::Paused
        }
    }
}

/// CR 701.8a: retain outer approval on the authoritative staged Destroy payload.
fn approve_staged_destroy_outer(owner: &mut PendingBatchDeliveries, cursor: usize) {
    let member = owner
        .staged
        .as_mut()
        .and_then(|staged| staged.members.get_mut(cursor))
        .expect("outer approval retains its staged member");
    let StagedMemberOperation::Destroy(operation) = member.operation else {
        panic!("only a Destroy member can approve its outer event");
    };
    member.operation = StagedMemberOperation::Destroy(StagedDestroyOperation {
        outer: StagedDestroyOuterApproval::Approved,
        ..operation
    });
}

/// CR 701.8a + CR 616.1: stage one Destroy member's outer event. The four
/// pre-proposal guards are re-applied here, before the outer `Destroy` reaches
/// the replacement pipeline.
fn consult_staged_destroy_outer(
    state: &mut GameState,
    owner: &mut PendingBatchDeliveries,
    events: &mut Vec<GameEvent>,
    member: StagedBatchMember,
    operation: StagedDestroyOperation,
) -> StagedConsult {
    let cursor = member.announcement_index as usize;
    let object_id = member.identity.object_id;
    let Some(object) = state.objects.get(&object_id) else {
        mark_staged_member_terminal(state, owner, cursor, ZoneMoveCompletion::Remained, events);
        return StagedConsult::Continue;
    };
    if object.is_emblem
        || object.zone != Zone::Battlefield
        || object.has_keyword(&crate::types::keywords::Keyword::Indestructible)
    {
        mark_staged_member_terminal(state, owner, cursor, ZoneMoveCompletion::Remained, events);
        return StagedConsult::Continue;
    }
    let proposed = ProposedEvent::Destroy {
        object_id,
        source: Some(operation.source),
        cant_regenerate: operation.cant_regenerate,
        applied: HashSet::new(),
    };
    set_staged_member_state(owner, cursor, StagedBatchMemberState::OuterConsulting);
    let drain_point = state.resolution_stack.consult_drain_point();
    match replacement::replace_event(state, proposed.clone(), events) {
        ReplacementResult::Execute(event) => {
            if matches!(event, ProposedEvent::Destroy { .. }) {
                retain_member_drain(state, owner, cursor, drain_point);
                approve_staged_destroy_outer(owner, cursor);
                set_staged_member_state(owner, cursor, StagedBatchMemberState::InnerConsulting);
                let member = owner
                    .staged
                    .as_ref()
                    .and_then(|staged| staged.members.get(cursor))
                    .cloned()
                    .expect("outer-approved Destroy member remains the cursor");
                consult_staged_destroy_inner(state, owner, events, member, operation)
            } else {
                // CR 614.6: the outer Destroy replacement redirected directly
                // to a zone change; it is the final event.
                record_staged_member_approval(owner, cursor, event);
                StagedConsult::Continue
            }
        }
        ReplacementResult::Prevented => {
            // CR 616.1 + CR 702.89a: a preventer (Umbra armor) can destroy a
            // dependent permanent as part of its replacing action; that nested
            // move may itself be waiting on a replacement choice. Keep the
            // member parked until the child resolves instead of consulting the
            // next member over a live choice.
            let live_child = state.pending_replacement.is_some()
                || !matches!(
                    state.waiting_for,
                    WaitingFor::Priority { .. } | WaitingFor::ReplacementChoice { .. }
                );
            if live_child {
                if let Some(member) = owner
                    .staged
                    .as_mut()
                    .and_then(|staged| staged.members.get_mut(cursor))
                {
                    member.state = StagedBatchMemberState::PreventedWithChild;
                    member.pending_outcome = Some(StagedMemberOutcome::Prevented);
                }
                sync_staged_requests(owner);
                return StagedConsult::Paused;
            }
            mark_staged_member_terminal(
                state,
                owner,
                cursor,
                ZoneMoveCompletion::Prevented,
                events,
            );
            StagedConsult::Continue
        }
        ReplacementResult::NeedsChoice(player) => {
            let boundary = state
                .pending_staged_approval_boundary()
                .filter(|(identity, _)| *identity == member.identity)
                .map(|(identity, expected)| PendingZoneChangeDelivery::new(identity, expected))
                .unwrap_or_else(|| {
                    PendingZoneChangeDelivery::new(member.identity, proposed.clone())
                });
            owner.paused_current = Some(boundary);
            sync_staged_requests(owner);
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            StagedConsult::Paused
        }
    }
}

/// CR 701.8a + CR 614.6: propose the inner guarded `Battlefield → Graveyard`
/// move for an outer-approved Destroy member. This is a second, independent
/// replacement consult.
fn consult_staged_destroy_inner(
    state: &mut GameState,
    owner: &mut PendingBatchDeliveries,
    events: &mut Vec<GameEvent>,
    member: StagedBatchMember,
    operation: StagedDestroyOperation,
) -> StagedConsult {
    let cursor = member.announcement_index as usize;
    let object_id = member.identity.object_id;
    if !state.objects.contains_key(&object_id) {
        mark_staged_member_terminal(state, owner, cursor, ZoneMoveCompletion::Remained, events);
        return StagedConsult::Continue;
    }
    let proposed = ProposedEvent::zone_change(
        object_id,
        Zone::Battlefield,
        Zone::Graveyard,
        Some(operation.source),
    );
    set_staged_member_state(owner, cursor, StagedBatchMemberState::InnerConsulting);
    let drain_point = state.resolution_stack.consult_drain_point();
    match replacement::replace_event(state, proposed.clone(), events) {
        ReplacementResult::Execute(event) => {
            retain_member_drain(state, owner, cursor, drain_point);
            record_staged_member_approval(owner, cursor, event);
            StagedConsult::Continue
        }
        ReplacementResult::Prevented => {
            // CR 616.1 + CR 614.6: a replacing action can move a dependent
            // permanent; that nested move may itself be waiting on a replacement
            // choice. Keep the
            // member parked until the child resolves instead of consulting the
            // next member over a live choice.
            let live_child = state.pending_replacement.is_some()
                || !matches!(
                    state.waiting_for,
                    WaitingFor::Priority { .. } | WaitingFor::ReplacementChoice { .. }
                );
            if live_child {
                if let Some(member) = owner
                    .staged
                    .as_mut()
                    .and_then(|staged| staged.members.get_mut(cursor))
                {
                    member.state = StagedBatchMemberState::PreventedWithChild;
                    member.pending_outcome = Some(StagedMemberOutcome::Prevented);
                }
                sync_staged_requests(owner);
                return StagedConsult::Paused;
            }
            mark_staged_member_terminal(
                state,
                owner,
                cursor,
                ZoneMoveCompletion::Prevented,
                events,
            );
            StagedConsult::Continue
        }
        ReplacementResult::NeedsChoice(player) => {
            let boundary = state
                .pending_zone_change_delivery_from_replacement()
                .filter(|parked| parked.member == member.identity)
                .unwrap_or_else(|| {
                    PendingZoneChangeDelivery::new(member.identity, proposed.clone())
                });
            owner.paused_current = Some(boundary);
            sync_staged_requests(owner);
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            StagedConsult::Paused
        }
    }
}

fn set_staged_member_state(
    owner: &mut PendingBatchDeliveries,
    index: usize,
    new_state: StagedBatchMemberState,
) {
    if let Some(member) = owner
        .staged
        .as_mut()
        .and_then(|staged| staged.members.get_mut(index))
    {
        member.state = new_state;
    }
    sync_staged_requests(owner);
}

fn record_staged_member_approval(
    owner: &mut PendingBatchDeliveries,
    index: usize,
    event: ProposedEvent,
) {
    if let Some(staged) = owner.staged.as_mut() {
        if let Some(member) = staged.members.get_mut(index) {
            member.state = StagedBatchMemberState::Approved;
            member.approved_event = Some(event);
            member.pending_outcome = None;
        }
        staged.cursor = staged.cursor.max(index + 1);
    }
    owner.paused_current = None;
    sync_staged_requests(owner);
}

fn mark_staged_member_terminal(
    state: &mut GameState,
    owner: &mut PendingBatchDeliveries,
    index: usize,
    terminal: ZoneMoveCompletion,
    events: &mut Vec<GameEvent>,
) {
    // CR 608.2c: the mass hand-exile counter is the completion's typed
    // `MassAllMembers` policy; a targeted or choice-selected population never
    // increments it.
    let count_hand_exile = matches!(
        owner.completion.as_ref(),
        Some(crate::types::game_state::BatchCompletion::ChangeZonePopulationSettled { tail, .. })
            if matches!(
                tail.count,
                crate::types::game_state::ChangeZoneCountPolicy::MassAllMembers
            )
    );
    // CR 400.7 + CR 608.2c: consuming the member's own exile link is the
    // completion tail's typed fact — the same tail that carries the count
    // policy. Link creation for a later return is a different producer and
    // keeps its links.
    let own_exile_links = match owner.completion.as_ref() {
        Some(crate::types::game_state::BatchCompletion::ChangeZonePopulationSettled {
            tail,
            ..
        }) => tail.own_exile_links,
        _ => crate::types::game_state::OwnExileLinkSettlement::Retain,
    };
    let mut destroyed = None;
    let mut change_zone_bookkeeping = None;
    if let Some(staged) = owner.staged.as_mut() {
        if let Some(member) = staged.members.get_mut(index) {
            if let StagedMemberOperation::Destroy(StagedDestroyOperation {
                source,
                outer: StagedDestroyOuterApproval::Approved,
                ..
            }) = member.operation
            {
                destroyed = Some((member.identity.object_id, source));
            } else if matches!(member.operation, StagedMemberOperation::ZoneMove) {
                change_zone_bookkeeping =
                    Some((member.identity.object_id, member.approved_event.clone()));
            }
            member.state = StagedBatchMemberState::Terminal;
            member.approved_post_replacement = None;
            member.terminal = Some(terminal);
            member.pending_outcome = None;
        }
        staged.cursor = staged.cursor.max(index + 1);
    }
    owner.paused_current = None;
    sync_staged_requests(owner);
    if let Some((object_id, source_id)) = destroyed {
        // CR 701.8a: the singleton emits `CreatureDestroyed` for both an inner
        // `Moved` and an inner `Prevented`; the outer approval is the witness.
        events.push(GameEvent::CreatureDestroyed {
            object_id,
            source_id: Some(source_id),
        });
    }
    if let Some((object_id, approved_event)) = change_zone_bookkeeping {
        note_change_zone_member_terminal(
            state,
            object_id,
            approved_event.as_ref(),
            terminal,
            count_hand_exile,
            own_exile_links,
        );
    }
}

/// CR 400.7 + CR 608.2c: the legacy per-member ChangeZone bookkeeping the
/// producer loop ran inline after each move. A staged member may pause before
/// delivery, so the rule runs at the delivery terminal instead — derived from
/// the member's own typed request tracking and the batch completion's typed
/// count policy, never from parallel flags.
fn note_change_zone_member_terminal(
    state: &mut GameState,
    object_id: ObjectId,
    approved_event: Option<&ProposedEvent>,
    terminal: ZoneMoveCompletion,
    count_hand_exile: bool,
    own_exile_links: crate::types::game_state::OwnExileLinkSettlement,
) {
    // `approved_event` is the member's own approved move: only a Hand->Exile
    // terminal counts toward the mass hand-exile producer.
    if count_hand_exile && terminal == ZoneMoveCompletion::Moved {
        let hand_to_exile = matches!(
            approved_event,
            Some(ProposedEvent::ZoneChange {
                from: Zone::Hand,
                to: Zone::Exile,
                ..
            })
        );
        if hand_to_exile {
            state.exiled_from_hand_this_resolution =
                state.exiled_from_hand_this_resolution.saturating_add(1);
        }
    }
    match own_exile_links {
        crate::types::game_state::OwnExileLinkSettlement::Retain => {}
        crate::types::game_state::OwnExileLinkSettlement::Consume => {
            state.exile_links.retain(|link| link.exiled_id != object_id);
        }
    }
}

/// CR 401.4: final approvals sharing an owner and fixed position form one group.
pub(crate) fn staged_arrangement_groups(
    staged: &StagedBatchDelivery,
) -> Vec<((PlayerId, LibraryPosition), Vec<usize>)> {
    let mut groups: Vec<((PlayerId, LibraryPosition), Vec<usize>)> = Vec::new();
    for (index, member) in staged.members.iter().enumerate() {
        if !matches!(
            member.state,
            StagedBatchMemberState::Approved | StagedBatchMemberState::Terminal
        ) {
            continue;
        }
        let Some(ProposedEvent::ZoneChange {
            to: Zone::Library,
            library_placement: Some(position),
            ..
        }) = member.approved_event.as_ref()
        else {
            continue;
        };
        match position {
            LibraryPosition::Top | LibraryPosition::Bottom | LibraryPosition::NthFromTop { .. } => {
            }
            LibraryPosition::BeneathTop { .. } | LibraryPosition::RandomWithinTop { .. } => {
                continue
            }
        }
        let key = (member.owner, position.clone());
        match groups.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, indices)) => indices.push(index),
            None => groups.push((key, vec![index])),
        }
    }
    // Numbered blocks need a stable base: library-origin removals or a second
    // position in this library keep the ordinary per-member placement route.
    groups.retain(|((owner, position), _)| {
        !matches!(position, LibraryPosition::NthFromTop { .. })
            || !staged.members.iter().any(|member| {
                if member.owner != *owner
                    || !matches!(
                        member.state,
                        StagedBatchMemberState::Approved | StagedBatchMemberState::Terminal
                    )
                {
                    return false;
                }
                let Some(ProposedEvent::ZoneChange {
                    from,
                    to: Zone::Library,
                    library_placement: Some(other_position),
                    ..
                }) = member.approved_event.as_ref()
                else {
                    return false;
                };
                match other_position {
                    LibraryPosition::Top
                    | LibraryPosition::Bottom
                    | LibraryPosition::NthFromTop { .. } => {
                        other_position != position || *from == Zone::Library
                    }
                    LibraryPosition::BeneathTop { .. }
                    | LibraryPosition::RandomWithinTop { .. } => false,
                }
            })
    });
    groups
}

/// CR 401.4: derive the private owner/position groups from final
/// approvals. Top/Bottom and domain-gated numbered positions are grouped;
/// request-origin groups replay the legacy insertion order without a prompt.
fn begin_staged_arrangement(state: &GameState, owner: &mut PendingBatchDeliveries) {
    let Some(staged) = owner.staged.as_mut() else {
        return;
    };
    let groups = staged_arrangement_groups(staged);
    // CR 401.4 + CR 730.3a: outer seeds lose redirected members only after
    // consultation. A linked child preserves the parent's full physical ranks.
    let mut arrangements = std::mem::take(&mut staged.arrangements);
    for arrangement in &mut arrangements {
        let inherited = owner
            .component_delivery_parent
            .as_ref()
            .is_some_and(|link| link.inherited_arrangements.contains(arrangement));
        if !inherited {
            arrangement.batch.members.retain(|entry| {
                staged.members.iter().any(|member| {
                    (entry.identity == member.identity
                        || entry.departing_permanent == Some(member.identity))
                        && matches!(
                            member.state,
                            StagedBatchMemberState::Approved | StagedBatchMemberState::Terminal
                        )
                        && matches!(
                            member.approved_event.as_ref(),
                            Some(ProposedEvent::ZoneChange {
                                to: Zone::Library,
                                library_placement: Some(position),
                                ..
                            }) if *position == arrangement.position
                        )
                })
            });
        }
    }
    arrangements.retain(|arrangement| !arrangement.batch.members.is_empty());
    for ((owner_id, position), indices) in groups {
        if let Some(seed) = arrangements.iter().find(|arrangement| {
            arrangement.batch.owner == owner_id && arrangement.position == position
        }) {
            debug_assert!(
                indices.iter().all(|&index| {
                    seed.batch.members.iter().any(|entry| {
                        entry.identity == staged.members[index].identity
                            || entry.departing_permanent == Some(staged.members[index].identity)
                    })
                }),
                "submitted arrangement covers every approval at its key"
            );
            continue;
        }
        let announced: Vec<&StagedBatchMember> = indices
            .iter()
            .map(|&index| &staged.members[index])
            .collect();
        // CR 730.3a + CR 401.4: a merged permanent's absorbed components are
        // new objects put into the same library; their owner arranges them
        // even when the inherited position itself is unchanged.
        let merged_components = announced.iter().any(|member| {
            matches!(
                member.request.cause,
                crate::types::game_state::PendingBatchZoneChangeCause::MergedComponentRouting
            )
        });
        let needs_prompt = announced.len() >= 2
            && (merged_components
                || announced
                    .iter()
                    .any(|member| staged_member_placement_is_replacement(member)));
        let ordered: Vec<&StagedBatchMember> = if needs_prompt {
            announced.clone()
        } else if matches!(
            position,
            LibraryPosition::Top | LibraryPosition::NthFromTop { .. }
        ) {
            announced.iter().rev().copied().collect()
        } else {
            announced.clone()
        };
        let members = ordered
            .iter()
            .map(|member| {
                MassLibraryOrderMember::card(
                    member.identity,
                    member
                        .approved_event
                        .as_ref()
                        .and_then(|event| match event {
                            ProposedEvent::ZoneChange { from, .. } => Some(*from),
                            _ => None,
                        })
                        .unwrap_or(member.request.destination),
                )
            })
            .collect();
        arrangements.push(StagedLibraryArrangement {
            position,
            batch: MassLibraryOrderBatch {
                owner: owner_id,
                arranger: None,
                members,
            },
            status: if needs_prompt {
                StagedArrangementStatus::Awaiting
            } else {
                StagedArrangementStatus::Derived
            },
        });
    }
    // CR 101.4: store the groups in APNAP order, so the monotone arrangement
    // cursor IS the publication order and a later submission cannot skip a
    // still-awaiting earlier owner. The sort is stable, so an owner's own
    // groups keep the order they were announced in, and every group keeps its
    // within-group member order untouched.
    let apnap = crate::game::players::apnap_order(state);
    arrangements.sort_by_key(|arrangement| {
        apnap
            .iter()
            .position(|player| *player == arrangement.batch.owner)
            .unwrap_or(usize::MAX)
    });
    staged.arrangements = arrangements;
    staged.arrangement_cursor = 0;
    // The shared cursor restarts for delivery: every member has been consulted,
    // so `Delivering` walks the original announcement order from index 0.
    staged.cursor = 0;
    staged.stage = if staged
        .arrangements
        .iter()
        .any(|arrangement| arrangement.status == StagedArrangementStatus::Awaiting)
    {
        BatchDeliveryStage::Arranging
    } else {
        BatchDeliveryStage::Delivering
    };
    owner.paused_current = None;
    sync_staged_requests(owner);
}

fn staged_member_placement_is_replacement(member: &StagedBatchMember) -> bool {
    let Some(ProposedEvent::ZoneChange {
        to,
        library_placement,
        ..
    }) = member.approved_event.as_ref()
    else {
        return false;
    };
    *to != member.request.destination || library_placement != &member.request.library_placement
}

/// CR 401.4 + CR 101.4: publish the cursor group's owner-addressed arrangement
/// prompt. Returns `Some(Arranging)` when a prompt was set, `None` when every
/// group is already final (the stage then becomes `Delivering`).
///
/// `begin_staged_arrangement` stored the groups in APNAP order, so the first
/// still-`Awaiting` group is the APNAP-earliest one AND the monotone cursor the
/// submission validator checks against. Nothing here may reorder them.
fn publish_next_staged_arrangement(
    state: &mut GameState,
    owner: &mut PendingBatchDeliveries,
) -> Option<StagedStop> {
    let staged = owner.staged.as_mut()?;
    let index = staged
        .arrangements
        .iter()
        .position(|arrangement| arrangement.status == StagedArrangementStatus::Awaiting);
    let Some(index) = index else {
        staged.stage = BatchDeliveryStage::Delivering;
        owner.paused_current = None;
        return None;
    };
    staged.arrangement_cursor = index;
    let arrangement = &mut staged.arrangements[index];
    arrangement.status = StagedArrangementStatus::Offered;
    let library_owner = arrangement.batch.owner;
    let cards: Vec<ObjectId> = arrangement
        .batch
        .members
        .iter()
        .map(|member| member.identity.object_id)
        .collect();
    // OrderOnly's shared UI labels the block "top"; both top and numbered
    // blocks are submitted in their final top-down order.
    let top_count = if matches!(
        arrangement.position,
        LibraryPosition::Top | LibraryPosition::NthFromTop { .. }
    ) {
        cards.len()
    } else {
        0
    };
    let source_id = staged
        .members
        .iter()
        .find(|member| member.owner == library_owner)
        .and_then(|member| match &member.request.cause {
            PendingBatchZoneChangeCause::Effect { source }
            | PendingBatchZoneChangeCause::Cost { source } => Some(*source),
            _ => None,
        });
    state.waiting_for = WaitingFor::new_dig_rest_split_for_batch(
        library_owner,
        library_owner,
        cards,
        top_count,
        DigRestSplitScope::OrderOnly,
        source_id,
        owner.logical_zone_change_group.logical_group_id,
    );
    Some(StagedStop::Arranging)
}

/// CR 401.4: deliver every approved member in announcement order,
/// using the tail-insertion slot for grouped fixed library positions.
fn deliver_staged_members(
    state: &mut GameState,
    owner: &mut PendingBatchDeliveries,
    events: &mut Vec<GameEvent>,
) -> StagedStop {
    let segment_start = events.len();
    loop {
        let Some(member) = owner
            .staged
            .as_ref()
            .and_then(|staged| staged.members.get(staged.cursor))
            .cloned()
        else {
            return StagedStop::Delivered;
        };
        let cursor = member.announcement_index as usize;
        match member.state {
            StagedBatchMemberState::Terminal | StagedBatchMemberState::Abandoned => {
                advance_staged_cursor(owner, cursor);
                continue;
            }
            StagedBatchMemberState::Approved => {}
            _ => return StagedStop::Paused,
        }
        let live_identity = state
            .objects
            .get(&member.identity.object_id)
            .map(ObjectIncarnationRef::from_object);
        if live_identity != Some(member.identity) {
            mark_staged_member_terminal(state, owner, cursor, ZoneMoveCompletion::Remained, events);
            continue;
        }
        let Some(event) = member.approved_event.clone() else {
            mark_staged_member_terminal(state, owner, cursor, ZoneMoveCompletion::Remained, events);
            continue;
        };
        let event_cause = match &event {
            ProposedEvent::ZoneChange { cause, .. } => *cause,
            _ => {
                mark_staged_member_terminal(
                    state,
                    owner,
                    cursor,
                    ZoneMoveCompletion::Remained,
                    events,
                );
                continue;
            }
        };
        let explicit_index = staged_arrangement_slot(state, owner, &member);
        let delivery_start = events.len();
        // CR 614.13a + CR 616.1: hand the population snapshot and the retained
        // drain to the aura-host delivery, which reinstalls them on Deliver.
        let to_battlefield = matches!(
            &event,
            ProposedEvent::ZoneChange {
                to: Zone::Battlefield,
                ..
            }
        );
        let population_devour_snapshot = (to_battlefield
            && crate::game::engine_replacement::object_has_devour_replacement(
                state,
                member.identity.object_id,
            ))
        .then(|| {
            owner
                .staged
                .as_ref()
                .and_then(|staged| staged.population_devour_snapshot.clone())
        })
        .flatten();
        let approved_post_replacement = owner
            .staged
            .as_mut()
            .and_then(|staged| staged.members.get_mut(cursor))
            .and_then(|member| member.approved_post_replacement.take());
        let inherited_arrangements = owner.staged.as_ref().map_or_else(Vec::new, |staged| {
            staged
                .arrangements
                .iter()
                .filter(|arrangement| {
                    arrangement
                        .batch
                        .members
                        .iter()
                        .any(|entry| entry.departing_permanent == Some(member.identity))
                })
                .cloned()
                .collect()
        });
        let delivery_owner = Some(BatchMemberDeliveryOwner {
            group: owner.logical_zone_change_group.logical_group_id,
            member: member.identity,
            expected_event: Some(event.clone()),
            population_devour_snapshot,
            approved_post_replacement,
            inherited_arrangements,
        });
        match deliver_replaced_zone_change_with_aura_host(
            state,
            member.identity.object_id,
            member.identity,
            event.clone(),
            event_cause,
            member.request.exile_duration.as_ref(),
            member.request.exile_controller,
            matches!(
                member.request.exile_tracking,
                ZoneDeliveryExileTracking::TrackBySource
            ),
            None,
            explicit_index,
            delivery_owner,
            events,
        ) {
            AuraAwareZoneDelivery::Completed(completion) => {
                mark_staged_member_terminal(state, owner, cursor, completion, events);
                // CR 614.13a: retire this completed member's eligibility frame.
                state.clear_completed_active_devour_snapshot();
            }
            AuraAwareZoneDelivery::NeedsChoice(_) | AuraAwareZoneDelivery::NeedsAuraHostChoice => {
                // Keep the authoritative parked boundary and retain this
                // member's own delivered slice; the drain derives the terminal
                // from it when no sidecar classification exists. A host choice
                // pauses AFTER the entry was delivered, so its retained slice
                // already witnesses the member's terminal.
                let paused_current = staged_delivery_pause(
                    state,
                    owner.logical_zone_change_group.logical_group_id,
                    &member,
                    &event,
                    &events[delivery_start..],
                );
                crate::game::triggers::append_and_collect_logical_zone_trigger_segment(
                    state,
                    &mut owner.logical_zone_change_group,
                    &events[segment_start..delivery_start],
                )
                .expect("staged batch retains its exact delivered segment");
                owner.paused_current = Some(paused_current);
                sync_staged_requests(owner);
                return StagedStop::Paused;
            }
        }
    }
}

/// CR 616.1: retain the approved consultation's post-replacement drain
/// on the member.
fn retain_member_drain(
    state: &mut GameState,
    owner: &mut PendingBatchDeliveries,
    cursor: usize,
    point: (crate::types::resolution::ChildStackDepth, Option<usize>),
) {
    let Some(drain) = state
        .resolution_stack
        .retain_consultation_post_replacement(point)
    else {
        return;
    };
    attach_member_drain(owner, cursor, drain);
}

fn attach_member_drain(
    owner: &mut PendingBatchDeliveries,
    cursor: usize,
    drain: crate::types::game_state::PostReplacementDrainStack,
) {
    if let Some(member) = owner
        .staged
        .as_mut()
        .and_then(|staged| staged.members.get_mut(cursor))
    {
        match member.approved_post_replacement.as_mut() {
            Some(existing) => existing.append_retained(drain),
            None => member.approved_post_replacement = Some(drain),
        }
    }
}

/// Build the pause record for one delivery-stage pause, preferring a component
/// child's retained parent boundary to its component-specific replacement.
/// Retain this member's own delivered slice for terminal derivation.
fn staged_delivery_pause(
    state: &GameState,
    group: LogicalZoneChangeGroupId,
    member: &StagedBatchMember,
    event: &ProposedEvent,
    delivery_events: &[GameEvent],
) -> PendingZoneChangeDelivery {
    // CR 903.9b + CR 903.9c: a commander's component replacement is a child
    // event; its parent must retain the original whole-permanent boundary.
    let delegated = state
        .active_batch_delivery()
        .and_then(|child| child.component_delivery_parent.as_ref())
        .filter(|parent| parent.parent_group_id == group);
    let mut paused = delegated
        .map(|parent| PendingZoneChangeDelivery::new(parent.member, parent.expected_event.clone()))
        .or_else(|| {
            state
                .pending_zone_change_delivery_from_replacement()
                .filter(|parked| parked.member == member.identity)
        })
        .unwrap_or_else(|| PendingZoneChangeDelivery::new(member.identity, event.clone()));
    paused.face_down_in_exile = member.request.face_down_in_exile;
    paused.append_delivery_events(delivery_events);
    paused
}

fn advance_staged_cursor(owner: &mut PendingBatchDeliveries, index: usize) {
    if let Some(staged) = owner.staged.as_mut() {
        staged.cursor = staged.cursor.max(index + 1);
    }
    owner.paused_current = None;
    sync_staged_requests(owner);
}

/// Tail-insertion slot for a grouped fixed-position member. `None` keeps the
/// ordinary placement arm for members outside the grouping domain.
fn staged_arrangement_slot(
    state: &GameState,
    owner: &PendingBatchDeliveries,
    member: &StagedBatchMember,
) -> Option<usize> {
    let staged = owner.staged.as_ref()?;
    let Some(ProposedEvent::ZoneChange {
        to: Zone::Library,
        library_placement: Some(position),
        ..
    }) = member.approved_event.as_ref()
    else {
        return None;
    };
    let arrangement = staged.arrangements.iter().find(|arrangement| {
        arrangement.batch.owner == member.owner
            && arrangement.position == *position
            && arrangement
                .batch
                .members
                .iter()
                .any(|ordered| ordered.identity == member.identity)
    })?;
    let rank = arrangement
        .batch
        .members
        .iter()
        .position(|ordered| ordered.identity == member.identity)?;
    let library = state.library_of(arrangement.batch.owner);
    let in_library = library.contains(&member.identity.object_id);
    let len_after_removal = library.len().saturating_sub(usize::from(in_library));
    // CR 400.7: cross-zone arrivals have a new incarnation; a library reorder
    // retains its incarnation and is present only after its own Moved terminal.
    let present = arrangement
        .batch
        .members
        .iter()
        .enumerate()
        .filter_map(|(rank, ordered)| {
            let position = library
                .iter()
                .position(|id| *id == ordered.identity.object_id)?;
            let delivered = if ordered.origin == Zone::Library {
                staged.members.iter().any(|candidate| {
                    candidate.identity == ordered.identity
                        && candidate.state == StagedBatchMemberState::Terminal
                        && candidate.terminal == Some(ZoneMoveCompletion::Moved)
                })
            } else {
                state
                    .objects
                    .get(&ordered.identity.object_id)
                    .is_some_and(|object| object.incarnation != ordered.identity.incarnation)
            };
            delivered.then_some((rank, position))
        });
    match &arrangement.position {
        LibraryPosition::NthFromTop { .. } => {
            // Numbered blocks require a stable contiguous base. Top/Bottom
            // may legally interleave present arrivals with library-origin
            // members still awaiting their reorder (CR 401.4).
            debug_assert!(
                {
                    let (count, first, last) = present.clone().fold(
                        (0usize, usize::MAX, 0usize),
                        |(count, first, last), (_, position)| {
                            (count + 1, first.min(position), last.max(position))
                        },
                    );
                    count == 0 || last - first + 1 == count
                },
                "present arranged members occupy contiguous library positions"
            );
        }
        LibraryPosition::Top
        | LibraryPosition::Bottom
        | LibraryPosition::BeneathTop { .. }
        | LibraryPosition::RandomWithinTop { .. } => {}
    }
    Some(zones::arranged_library_slot(
        len_after_removal,
        present.map(|(rank, _)| rank),
        rank,
        &arrangement.position,
    ))
}

/// CR 603.10a: entry for an admitted batch. The owner is driven
/// detached and parked only if the batch pauses or needs an arrangement prompt.
fn deliver_staged_batch_entry(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    logical_zone_change_group: LogicalZoneChangeGroup,
    arrangements: Vec<StagedLibraryArrangement>,
    component_delivery_parent: Option<PendingComponentDeliveryParent>,
    events: &mut Vec<GameEvent>,
) -> BatchDeliveryResult {
    let arrangements = component_delivery_parent
        .as_ref()
        .map_or(arrangements, |link| link.inherited_arrangements.clone());
    let staged = new_staged_batch(state, &reqs, arrangements);
    let mut owner = new_staged_owner(
        state,
        &reqs,
        logical_zone_change_group,
        staged,
        component_delivery_parent,
    );
    match run_staged_batch(state, &mut owner, events) {
        StagedStop::Paused | StagedStop::Arranging => {
            state
                .resolution_stack
                .park_batch_owner(owner)
                .expect("paused staged batch parks its exact owner");
            BatchDeliveryResult::NeedsChoice
        }
        StagedStop::Delivered => {
            // The caller completes the logical group; record every terminal
            // first so `validate_complete` never sees a Pending slot.
            record_staged_terminals_for_settlement(&mut owner);
            BatchDeliveryResult::Done(Box::new(owner.logical_zone_change_group))
        }
    }
}

/// The outcome of one admitted family entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagedFamilyOutcome {
    Settled,
    Paused,
}

/// CR 701.8a + CR 603.10a: enter one admitted destruction population. On a
/// synchronous delivery the entry settles the whole owner (including its
/// `DestroyPopulationSettled` completion) and returns nothing to a producer
/// tail; on a pause it parks the exact owner with its completion.
pub(crate) fn deliver_staged_destroy_batch(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    logical_zone_change_group: LogicalZoneChangeGroup,
    ability: &crate::types::ability::ResolvedAbility,
    source: ObjectId,
    cant_regenerate: bool,
    events: &mut Vec<GameEvent>,
) -> StagedFamilyOutcome {
    let event_start = events.len();
    let mut staged = new_staged_batch(state, &reqs, Vec::new());
    for member in &mut staged.members {
        member.operation = StagedMemberOperation::Destroy(StagedDestroyOperation {
            source,
            cant_regenerate,
            outer: StagedDestroyOuterApproval::Unapproved,
        });
    }
    let mut owner = new_staged_owner(state, &reqs, logical_zone_change_group, staged, None);
    owner.completion = Some(BatchCompletion::DestroyPopulationSettled {
        ability: Box::new(ability.clone()),
        settled_destroyed: None,
    });
    let boundary = state.resolution_stack.capture_child_boundary();
    match run_staged_batch(state, &mut owner, events) {
        StagedStop::Paused | StagedStop::Arranging => {
            state.repark_batch_owner_below_children(owner, boundary);
            StagedFamilyOutcome::Paused
        }
        StagedStop::Delivered => {
            record_staged_terminals_for_settlement(&mut owner);
            let deferred_events = owner.deferred_events.clone();
            // CR 608.2c + CR 603.2c: settle only this Destroy instruction's
            // newly emitted slice. Settling the shared action buffer would
            // re-collect an earlier modal instruction's ZoneChanged events.
            let mut scoped_events = events[event_start..].to_vec();
            settle_batch_owner(state, owner, deferred_events, &mut scoped_events);
            events.truncate(event_start);
            events.extend(scoped_events);
            StagedFamilyOutcome::Settled
        }
    }
}

/// CR 608.2c + CR 603.10a: the ChangeZone family's synchronous delivery result.
///
/// `Settled` carries the delivered group and its member terminals inline.
/// Synchronous callers consume this result immediately rather than retaining
/// the population, so boxing the payload would add a heap indirection and an
/// allocation per settled move without saving retained state.
#[allow(clippy::large_enum_variant)]
pub(crate) enum StagedChangeZoneOutcome {
    Settled {
        group: LogicalZoneChangeGroup,
        members: Vec<StagedBatchMember>,
    },
    Paused,
}

/// CR 608.2c + CR 603.10a: enter one admitted ChangeZone population. The group
/// is moved by value into the child, which is its single logical owner; on a
/// pause the ChangeZone parent marker is installed immediately below the batch
/// child and the tail rides the batch's typed completion.
pub(crate) fn deliver_staged_change_zone_batch(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    logical_zone_change_group: LogicalZoneChangeGroup,
    tail: crate::types::game_state::ChangeZoneBatchTail,
    events: &mut Vec<GameEvent>,
) -> StagedChangeZoneOutcome {
    let event_start = events.len();
    let group_id = logical_zone_change_group.logical_group_id;
    let mut staged = new_staged_batch(state, &reqs, Vec::new());
    // Each first-run population owns its transient producer-tail anchor.
    state.staged_population_parent = None;
    // CR 614.13a: freeze eligibility before any population member enters.
    staged.population_devour_snapshot = state.install_change_zone_batch_marker(group_id, &reqs);
    let child_stack_start = state.resolution_stack.capture_child_boundary();
    let mut owner = new_staged_owner(state, &reqs, logical_zone_change_group, staged, None);
    owner.completion = Some(BatchCompletion::ChangeZonePopulationSettled {
        tail: Box::new(tail),
        settled: None,
    });
    match run_staged_batch(state, &mut owner, events) {
        StagedStop::Paused | StagedStop::Arranging => {
            state.repark_batch_owner_below_children(owner, child_stack_start);
            state.staged_population_parent =
                Some(crate::types::game_state::StagedPopulationParent {
                    group: group_id,
                    batch_depth: child_stack_start,
                });
            StagedChangeZoneOutcome::Paused
        }
        StagedStop::Delivered => {
            let retired = state.take_change_zone_batch_marker(group_id);
            debug_assert!(
                retired,
                "a synchronously delivered staged ChangeZone retires its marker"
            );
            record_staged_terminals_for_settlement(&mut owner);
            let mut group = owner.logical_zone_change_group;
            let members = owner
                .staged
                .as_ref()
                .map(|staged| staged.members.clone())
                .unwrap_or_default();
            crate::game::triggers::complete_logical_zone_trigger_collection(
                state,
                &mut group,
                &mut events[event_start..],
            )
            .expect("completed ChangeZone owns every terminal member outcome");
            crate::game::triggers::mark_logical_zone_events_consumed_before_priority(
                state,
                &group,
                &events[event_start..],
                crate::game::triggers::ConsumedTriggerEventScope::OrdinaryCollectorsOnly,
            );
            StagedChangeZoneOutcome::Settled { group, members }
        }
    }
}

/// CR 616.1: identify the staged member whose replacement prompt is
/// parked. The owner key is the exact parked boundary — not a live-object
/// lookup — so a modified proposal or a changed incarnation cannot be consumed
/// as this member's approval. Returns the group and whether the member still
/// has no retained outcome.
pub(crate) fn staged_approval_owner(
    state: &GameState,
    parked: &PendingZoneChangeDelivery,
) -> Option<(LogicalZoneChangeGroupId, bool)> {
    staged_approval_owner_for(state, parked.member, &parked.expected_event)
}

/// CR 616.1: the exact parked boundary a staged consultation owns,
/// for either an outer `Destroy` proposal or a zone change.
pub(crate) fn staged_approval_owner_for(
    state: &GameState,
    identity: ObjectIncarnationRef,
    expected_event: &ProposedEvent,
) -> Option<(LogicalZoneChangeGroupId, bool)> {
    let batch = state.active_batch_delivery()?;
    let staged = batch.staged.as_ref()?;
    if staged.stage != BatchDeliveryStage::Approving {
        return None;
    }
    let member = staged.members.get(staged.cursor)?;
    let paused = batch.paused_current.as_ref()?;
    if paused.terminal_completion.is_some()
        || !paused.captures(identity, expected_event)
        || identity != member.identity
    {
        return None;
    }
    let compatible = match (&member.operation, expected_event) {
        (StagedMemberOperation::ZoneMove, ProposedEvent::ZoneChange { .. }) => true,
        (StagedMemberOperation::Destroy(_), ProposedEvent::Destroy { .. }) => {
            member.state == StagedBatchMemberState::OuterConsulting
        }
        (StagedMemberOperation::Destroy(_), ProposedEvent::ZoneChange { .. }) => {
            member.state == StagedBatchMemberState::InnerConsulting
        }
        _ => false,
    };
    if !compatible {
        return None;
    }
    Some((
        batch.logical_zone_change_group.logical_group_id,
        member.pending_outcome.is_none(),
    ))
}

/// Whether the staged owner still has work before the shared Execute epilogue.
pub(crate) enum StagedConsultationHandback {
    NotOwned,
    Parked(Box<WaitingFor>),
    Settled,
}

/// CR 614.6 + CR 616.1: retain one staged consultation result, then continue
/// approval and delivery for its whole population. Report settlement only when
/// the completion left no live prompt for the shared Execute epilogue to preserve.
pub(crate) fn record_staged_batch_consultation_result(
    state: &mut GameState,
    group_id: LogicalZoneChangeGroupId,
    outcome: StagedMemberOutcome,
    events: &mut Vec<GameEvent>,
) -> StagedConsultationHandback {
    // CR 616.1: detach the approved drain before checking for its batch owner.
    let mut retained = if matches!(outcome, StagedMemberOutcome::Approved(_)) {
        state
            .resolution_stack
            .take_ready_post_replacement_above_batch(group_id)
    } else {
        None
    };
    if state
        .active_batch_delivery()
        .is_none_or(|pending| pending.logical_zone_change_group.logical_group_id != group_id)
    {
        if let Some(drain) = retained.take() {
            state.resolution_stack.push_post_replacement(drain);
        }
        return StagedConsultationHandback::NotOwned;
    }
    let Some(mut owner) = state
        .resolution_stack
        .take_active_batch_delivery()
        .ok()
        .flatten()
    else {
        if let Some(drain) = retained.take() {
            state.resolution_stack.push_post_replacement(drain);
        }
        return StagedConsultationHandback::NotOwned;
    };
    let boundary = state.resolution_stack.capture_child_boundary();
    let live_child = state.pending_replacement.is_some()
        || !matches!(
            state.waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::ReplacementChoice { .. }
        );
    let refuse =
        |state: &mut GameState,
         owner: PendingBatchDeliveries,
         retained: Option<crate::types::game_state::PostReplacementDrainStack>| {
            state.repark_batch_owner_below_children(owner, boundary);
            if let Some(drain) = retained {
                state.resolution_stack.push_post_replacement(drain);
            }
        };
    let approving = owner
        .staged
        .as_ref()
        .is_some_and(|staged| staged.stage == BatchDeliveryStage::Approving);
    if !approving {
        refuse(state, owner, retained);
        return StagedConsultationHandback::NotOwned;
    }
    let staged = owner.staged.as_mut().expect("checked staged");
    let cursor = staged.cursor;
    let Some(member) = staged.members.get_mut(cursor) else {
        refuse(state, owner, retained);
        return StagedConsultationHandback::NotOwned;
    };
    if member.pending_outcome.is_some() {
        let waiting = state.waiting_for.clone();
        refuse(state, owner, retained);
        return StagedConsultationHandback::Parked(Box::new(waiting));
    }
    member.pending_outcome = Some(outcome.clone());
    if live_child {
        if let Some(drain) = retained.take() {
            attach_member_drain(&mut owner, cursor, drain);
        }
        if matches!(outcome, StagedMemberOutcome::Prevented) {
            if let Some(member) = owner
                .staged
                .as_mut()
                .and_then(|staged| staged.members.get_mut(cursor))
            {
                member.state = StagedBatchMemberState::PreventedWithChild;
            }
        }
        let waiting = state.waiting_for.clone();
        state.repark_batch_owner_below_children(owner, boundary);
        return StagedConsultationHandback::Parked(Box::new(waiting));
    }
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    if !finalize_staged_pending_outcome_on(state, &mut owner, events) {
        refuse(state, owner, retained);
        return StagedConsultationHandback::Parked(Box::new(state.waiting_for.clone()));
    }
    if let Some(drain) = retained.take() {
        attach_member_drain(&mut owner, cursor, drain);
    }
    let stop = run_staged_batch(state, &mut owner, events);
    if stop == StagedStop::Delivered {
        let deferred_events = owner.deferred_events.clone();
        settle_batch_owner(state, owner, deferred_events, events);
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            StagedConsultationHandback::Settled
        } else {
            StagedConsultationHandback::Parked(Box::new(state.waiting_for.clone()))
        }
    } else {
        state.repark_batch_owner_below_children(owner, boundary);
        StagedConsultationHandback::Parked(Box::new(state.waiting_for.clone()))
    }
}

/// The only child-settlement entry for Approving. Moves the retained outcome
/// into the cursor member once no child or replacement is live.
fn finalize_staged_pending_outcome_on(
    state: &mut GameState,
    owner: &mut PendingBatchDeliveries,
    events: &mut Vec<GameEvent>,
) -> bool {
    if state.pending_replacement.is_some()
        || !matches!(state.waiting_for, WaitingFor::Priority { .. })
    {
        return false;
    }
    let Some(staged) = owner.staged.as_mut() else {
        return false;
    };
    if staged.stage != BatchDeliveryStage::Approving {
        return false;
    }
    let cursor = staged.cursor;
    let Some(member) = staged.members.get_mut(cursor) else {
        return false;
    };
    let Some(outcome) = member.pending_outcome.clone() else {
        return false;
    };
    match (member.state, outcome) {
        (
            StagedBatchMemberState::OuterConsulting,
            StagedMemberOutcome::Approved(ProposedEvent::Destroy { .. }),
        ) => {
            // CR 701.8a: outer approval, not delivery. Keep the cursor here and
            // let the driver run the second, inner consult.
            member.state = StagedBatchMemberState::InnerConsulting;
            member.pending_outcome = None;
            approve_staged_destroy_outer(owner, cursor);
            owner.paused_current = None;
            sync_staged_requests(owner);
            true
        }
        (
            StagedBatchMemberState::OuterConsulting
            | StagedBatchMemberState::InnerConsulting
            | StagedBatchMemberState::PreventedWithChild,
            StagedMemberOutcome::Approved(event),
        ) => {
            member.approved_event = Some(event);
            member.state = StagedBatchMemberState::Approved;
            member.pending_outcome = None;
            staged.cursor = cursor + 1;
            owner.paused_current = None;
            sync_staged_requests(owner);
            true
        }
        (
            StagedBatchMemberState::OuterConsulting
            | StagedBatchMemberState::InnerConsulting
            | StagedBatchMemberState::PreventedWithChild,
            StagedMemberOutcome::Prevented,
        ) => {
            let destroyed = match member.operation {
                StagedMemberOperation::Destroy(StagedDestroyOperation {
                    source,
                    outer: StagedDestroyOuterApproval::Approved,
                    ..
                }) => Some((member.identity.object_id, source)),
                _ => None,
            };
            member.approved_post_replacement = None;
            member.terminal = Some(ZoneMoveCompletion::Prevented);
            member.state = StagedBatchMemberState::Terminal;
            member.pending_outcome = None;
            staged.cursor = cursor + 1;
            owner.paused_current = None;
            sync_staged_requests(owner);
            if let Some((object_id, source_id)) = destroyed {
                // CR 701.8a: an inner prevention after outer approval still
                // emits `CreatureDestroyed` once.
                events.push(GameEvent::CreatureDestroyed {
                    object_id,
                    source_id: Some(source_id),
                });
            }
            true
        }
        _ => false,
    }
}

/// Test/diagnostic wrapper: finalize the top staged owner in place.
#[cfg(test)]
pub(crate) fn finalize_staged_pending_outcome(
    state: &mut GameState,
    group_id: LogicalZoneChangeGroupId,
) -> bool {
    if state
        .active_batch_delivery()
        .is_none_or(|pending| pending.logical_zone_change_group.logical_group_id != group_id)
    {
        return false;
    }
    let Ok(Some(mut owner)) = state.resolution_stack.take_active_batch_delivery() else {
        return false;
    };
    let finalized = finalize_staged_pending_outcome_on(state, &mut owner, &mut Vec::new());
    state
        .resolution_stack
        .park_batch_owner(owner)
        .expect("staged batch re-parks its exact owner");
    finalized
}

/// CR 401.4 + CR 401.2: validate and store one batch-owned arrangement
/// permutation, then open the next prompt or deliver synchronously.
///
/// The eight parameters are all distinct semantic axes of one submission
/// (owner, member set, prefix count, split scope, arrangement, event sink).
/// Grouping any pair would fabricate a transient struct used only here and
/// hide a real axis at the call site.
#[allow(clippy::too_many_arguments)]
pub(crate) fn submit_staged_batch_arrangement(
    state: &mut GameState,
    group_id: LogicalZoneChangeGroupId,
    library_owner: PlayerId,
    cards: &[ObjectId],
    top_count: usize,
    scope: DigRestSplitScope,
    arrangement: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    if scope != DigRestSplitScope::OrderOnly {
        return Err("batch-owned arrangement must be answered by the library owner".to_string());
    }
    if top_count != 0 && top_count != arrangement.len() {
        return Err("batch-owned arrangement mixes Top and Bottom positions".to_string());
    }
    if arrangement.len() != cards.len() {
        return Err("batch-owned arrangement must list exactly the offered cards".to_string());
    }
    let offered: HashSet<ObjectId> = cards.iter().copied().collect();
    if offered.len() != cards.len() {
        return Err("batch-owned arrangement offer contains a duplicate card".to_string());
    }
    let mut seen = HashSet::new();
    for id in arrangement {
        if !seen.insert(*id) || !offered.contains(id) {
            return Err("batch-owned arrangement is not a permutation of the offer".to_string());
        }
    }
    if state
        .active_batch_delivery()
        .is_none_or(|pending| pending.logical_zone_change_group.logical_group_id != group_id)
    {
        return Err("batch-owned arrangement lost its staged batch".to_string());
    }
    let mut owner = state
        .resolution_stack
        .take_active_batch_delivery()
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| "batch-owned arrangement lost its staged frame".to_string())?;
    let boundary = state.resolution_stack.capture_child_boundary();
    let validated: Result<(usize, Vec<MassLibraryOrderMember>, bool), String> = (|| {
        let staged = owner
            .staged
            .as_ref()
            .ok_or_else(|| "batch-owned arrangement names an unstaged batch".to_string())?;
        if staged.stage != BatchDeliveryStage::Arranging {
            return Err("batch-owned arrangement is not taking submissions".to_string());
        }
        let index = staged.arrangement_cursor;
        let group = staged
            .arrangements
            .get(index)
            .ok_or_else(|| "batch-owned arrangement lost its cursor group".to_string())?;
        if group.status != StagedArrangementStatus::Offered {
            return Err("batch-owned arrangement group is not the live prompt".to_string());
        }
        if group.batch.owner != library_owner {
            return Err("batch-owned arrangement names another library owner".to_string());
        }
        if group
            .batch
            .members
            .iter()
            .map(|member| member.identity.object_id)
            .ne(cards.iter().copied())
        {
            return Err("batch-owned arrangement offer no longer matches the group".to_string());
        }
        for ordered in &group.batch.members {
            let live = state
                .objects
                .get(&ordered.identity.object_id)
                .map(ObjectIncarnationRef::from_object);
            if live != Some(ordered.identity) {
                return Err("batch-owned arrangement rebinds a stale incarnation".to_string());
            }
        }
        let mut submitted: Vec<MassLibraryOrderMember> = Vec::with_capacity(arrangement.len());
        for id in arrangement {
            let ordered = group
                .batch
                .members
                .iter()
                .find(|member| member.identity.object_id == *id)
                .expect("validated permutation member");
            submitted.push(ordered.clone());
        }
        let has_next = staged
            .arrangements
            .iter()
            .enumerate()
            .any(|(position, group)| {
                position > index && group.status == StagedArrangementStatus::Awaiting
            });
        Ok((index, submitted, has_next))
    })();
    let (index, submitted, has_next) = match validated {
        Ok(validated) => validated,
        Err(message) => {
            state.repark_batch_owner_below_children(owner, boundary);
            return Err(message);
        }
    };
    {
        let staged = owner.staged.as_mut().expect("validated staged payload");
        let group = staged
            .arrangements
            .get_mut(index)
            .expect("validated cursor group");
        group.batch.members = submitted;
        group.status = StagedArrangementStatus::Submitted;
        staged.arrangement_cursor = index + 1;
        if !has_next {
            staged.stage = BatchDeliveryStage::Delivering;
        }
    }
    if has_next {
        let _ = publish_next_staged_arrangement(state, &mut owner);
        state.repark_batch_owner_below_children(owner, boundary);
        return Ok(());
    }
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    match run_staged_batch(state, &mut owner, events) {
        StagedStop::Delivered => {
            let deferred_events = owner.deferred_events.clone();
            settle_batch_owner(state, owner, deferred_events, events);
        }
        StagedStop::Paused | StagedStop::Arranging => {
            state.repark_batch_owner_below_children(owner, boundary);
        }
    }
    Ok(())
}

/// CR 800.4a: prune one abandoned staged member from the exact membership,
/// the prompt groups and the legacy compatibility projection. The frozen
/// announcement vector keeps its indices; an abandoned member is neither
/// consulted nor delivered.
pub(crate) fn prune_staged_member_for_player_left(
    pending: &mut PendingBatchDeliveries,
    identity: ObjectIncarnationRef,
) {
    let Some(staged) = pending.staged.as_mut() else {
        return;
    };
    let Some(index) = staged
        .members
        .iter()
        .position(|member| member.identity == identity)
    else {
        return;
    };
    let owner = &mut staged.members[index];
    owner.approved_post_replacement = None;
    owner.state = StagedBatchMemberState::Abandoned;
    owner.approved_event = None;
    owner.pending_outcome = None;
    owner.terminal = None;
    if index == staged.cursor {
        staged.cursor += 1;
    }
    let mut arrangements = std::mem::take(&mut staged.arrangements);
    for arrangement in &mut arrangements {
        arrangement.batch.members.retain(|member| {
            member.identity != identity && member.departing_permanent != Some(identity)
        });
        if arrangement.batch.members.len() < 2
            && matches!(
                arrangement.status,
                StagedArrangementStatus::Awaiting | StagedArrangementStatus::Offered
            )
        {
            arrangement.status = StagedArrangementStatus::Derived;
        }
    }
    arrangements.retain(|arrangement| !arrangement.batch.members.is_empty());
    staged.arrangements = arrangements;
    staged.arrangement_cursor = staged
        .arrangements
        .iter()
        .position(|arrangement| {
            !matches!(
                arrangement.status,
                StagedArrangementStatus::Derived | StagedArrangementStatus::Submitted
            )
        })
        .unwrap_or(staged.arrangements.len());
    sync_staged_requests(pending);
}

/// CR 800.4a: reconcile a staged batch after every departure sweep
/// and before the generic dead-actor repoint. Surviving groups advance, a
/// batch-owned prompt whose actor or group is gone is cleared, and a batch with
/// nothing left to deliver settles and runs its completion once. This seam also
/// reaches a staged owner buried below a top `PostReplacement` child; that case
/// mutates the owner in place (the child's own resume drains it) rather than
/// popping a frame it does not own.
/// A retired replacement consultation skips its member, preserving main's
/// departure behavior; FU-7/#341 tracks reconsulting the surviving controller.
pub(crate) fn reconcile_staged_batch_after_departure(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<crate::game::engine_resolution_choices::DepartureSettlement, String> {
    // Capture exactly which staged frame owns this action — its
    // logical group and, when it is a linked component batch, the parent
    // boundary it captured. Every mutation below names that same frame; none of
    // them re-derives the owner from a census or from the nearest staged frame.
    let Some(identity) = state.resolution_stack.staged_owner_identity() else {
        return Ok(crate::game::engine_resolution_choices::DepartureSettlement::NotSettled);
    };
    let is_active_owner = state.active_batch_delivery().is_some_and(|pending| {
        pending.logical_zone_change_group.logical_group_id == identity.group
            && pending.component_delivery_parent == identity.component
    });
    if is_active_owner {
        let Ok(Some(mut owner)) = state.resolution_stack.take_active_batch_delivery() else {
            return Ok(crate::game::engine_resolution_choices::DepartureSettlement::NotSettled);
        };
        let boundary = state.resolution_stack.capture_child_boundary();
        // CR 800.4a: reconciliation may deliver surviving batch
        // members. If it settles, give settlement only that delivery segment,
        // not the departure sweep; otherwise append the segment and re-park.
        let mut batch_events = Vec::new();
        // CR 800.4a + CR 616.1: departure can retire the chooser's pending
        // replacement without retiring this surviving member's consultation.
        // Only the active owner with no live child may skip that member.
        let retired_prompt = match state.waiting_for {
            WaitingFor::Priority { .. } => true,
            WaitingFor::ReplacementChoice { player, .. } => {
                !crate::game::players::is_alive(state, player)
            }
            _ => false,
        };
        if state.pending_replacement.is_none() && retired_prompt {
            let retired_cursor = owner.staged.as_mut().and_then(|staged| {
                if staged.stage != BatchDeliveryStage::Approving {
                    return None;
                }
                let member = staged.members.get_mut(staged.cursor)?;
                if member.pending_outcome.is_some()
                    || !matches!(
                        member.state,
                        StagedBatchMemberState::OuterConsulting
                            | StagedBatchMemberState::InnerConsulting
                    )
                {
                    return None;
                }
                if let StagedMemberOperation::Destroy(operation) = &mut member.operation {
                    operation.outer = StagedDestroyOuterApproval::Unapproved;
                }
                Some(staged.cursor)
            });
            if let Some(cursor) = retired_cursor {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                mark_staged_member_terminal(
                    state,
                    &mut owner,
                    cursor,
                    ZoneMoveCompletion::Remained,
                    &mut batch_events,
                );
            }
        }
        if reconcile_staged_owner(state, &mut owner, identity.group, &mut batch_events) {
            let deferred_events = std::mem::take(&mut owner.deferred_events);
            settle_batch_owner(state, owner, deferred_events, &mut batch_events);
            events.append(&mut batch_events);
            // CR 608.2m: the departure completed this instruction through the
            // typed settlement, so the resolving spell keeps its owed final
            // part instead of being retired as an unanswered question.
            return Ok(crate::game::engine_resolution_choices::DepartureSettlement::Settled);
        }
        events.append(&mut batch_events);
        state.repark_batch_owner_below_children(owner, boundary);
        return Ok(crate::game::engine_resolution_choices::DepartureSettlement::NotSettled);
    }

    // The staged owner may be buried below a top PostReplacement delivery child.
    let mut buried = None;
    if !state
        .resolution_stack
        .visit_staged_batch_owner_mut(&identity, |pending| buried = Some(pending.clone()))
    {
        return Ok(crate::game::engine_resolution_choices::DepartureSettlement::NotSettled);
    }
    let Some(mut owner) = buried else {
        return Ok(crate::game::engine_resolution_choices::DepartureSettlement::NotSettled);
    };
    let settle = reconcile_staged_owner(state, &mut owner, identity.group, events);
    // A buried owner cannot be popped here: the child frame owns the stack.
    // All abandoned slots were already recorded by the departure sweep, so the
    // batch is coherent; the child's resume reaches `BatchDelivery` and settles
    // it through the ordinary drain.
    let _ = settle;
    state
        .resolution_stack
        .visit_staged_batch_owner_mut(&identity, |pending| *pending = owner.clone());
    // A buried owner is not settled here: the child frame owns the stack, so
    // its own resume performs the settlement and the owed final part is still
    // outstanding for the ordinary convergence.
    Ok(crate::game::engine_resolution_choices::DepartureSettlement::NotSettled)
}

/// Shared reconcile of one staged owner (top or buried). Returns whether the
/// batch has nothing left and must settle.
fn reconcile_staged_owner(
    state: &mut GameState,
    owner: &mut PendingBatchDeliveries,
    group_id: LogicalZoneChangeGroupId,
    events: &mut Vec<GameEvent>,
) -> bool {
    let prompt_group = match &state.waiting_for {
        WaitingFor::DigRestSplitChoice {
            completion: Some(LibraryPlacementContinuation::ApprovedBatch { logical_group_id }),
            ..
        } => Some(*logical_group_id),
        _ => None,
    };
    if prompt_group == Some(group_id) {
        let actor_alive = matches!(
            &state.waiting_for,
            WaitingFor::DigRestSplitChoice { player, .. }
                if crate::game::players::is_alive(state, *player)
        );
        let group_still_valid = owner.staged.as_ref().is_some_and(|staged| {
            staged.stage == BatchDeliveryStage::Arranging
                && staged
                    .arrangements
                    .get(staged.arrangement_cursor)
                    .is_some_and(|arrangement| {
                        arrangement.status == StagedArrangementStatus::Offered
                    })
        });
        if actor_alive && group_still_valid {
            return false;
        }
        state.waiting_for = WaitingFor::Priority {
            player: state.active_player,
        };
    }

    let live = owner.staged.as_ref().is_some_and(|staged| {
        staged.members.iter().any(|member| {
            matches!(
                member.state,
                StagedBatchMemberState::Unconsulted
                    | StagedBatchMemberState::OuterConsulting
                    | StagedBatchMemberState::InnerConsulting
                    | StagedBatchMemberState::Approved
                    | StagedBatchMemberState::PreventedWithChild
            )
        })
    });
    if !live {
        return true;
    }
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        return false;
    }
    run_staged_batch(state, owner, events) == StagedStop::Delivered
}

/// Record every staged member's terminal into its logical slot before the
/// group's final collection. `Moved` is recorded by `append_delivery_events`
/// from the actual `ZoneChanged`; a slot that already carries a terminal (a
/// resumed delivery pause) is never recorded twice.
fn record_staged_terminals_for_settlement(pending: &mut PendingBatchDeliveries) {
    let Some(staged) = pending.staged.as_ref() else {
        return;
    };
    let terminals: Vec<(ObjectIncarnationRef, ZoneMoveCompletion)> = staged
        .members
        .iter()
        .filter_map(|member| member.terminal.map(|terminal| (member.identity, terminal)))
        .collect();
    for (identity, terminal) in terminals {
        let Some(index) = pending
            .logical_zone_change_group
            .prospective_battlefield_members
            .iter()
            .position(|candidate| candidate.identity == identity)
        else {
            continue;
        };
        if matches!(
            pending.logical_zone_change_group.terminal_outcomes[index],
            crate::types::game_state::LogicalZoneChangeTerminalOutcome::Pending
        ) {
            pending
                .logical_zone_change_group
                .record_delivery_completion(identity.object_id, terminal)
                .expect("staged settlement records a pending terminal");
        }
    }
}

/// CR 603.10a + CR 616.1: settle one batch owner exactly once. Extracted from
/// the drain's `Done` body so the staged and legacy paths share it.
fn settle_batch_owner(
    state: &mut GameState,
    mut pending: PendingBatchDeliveries,
    mut deferred_events: Vec<GameEvent>,
    events: &mut Vec<GameEvent>,
) {
    let component_delivery_parent = pending.component_delivery_parent.clone();
    let paused_current = pending.paused_current.clone();
    let completion = pending.completion.clone();
    record_staged_terminals_for_settlement(&mut pending);
    crate::game::triggers::complete_logical_zone_trigger_collection(
        state,
        &mut pending.logical_zone_change_group,
        events,
    )
    .expect("completed staged batch owns every terminal zone-change outcome");
    crate::game::triggers::sync_logical_zone_change_departure_stamps(
        &pending.logical_zone_change_group,
        &mut deferred_events,
    );
    deferred_events.append(events);
    events.append(&mut deferred_events);
    // CR 603.7b: staged population settlement collects ordinary triggers only;
    // delayed triggers must still see the population's departure occurrences.
    let consumed_scope = match completion.as_ref() {
        Some(
            BatchCompletion::ChangeZonePopulationSettled { .. }
            | BatchCompletion::DestroyPopulationSettled { .. },
        ) => crate::game::triggers::ConsumedTriggerEventScope::OrdinaryCollectorsOnly,
        None
        | Some(
            BatchCompletion::RecordInstructionZoneResult { .. }
            | BatchCompletion::ReturnAsAuraNoTargetComplete { .. }
            | BatchCompletion::ExploreLandDeliveryComplete { .. }
            | BatchCompletion::CloakExileDeliveryComplete { .. }
            | BatchCompletion::ExileFaceDownPileDeliveryComplete { .. }
            | BatchCompletion::ExileFaceDownPileReturnComplete { .. }
            | BatchCompletion::CastFromZoneExileDeliveryComplete { .. }
            | BatchCompletion::CascadeExileLoopComplete { .. }
            | BatchCompletion::TopCardContestRoundComplete { .. }
            | BatchCompletion::CascadeBottomComplete { .. }
            | BatchCompletion::RippleTerminalComplete { .. }
            | BatchCompletion::DiscoverBottomComplete { .. }
            | BatchCompletion::DiscoverPlacementComplete { .. }
            | BatchCompletion::DiscoverDeclined { .. }
            | BatchCompletion::ResolutionCastRejectedToHand { .. }
            | BatchCompletion::DiscoverDeclinedComplete { .. }
            | BatchCompletion::ResolutionCastRejectionComplete { .. }
            | BatchCompletion::PutOnTopComplete { .. }
            | BatchCompletion::TopOrBottomComplete { .. }
            | BatchCompletion::EffectZonePutAtLibraryPositionComplete { .. }
            | BatchCompletion::ForEachCategoryExileComplete { .. }
            | BatchCompletion::DrawnThisTurnTopdeckComplete { .. }
            | BatchCompletion::SurveilKeepOnTop { .. }
            | BatchCompletion::ManifestDreadCleanup { .. }
            | BatchCompletion::RevealRestPile { .. }
            | BatchCompletion::DigMassPutAllRestComplete { .. }
            | BatchCompletion::DigMassPutAllComplete { .. }
            | BatchCompletion::DigPriorLookRestComplete { .. }
            | BatchCompletion::RemoveExileLinks { .. }
            | BatchCompletion::NinjutsuPlacement { .. }
            | BatchCompletion::AttractionOpenRemainder { .. }
            | BatchCompletion::ContraptionAssembleRemainder { .. }
            | BatchCompletion::LibrarySearchDeliverySettled { .. }
            | BatchCompletion::SearchPartitionPrimaryDelivered { .. }
            | BatchCompletion::SearchFoundZoneDelivery { .. }
            | BatchCompletion::MeldExile { .. }
            | BatchCompletion::MeldEntry { .. }
            | BatchCompletion::MeldRedirect { .. }
            | BatchCompletion::MilledDeliveryComplete { .. },
        ) => crate::game::triggers::ConsumedTriggerEventScope::AllCollectors,
    };
    crate::game::triggers::mark_logical_zone_events_consumed_before_priority(
        state,
        &pending.logical_zone_change_group,
        events,
        consumed_scope,
    );
    if let Some(link) = component_delivery_parent.as_ref() {
        let parent = state
            .active_batch_delivery()
            .expect("component child reveals its original owner");
        assert_eq!(
            parent.logical_zone_change_group.logical_group_id,
            link.parent_group_id
        );
        assert!(parent
            .paused_current
            .as_ref()
            .is_some_and(|paused| paused.captures(link.member, &link.expected_event)
                && paused.terminal_completion.is_none()));
        let index = pending
            .logical_zone_change_group
            .prospective_battlefield_members
            .iter()
            .position(|member| member.identity == link.member)
            .expect("component group retains original incarnation");
        let terminal = match pending.logical_zone_change_group.terminal_outcomes[index] {
            LogicalZoneChangeTerminalOutcome::Moved { .. } => ZoneMoveCompletion::Moved,
            LogicalZoneChangeTerminalOutcome::Prevented => ZoneMoveCompletion::Prevented,
            LogicalZoneChangeTerminalOutcome::Remained => ZoneMoveCompletion::Remained,
            LogicalZoneChangeTerminalOutcome::Pending
            | LogicalZoneChangeTerminalOutcome::AbandonedByPlayerLeft => {
                panic!("settled component handback requires a terminal original delivery")
            }
        };
        assert!(state.capture_paused_zone_change_delivery(
            link.member,
            &link.expected_event,
            events,
            terminal
        ));
        crate::game::effects::settle_replaced_forwarded_zone_delivery(state, link.member, events);
    }
    if let (Some(_), Some(audiences)) = (
        paused_current.as_ref(),
        completion
            .as_ref()
            .and_then(|completion| completion.hidden_search_audiences()),
    ) {
        for entry in audiences {
            state.record_completed_hidden_search_audience(entry.identity, &entry.audience);
        }
    }
    let staged_members: Vec<StagedBatchMember> = pending
        .staged
        .as_ref()
        .map(|staged| staged.members.clone())
        .unwrap_or_default();
    // CR 608.2c: the typed completion IS the family authority — a
    // `ChangeZonePopulationSettled` tail is carried only by the ChangeZone
    // family, and every such batch carries one.
    let change_zone_family = matches!(
        completion.as_ref(),
        Some(crate::types::game_state::BatchCompletion::ChangeZonePopulationSettled { .. })
    );
    if let Some(mut completion) = completion {
        crate::types::game_state::settle_batch_delivery_outcome(
            &mut completion,
            state,
            &pending.logical_zone_change_group,
            &staged_members,
        );
        let _ = run_batch_completion(state, completion, events);
    }
    if change_zone_family {
        // CR 608.2c + CR 400.7j: the ChangeZone completion retires the parent
        // marker once, then binds the forwarded result against this exact
        // logical group's arrivals.
        let group_id = pending.logical_zone_change_group.logical_group_id;
        let _ = state.take_change_zone_batch_marker(group_id);
        crate::game::effects::settle_forwarded_zone_result(
            state,
            &pending.logical_zone_change_group,
        );
    }
    if component_delivery_parent.is_some()
        && matches!(state.waiting_for, WaitingFor::Priority { .. })
    {
        drain_pending_batch_deliveries(state, events);
    }
}

/// CR 603.10a + CR 616.1: Park the undelivered batch tail so the resume path
/// can finish it. New saves serialize every request's complete heterogeneous
/// context. The legacy uniform projection remains populated for old-save wire
/// compatibility but is not authoritative for newly parked actions.
fn stash_batch_tail(
    state: &mut GameState,
    logical_zone_change_group: LogicalZoneChangeGroup,
    tail: Vec<ZoneMoveRequest>,
    destination: Zone,
    paused_current: Option<PendingZoneChangeDelivery>,
    parking: BatchDeliveryParking,
) {
    let source_id = tail
        .first()
        .and_then(|first| first.source().filter(|&source| source != first.object_id));
    let enter_tapped = tail
        .first()
        .map_or(EtbTapState::Unspecified, |first| first.mods.enter_tapped);
    let exile_tracking = tail
        .first()
        .map_or(ZoneDeliveryExileTracking::None, |first| {
            first.exile_links.tracking
        });
    let library_placement = tail.first().and_then(|first| first.placement.clone());
    let replacement_applied = tail
        .first()
        .map_or_else(HashSet::new, |first| first.replacement_applied.clone());
    let remaining = tail.iter().map(|request| request.object_id).collect();
    let requests = tail
        .into_iter()
        .map(ZoneMoveRequest::into_pending)
        .collect();
    let pending = PendingBatchDeliveries {
        component_delivery_parent: None,
        logical_zone_change_group,
        paused_current,
        remaining,
        destination,
        source_id,
        enter_tapped,
        exile_tracking,
        library_placement,
        replacement_applied,
        // The post-loop cleanup (if any) is attached by the batch caller after
        // it observes the `NeedsChoice`; `move_objects_simultaneously` itself
        // has no completion to stash.
        completion: None,
        requests,
        attempted: Vec::new(),
        zone_change_record_start: state.zone_changes_this_turn.len(),
        deferred_events: Vec::new(),
        staged: None,
    };
    match parking {
        BatchDeliveryParking::NewChild => state
            .resolution_stack
            .park_batch_owner(pending)
            .expect("new batch parks below its explicitly delegated child"),
        BatchDeliveryParking::ReparkActive => state
            .resolution_stack
            .park_batch_owner(pending)
            .expect("re-paused batch delivery must own the active frame"),
    }
}

/// Captures the pre-delivery identity and the proposed event a batch member is
/// about to attempt. Replacement pauses overwrite this with the authoritative
/// parked `PendingReplacement` event; Aura/copy-target pauses retain this
/// request-derived event because their prompt is surfaced after delivery.
fn anticipated_zone_change_delivery(
    state: &GameState,
    request: &ZoneMoveRequest,
) -> Option<PendingZoneChangeDelivery> {
    let object = state.objects.get(&request.object_id)?;
    let mut expected_event =
        ProposedEvent::zone_change(request.object_id, object.zone, request.to, request.source());
    if let ProposedEvent::ZoneChange {
        enter_tapped,
        enters_attacking,
        enter_transformed,
        controller_override,
        enter_with_counters,
        face_down_profile,
        face_down_in_exile,
        chain_referent,
        attach_to,
        performed_by,
        applied,
        ..
    } = &mut expected_event
    {
        *performed_by = request.mods.performed_by;
        *enter_tapped = request.mods.enter_tapped;
        *enters_attacking = request.mods.enters_attacking;
        *enter_transformed = request.mods.enter_transformed;
        *controller_override = request.mods.controller_override;
        *enter_with_counters = request.mods.enter_with_counters.clone();
        *face_down_profile = request.mods.face_down_profile.clone().map(Box::new);
        *face_down_in_exile = request.face_down_in_exile;
        *chain_referent = request.mods.chain_referent;
        *attach_to = request.mods.attach_to;
        *applied = request.replacement_applied.clone();
    }
    Some(PendingZoneChangeDelivery::new(
        crate::types::identifiers::ObjectIncarnationRef::from_object(object),
        expected_event,
    ))
}

/// CR 603.10a + CR 616.1: Resume a parked batch-delivery tail after the
/// per-object replacement choice that paused it resolved (and its object's
/// chosen event delivered). Re-parks — leaving `state.waiting_for` set — when
/// the next object surfaces its own prompt. Rebuilds each tail request from its
/// exact serialized context so heterogeneous destinations, causes, entry mods,
/// exile links, and placements all match the original action.
///
/// RE-PAUSE CONTRACT (the explicit guarantee for "a LATER item in the same batch
/// parks after the first one already parked and was resumed"): everything a batch
/// needs to finish identically across an arbitrary number of sequential parks is
/// held in the active `BatchDelivery` frame — not in the resuming caller — so
/// each park can replace that exact owner for the next one:
///   * the **undelivered tail** (`remaining`) — `deliver_batch` re-stashes the
///     still-undelivered suffix on every re-park, so no object is ever dropped;
///   * the **exact request context** (`requests`) — every undelivered request
///     retains its own destination, cause, entry mods, placement, exile links,
///     and applied replacements;
///   * the **post-loop `completion`** — taken out here, then re-attached via
///     `ensure_batch_record` on the `NeedsChoice` arm so it survives the second
///     pause boundary and still runs EXACTLY ONCE, the moment the final tail
///     empties (never early, never twice).
///
/// Because all of this lives on the parked record (not in `route_rest_partition`
/// or any synchronous caller frame), a second, third, … park is just another
/// `deliver_batch` → re-stash cycle. The contract is pinned by
/// `mill_double_redirect_choice_continuation` (two sequential parks, no
/// completion) and `surveil_rest_pile_redirect_continuation` (two sequential
/// parks WITH a completion that must fire once after the second park drains).
pub(crate) fn drain_pending_batch_deliveries(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let Some(pending) = state.active_batch_delivery().cloned() else {
        return;
    };
    if pending.staged.is_some() {
        drain_staged_batch(state, pending, events);
        return;
    }
    let PendingBatchDeliveries {
        component_delivery_parent,
        mut logical_zone_change_group,
        paused_current,
        remaining,
        destination,
        source_id,
        enter_tapped,
        exile_tracking,
        library_placement,
        completion,
        replacement_applied,
        requests,
        attempted,
        zone_change_record_start,
        mut deferred_events,
        staged: _,
    } = pending;
    deferred_events.append(events);
    let attempted = if attempted.is_empty() {
        remaining.clone()
    } else {
        attempted
    };
    let reqs: Vec<ZoneMoveRequest> = if requests.is_empty() {
        remaining
            .into_iter()
            .map(|obj_id| {
                let mut req =
                    ZoneMoveRequest::effect(obj_id, destination, source_id.unwrap_or(obj_id));
                req.mods.enter_tapped = enter_tapped;
                req.exile_links.tracking = exile_tracking;
                if let Some(position) = library_placement.clone() {
                    req = req.at_library_position(position);
                }
                req.replacement_applied = replacement_applied.clone();
                req
            })
            .collect()
    } else {
        requests
            .into_iter()
            .map(ZoneMoveRequest::from_pending)
            .collect()
    };
    if let Some(ref paused_current) = paused_current {
        crate::game::triggers::append_and_collect_logical_zone_trigger_segment(
            state,
            &mut logical_zone_change_group,
            &paused_current.delivery_events,
        )
        .expect("resumed batch retains its exact paused delivery");
        let terminal_completion = paused_current
            .terminal_completion
            .expect("resumed batch delivery records its exact terminal completion");
        logical_zone_change_group
            .record_delivery_completion(paused_current.member.object_id, terminal_completion)
            .expect("resumed batch member records its exact terminal outcome");
    }
    let group_id = logical_zone_change_group.logical_group_id;
    match deliver_batch(
        state,
        reqs,
        logical_zone_change_group,
        BatchDeliveryParking::ReparkActive,
        false,
        Vec::new(),
        None,
        events,
    ) {
        BatchDeliveryResult::Done(logical_zone_change_group) => {
            state
                .resolution_stack
                .take_active_batch_delivery()
                .expect("settled batch delivery must own the active frame")
                .expect("settled batch delivery frame must exist");
            let settled = PendingBatchDeliveries {
                component_delivery_parent,
                logical_zone_change_group: *logical_zone_change_group,
                paused_current,
                remaining: Vec::new(),
                destination,
                source_id,
                enter_tapped,
                exile_tracking,
                library_placement,
                completion,
                replacement_applied,
                requests: Vec::new(),
                attempted,
                zone_change_record_start,
                deferred_events: Vec::new(),
                staged: None,
            };
            settle_batch_owner(state, settled, deferred_events, events);
        }
        BatchDeliveryResult::NeedsChoice => {
            // `deliver_batch` already re-parked the exact same owner,
            // including a pause on its final member. Re-attach only the
            // completion and external output; never replace that owner.
            let reparking = state
                .resolution_stack
                .batch_delivery_owner_mut(group_id)
                .expect("repark retains the exact original batch owner");
            reparking.component_delivery_parent = component_delivery_parent;
            reparking.completion = completion;
            reparking.attempted = attempted;
            reparking.zone_change_record_start = zone_change_record_start;
            deferred_events.append(events);
            reparking.deferred_events = deferred_events;
        }
    }
}

/// CR 616.1: drain one staged batch by its current stage. The
/// owner is taken off the stack for the run and re-parked only if the batch
/// pauses; delivering while its frame were installed would let a delivery
/// child bury it.
fn drain_staged_batch(
    state: &mut GameState,
    pending: PendingBatchDeliveries,
    events: &mut Vec<GameEvent>,
) {
    let group_id = pending.logical_zone_change_group.logical_group_id;
    if state
        .active_batch_delivery()
        .is_none_or(|active| active.logical_zone_change_group.logical_group_id != group_id)
    {
        return;
    }
    let Ok(Some(mut owner)) = state.resolution_stack.take_active_batch_delivery() else {
        return;
    };
    // The boundary the detached owner left behind: a delivery child raised by
    // the run belongs above it, so the owner must be inserted back below that
    // child (and above its ChangeZone marker).
    let repark_boundary = state.resolution_stack.capture_child_boundary();
    let component_delivery_parent = owner.component_delivery_parent.clone();
    let completion = owner.completion.clone();
    let attempted = owner.attempted.clone();
    let zone_change_record_start = owner.zone_change_record_start;
    let mut deferred_events = owner.deferred_events.clone();
    deferred_events.append(events);
    let stage = owner.staged.as_ref().map(|staged| staged.stage);

    if stage == Some(BatchDeliveryStage::Delivering) {
        if let Some(paused) = owner.paused_current.clone() {
            // A resumed pause is terminal for this delivery even when the
            // sidecar classification is absent: its own retained events are the
            // authoritative witness (the same rule the legacy drain applies).
            let terminal = paused.terminal_completion_after_resume();
            crate::game::triggers::append_and_collect_logical_zone_trigger_segment(
                state,
                &mut owner.logical_zone_change_group,
                &paused.delivery_events,
            )
            .expect("resumed staged batch retains its exact paused delivery");
            owner
                .logical_zone_change_group
                .record_delivery_completion(paused.member.object_id, terminal)
                .expect("resumed staged member records its exact terminal outcome");
            if let Some(index) = owner.staged.as_ref().and_then(|staged| {
                staged
                    .members
                    .iter()
                    .position(|member| member.identity == paused.member)
            }) {
                mark_staged_member_terminal(state, &mut owner, index, terminal, events);
            }
        }
    }

    let stop = match stage {
        Some(BatchDeliveryStage::Approving) => {
            let _ = finalize_staged_pending_outcome_on(state, &mut owner, events);
            run_staged_batch(state, &mut owner, events)
        }
        Some(BatchDeliveryStage::Arranging) => {
            if matches!(state.waiting_for, WaitingFor::DigRestSplitChoice { .. }) {
                StagedStop::Arranging
            } else {
                run_staged_batch(state, &mut owner, events)
            }
        }
        _ => run_staged_batch(state, &mut owner, events),
    };

    match stop {
        StagedStop::Paused | StagedStop::Arranging => {
            owner.component_delivery_parent = component_delivery_parent;
            owner.completion = completion;
            owner.attempted = attempted;
            owner.zone_change_record_start = zone_change_record_start;
            deferred_events.append(events);
            owner.deferred_events = deferred_events;
            state.repark_batch_owner_below_children(owner, repark_boundary);
        }
        StagedStop::Delivered => {
            if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
            }
            owner.component_delivery_parent = component_delivery_parent;
            owner.completion = completion;
            owner.attempted = attempted;
            owner.zone_change_record_start = zone_change_record_start;
            settle_batch_owner(state, owner, deferred_events, events);
        }
    }
}

#[cfg(test)]
mod paused_search_audience_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::game_state::{
        HiddenSearchAudience, LibrarySearchDeliveryResume, PendingBatchDeliveries,
        PendingReplacement, PendingZoneChangeDelivery, ZoneMoveCompletion,
    };
    use crate::types::identifiers::CardId;

    #[test]
    fn paused_batch_settlement_preserves_exact_search_audience_identity() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(91001),
            PlayerId(0),
            "Paused search member".to_string(),
            Zone::Graveyard,
        );
        let second = create_object(
            &mut state,
            CardId(91002),
            PlayerId(0),
            "Other searched card".to_string(),
            Zone::Graveyard,
        );
        let identity_a = ObjectIncarnationRef::from_object(&state.objects[&first]);
        let identity_b = ObjectIncarnationRef::from_object(&state.objects[&second]);

        let mut paused = PendingZoneChangeDelivery::new(
            identity_a,
            ProposedEvent::zone_change(first, Zone::Graveyard, Zone::Hand, None),
        );
        paused.terminal_completion = Some(ZoneMoveCompletion::Remained);

        let completion = BatchCompletion::LibrarySearchDeliverySettled {
            resume: LibrarySearchDeliveryResume::Standard {
                searcher: PlayerId(0),
                hidden_search_audiences: vec![
                    HiddenSearchAudience {
                        identity: identity_a,
                        audience: vec![PlayerId(0)],
                    },
                    HiddenSearchAudience {
                        identity: identity_b,
                        audience: vec![PlayerId(1)],
                    },
                ],
            },
        };
        let logical_zone_change_group =
            crate::game::triggers::allocate_logical_zone_change_group(&mut state, &[]);
        state.push_batch_delivery(PendingBatchDeliveries {
            component_delivery_parent: None,
            logical_zone_change_group,
            paused_current: Some(paused),
            remaining: Vec::new(),
            destination: Zone::Hand,
            source_id: None,
            enter_tapped: EtbTapState::Unspecified,
            exile_tracking: ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: Some(completion),
            replacement_applied: HashSet::new(),
            requests: Vec::new(),
            attempted: Vec::new(),
            zone_change_record_start: 0,
            deferred_events: Vec::new(),
            staged: None,
        });

        drain_pending_batch_deliveries(&mut state, &mut Vec::new());

        assert_eq!(
            state.completed_hidden_search_audiences,
            vec![
                HiddenSearchAudience {
                    identity: identity_a,
                    audience: vec![PlayerId(0)],
                },
                HiddenSearchAudience {
                    identity: identity_b,
                    audience: vec![PlayerId(1)],
                },
            ],
            "paused settlement must retain each searched incarnation's own audience"
        );
        assert!(
            state.completed_hidden_search_audiences.iter().all(|entry| {
                !(entry.identity == identity_a && entry.audience.contains(&PlayerId(1)))
            }),
            "the paused member must not absorb the other search entry's audience"
        );
    }

    /// CR 614.12a + CR 616.1f: a parked entry whose proposal an answer has
    /// changed still settles its paused owner when the resume is Prevented.
    /// The owner parked on the original event; the shared resume reads the key
    /// through the record's pinned boundary, records the `Prevented` terminal
    /// completion on that owner, and the owner retires on its drain. The changed
    /// proposal itself is not the owner's key.
    #[test]
    fn prevented_resume_settles_the_owner_through_the_pinned_boundary() {
        let mut state = GameState::new_two_player(9783);
        let entrant = create_object(
            &mut state,
            CardId(9783),
            PlayerId(0),
            "Parked entrant".to_string(),
            Zone::Graveyard,
        );
        let original =
            ProposedEvent::zone_change(entrant, Zone::Graveyard, Zone::Battlefield, None);

        state.pending_replacement = Some(PendingReplacement {
            proposed: original.clone(),
            sacrifice_provenance: None,
            candidates: Vec::new(),
            search_found_candidates: Vec::new(),
            depth: 0,
            is_optional: false,
            choice_player: None,
            library_placement: None,
            exile_controller: None,
            exile_duration: None,
            exile_tracking: ZoneDeliveryExileTracking::None,
            excess_recipient: None,
            lifelink_bonus: 0,
            may_cost_paid: false,
            may_cost_remaining: None,
            delivery_boundary: None,
            unrepresentable_life_change: Default::default(),
        });

        // The owner parks on the unanswered event.
        let paused_current = state
            .pending_zone_change_delivery_from_replacement()
            .expect("a parked zone change has a delivery key");
        let logical_zone_change_group =
            crate::game::triggers::allocate_logical_zone_change_group(&mut state, &[entrant]);
        state.push_batch_delivery(PendingBatchDeliveries {
            component_delivery_parent: None,
            logical_zone_change_group,
            paused_current: Some(paused_current),
            remaining: Vec::new(),
            destination: Zone::Battlefield,
            source_id: None,
            enter_tapped: EtbTapState::Unspecified,
            exile_tracking: ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: None,
            replacement_applied: HashSet::new(),
            requests: Vec::new(),
            attempted: Vec::new(),
            zone_change_record_start: 0,
            deferred_events: Vec::new(),
            staged: None,
        });

        // An answer pins the boundary, then changes the proposal.
        let pending = state.pending_replacement.as_mut().expect("parked entry");
        pending.pin_delivery_boundary();
        if let ProposedEvent::ZoneChange { enter_tapped, .. } = &mut pending.proposed {
            *enter_tapped = EtbTapState::Tapped;
        }
        let changed = pending.proposed.clone();
        assert_ne!(changed, original, "reach: the proposal changed");

        // The shared resume reads the owner key before the record is consumed,
        // and the replacement resolves as Prevented.
        let key = state
            .pending_zone_change_delivery_from_replacement()
            .expect("the key survives the change");
        assert_eq!(key.expected_event, original);
        state.pending_replacement = None;
        let member = key.member;
        assert!(
            !state.capture_paused_zone_change_delivery(
                member,
                &changed,
                &[],
                ZoneMoveCompletion::Prevented
            ),
            "the changed proposal is not the owner's key"
        );
        assert!(state.capture_paused_zone_change_delivery(
            member,
            &key.expected_event,
            &[],
            ZoneMoveCompletion::Prevented
        ));
        assert_eq!(
            state
                .active_batch_delivery()
                .and_then(|owner| owner.paused_current.as_ref())
                .and_then(|paused| paused.terminal_completion),
            Some(ZoneMoveCompletion::Prevented)
        );

        drain_pending_batch_deliveries(&mut state, &mut Vec::new());
        assert!(
            state.active_batch_delivery().is_none(),
            "the settled owner retires"
        );
    }
}

/// Deliver an event that already passed the replacement consult. Only callable
/// with the `ApprovedZoneChange` proof token — the consult-once/deliver-once
/// contract for every bucket-A post-replacement site (destroy/sacrifice/SBA
/// lowering, the replacement-choice resume path, land play).
pub(crate) fn deliver(
    state: &mut GameState,
    approved: ApprovedZoneChange,
    ctx: DeliveryCtx,
    events: &mut Vec<GameEvent>,
) -> ZoneDeliveryResult {
    deliver_batch_member(state, approved, ctx, None, events)
}

pub(crate) fn deliver_batch_member(
    state: &mut GameState,
    approved: ApprovedZoneChange,
    ctx: DeliveryCtx,
    owner: Option<BatchMemberDeliveryOwner>,
    events: &mut Vec<GameEvent>,
) -> ZoneDeliveryResult {
    let track_exiled_by_source = matches!(
        ctx.exile_links.tracking,
        ZoneDeliveryExileTracking::TrackBySource
    );
    deliver_replaced_batch_member(
        state,
        approved.event,
        ctx.source_id,
        ctx.exile_links.duration.as_ref(),
        ctx.exile_links.controller,
        track_exiled_by_source,
        ctx.drain,
        // Compatibility context only: the sealed event owns placement.
        ctx.library_placement,
        None,
        owner,
        events,
    )
}

/// CR 614.1c + CR 122.1: Collect the additional ETB counters that active
/// "[scope] creatures you control enter with an additional [counter] counter on
/// them" statics contribute to the object that just entered the battlefield.
///
/// Scans the static sources that were already functioning before the zone move
/// for the `StaticMode::EntersWithAdditionalCounters` variant and tests each
/// one's `affected` filter against the entering object, using a `FilterContext`
/// anchored at the STATIC's source. Anchoring at the source is what makes the
/// "Other creatures you control" qualifier exclude the static's own permanent
/// (`FilterProp::Another` compares the candidate against the context source).
///
/// Returns an aggregated `(CounterType, count)` list so multiple active sources
/// stack additively (CR 616.1f: repeat the replacement process until none apply).
/// The caller folds this through the shared `apply_etb_counters` resolver.
fn enters_with_additional_counters_for_entry(
    state: &GameState,
    object_id: ObjectId,
    static_defs: &[(ObjectId, StaticDefinition)],
) -> Vec<(CounterType, u32)> {
    let mut additional: Vec<(CounterType, u32)> = Vec::new();
    for (source_id, def) in static_defs {
        let Some(source_obj) = state.objects.get(source_id) else {
            continue;
        };
        let crate::types::statics::StaticMode::EntersWithAdditionalCounters {
            counter_type,
            count,
        } = &def.mode
        else {
            continue;
        };
        let Some(affected) = def.affected.as_ref() else {
            continue;
        };
        // CR 109.5: evaluate the "you control" + Other/Legendary/Nontoken filter
        // with the static's source as the context anchor.
        let ctx = crate::game::filter::FilterContext::from_source(state, source_obj.id);
        // CR 614.12: an entrant whose as-enters choices were made before it
        // entered is matched as it exists with its own statics applied — the
        // layer pass hasn't run on it yet — when that's proven for this filter.
        let projected = crate::game::filter::EntryProjection::delivered(state, object_id, affected);
        let matches = match projected.as_ref() {
            Some(entrant) => {
                crate::game::filter::matches_target_filter_on_object(state, entrant, affected, &ctx)
            }
            None => crate::game::filter::matches_target_filter(state, object_id, affected, &ctx),
        };
        if matches {
            additional.push((counter_type.clone(), *count));
        }
    }
    additional
}

#[allow(clippy::too_many_arguments)]
fn append_zone_delivery_tail_after_counter_pause(
    state: &mut GameState,
    object_id: ObjectId,
    from: Zone,
    to: Zone,
    cause: Option<ObjectId>,
    source_id: Option<ObjectId>,
    duration: Option<&Duration>,
    exile_controller: Option<PlayerId>,
    exile_tracking: ZoneDeliveryExileTracking,
    drain: PostReplacementDrainOwner,
    enters_attacking: bool,
    clear_pending_etb_counters: Option<ObjectId>,
) -> ZoneDeliveryResult {
    let mut actions = Vec::new();
    if let Some(object_id) = clear_pending_etb_counters {
        actions.push(PendingCounterPostAction::ClearPendingEtbCounters { object_id });
    }
    actions.push(PendingCounterPostAction::ContinueZoneDeliveryTail {
        object_id,
        from,
        to,
        cause,
        source_id,
        duration: duration.cloned(),
        exile_controller,
        exile_tracking,
        drain,
        enters_attacking,
    });
    crate::game::effects::counters::append_pending_counter_post_actions(state, actions);
    replacement_pause_delivery_result(state)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_zone_delivery_tail(
    state: &mut GameState,
    object_id: ObjectId,
    from: Zone,
    to: Zone,
    cause: Option<ObjectId>,
    source_id: Option<ObjectId>,
    duration: Option<&Duration>,
    exile_controller: Option<PlayerId>,
    exile_tracking: ZoneDeliveryExileTracking,
    drain: PostReplacementDrainOwner,
    // CR 701.24a: when a specific library position was requested, the object was
    // placed at that index and the library is NOT shuffled — a placement
    // instruction is not a shuffle instruction (CR 701.24a defines shuffling).
    // `None` = plain library-destination ZoneChange, which the tail's auto-shuffle
    // convention then randomizes. The counter-pause continuation
    // (`ContinueZoneDeliveryTail`) never carries a placement: library placements
    // bear no enters-with counters and never enter the battlefield, so they
    // never reach the counter-replacement pause that re-enters this tail.
    library_placement: Option<&LibraryPosition>,
    events: &mut Vec<GameEvent>,
) -> ZoneDeliveryResult {
    // CR 701.24a: To shuffle a library, randomize the cards within it so that
    // no player knows their order. A request that places the object at a specific
    // position is NOT a shuffle (a placement instruction is not a shuffle
    // instruction), so suppress the tail's auto-shuffle convention when a
    // `library_placement` was honored by the move above. (CR 701.24g — shuffle and
    // placement instructed simultaneously, shuffle-with-object-pinned — is a
    // different case that does not arise here.)
    if to == Zone::Library && library_placement.is_none() {
        let owner = state.objects.get(&object_id).map(|o| o.owner);
        if let Some(owner) = owner {
            shuffle_library(state, owner, events);
        }
    }
    // Track cards exiled by the source. Some linked exiles return when the
    // source leaves; others are just remembered as "exiled with" the source.
    // Route through `exile_links::push_with_kind` so the link is deduped on the
    // `(exiled_id, source_id)` pair AND the per-turn `cards_exiled_with_source_
    // this_turn` rolling list stays in lockstep — matching the behavior of callers
    // that previously pushed via `push_tracked_by_source` (e.g. `ExileTop`).
    if to == Zone::Exile {
        if let Some(source_id) = cause.or(source_id) {
            let kind = match duration {
                Some(d) if d.ends_when_host_leaves_play() => {
                    Some(ExileLinkKind::UntilSourceLeaves { return_zone: from })
                }
                Some(Duration::UntilOpponentBecomesMonarch) => {
                    exile_controller.map(|controller| ExileLinkKind::UntilOpponentBecomesMonarch {
                        return_zone: from,
                        controller,
                    })
                }
                // CR 607.2b: track either when the caller already determined
                // (via `should_track_exiled_by_source`, ability-chain-aware)
                // that this exile must be linked, OR when the resolved source
                // independently carries a "cards exiled with [this object]"
                // ability — the auto-detect a bare replacement redirect (SBA
                // death, `Effect::Destroy`, `Effect::Sacrifice`, none of which
                // resolve through an ability chain) needs, since those callers
                // have no `ResolvedAbility` to run the ability-level half of
                // that check against. CR 607.2q: a tracked exile made while its
                // source is a spell being cast pays that cast's cost, so its kind
                // is `CastCost` (`exile_links::tracked_link_kind`).
                _ if matches!(exile_tracking, ZoneDeliveryExileTracking::TrackBySource)
                    || crate::game::exile_links::source_is_linked_exile_consumer(
                        state, source_id,
                    ) =>
                {
                    Some(crate::game::exile_links::tracked_link_kind(
                        state, source_id,
                    ))
                }
                _ => None,
            };
            if let Some(kind) = kind {
                crate::game::exile_links::push_with_kind(state, object_id, source_id, kind);
            }
        }
    }
    // CR 614.12a: Drain mandatory replacement post-effects after the zone
    // change completes. This shared delivery path covers effect-driven moves
    // (`ChangeZone`) in the same way stack resolution and land play already
    // do, so as-enters work such as "enters prepared" or persisted choices
    // applies before triggers and priority.
    //
    // CR 614.12a: A Devour as-enters sacrifice surfaces its own interactive
    // `EffectZoneChoice` here. Surface that pause to the caller via
    // `NeedsChoice` so the mass/single zone-change loop stashes the remaining
    // co-entering members and resumes after the choice (instead of dropping
    // them, issue #535 class).
    //
    // `CallerEpilogue` (the replacement-choice resume path) skips this drain:
    // its epilogue drains the continuation itself, WITH the spell-resolution
    // ctx and with `post_replacement_source` cleared for zone changes, and
    // only after `apply_pending_spell_resolution` (Phase-B divergence
    // reconciliation — the tail is parameterized instead of copied).
    if matches!(drain, PostReplacementDrainOwner::DeliveryTail)
        && state.has_post_replacement_drain()
    {
        // CR 603.6d + CR 614.12a: For an "as-enters" (battlefield-entry) Moved
        // post-effect, the effect resolves against the zone-changing object (the
        // ENTRANT), NOT the replacement's host source. Drop the stashed host
        // source slot for battlefield entries — exactly as the cast-resolution
        // (`stack.rs`), land-play (`engine.rs`), and replacement-choice resume
        // (`engine_replacement.rs`) drain sites already do — so a non-self `Moved`
        // GenericEffect (Displaced Dinosaurs: "As a historic permanent you control
        // enters, it becomes a 7/7 Dinosaur creature in addition to its other
        // types") binds its `SelfRef` execute to the entrant, not the host.
        //
        // Scoped to `to == Battlefield`: only as-enters replacements bind to the
        // entrant. A non-battlefield delivery that incidentally drains an outer
        // effect's still-pending continuation here (e.g. a Mill replacement's
        // doubling continuation while its milled cards move to the graveyard)
        // must keep the host source slot — its post-effect belongs to the host,
        // not the moved card.
        if to == Zone::Battlefield {
            state.clear_post_replacement_source();
        }
        let waiting_for = crate::game::engine_replacement::apply_pending_post_replacement_effect(
            state,
            Some(object_id),
            None,
            Some(crate::types::replacements::ReplacementEvent::Moved),
            events,
        );
        if let Some(wf) = waiting_for {
            if !matches!(wf, WaitingFor::Priority { .. }) {
                if matches!(wf, WaitingFor::CopyTargetChoice { .. }) {
                    if let Some(LiminalEntryKind::Meld {
                        context,
                        attack_target,
                        ..
                    }) = state
                        .liminal_entries
                        .get(&object_id)
                        .map(|entry| entry.kind.clone())
                    {
                        state.pending_liminal_entry_resume =
                            Some(PendingLiminalEntryResume::Meld {
                                source_id: object_id,
                                player: wf.acting_player().unwrap_or(state.active_player),
                                context,
                                attack_target,
                            });
                    }
                }
                state.waiting_for = wf;
                return replacement_pause_delivery_result(state);
            }
        }
    }
    ZoneDeliveryResult::Done
}

/// CR 614.12 + CR 303.4f: the characteristics the CR 303.4f/g consult must read
/// for `object_id` — "the characteristics of the permanent as it would exist on
/// the battlefield".
///
/// A liminal entry IS that projection, and while it is pending the object still
/// stored under the same id is the entrant's PRE-entry self: for a meld
/// (`LiminalEntryKind::Meld`) that is the exiled front-face component card, which
/// is not an Aura and carries none of the result face's `Enchant` abilities.
/// Reading `state.objects` alone therefore made the consult blind to every
/// card-backed liminal Aura entrant. Same dual lookup, in the same precedence,
/// that the intrinsic enter-with-counters seeding in
/// `consult_and_deliver_zone_change` and `copy_effect_for_source` already use.
fn entering_object_projection(state: &GameState, object_id: ObjectId) -> Option<&GameObject> {
    state.entering_or_live_object(object_id)
}

fn aura_enchant_filter(state: &GameState, object_id: ObjectId) -> Option<TargetFilter> {
    aura_enchant_filter_of(entering_object_projection(state, object_id)?)
}

/// CR 303.4 + CR 702.5: the Enchant ability an ENTRANT will have on the
/// battlefield, read from an explicitly supplied projection.
///
/// Split from [`aura_enchant_filter`] so a seam holding a projection that is not
/// (yet) the object stored under its id can consult it — CR 614.12's "the
/// characteristics of the permanent as it would exist on the battlefield". Two
/// seams need that: a liminal entrant, whose id still holds the pre-entry
/// component card, and a non-liminal copy token whose CR 707.9 exceptions are
/// applied by a later, unjournaled seam.
pub(crate) fn aura_enchant_filter_of(obj: &GameObject) -> Option<TargetFilter> {
    if !obj.card_types.subtypes.iter().any(|s| s == "Aura") {
        return None;
    }
    // CR 303.4d: An Aura that's also a creature can't enchant anything.
    if obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Creature)
    {
        return None;
    }
    let filters: Vec<TargetFilter> = obj
        .keywords
        .iter()
        .filter_map(|keyword| match keyword {
            Keyword::Enchant(filter) => Some(filter.clone()),
            _ => None,
        })
        .collect();
    match filters.as_slice() {
        [] => None,
        [filter] => Some(filter.clone()),
        _ => Some(TargetFilter::And { filters }),
    }
}

/// CR 303.4f: the legal hosts for an entering Aura.
///
/// `entrant` is the CR 614.12 projection of the ATTACHMENT — the characteristics
/// the Aura will have on the battlefield. Every host-legality check below that
/// reads the attachment side reads it, never `state.objects[aura_id]`: for a
/// liminal entrant that id still holds the pre-entry component card (a meld's
/// exiled front face), whose typeline, colors and controller are not the entering
/// permanent's, and for a non-liminal copy token the CR 707.9 exceptions have not
/// been stamped onto the stored object yet.
fn legal_aura_attachment_targets(
    state: &GameState,
    aura_id: ObjectId,
    entrant: Option<&GameObject>,
    controller: PlayerId,
    enchant_filter: &TargetFilter,
) -> Vec<TargetRef> {
    let ctx = crate::game::filter::FilterContext::from_source_with_controller(aura_id, controller);
    // CR 303.4f: the controller chooses a legal object per the Aura's current
    // enchant ability. Enumerate candidate hosts across whatever zone(s) that
    // ability implies — an ordinary Aura (Pacifism) imposes no zone property and
    // defaults to the battlefield, while a graveyard/hand-scoped enchant ability
    // (Animate Dead, Dance of the Dead, Spellweaver Volute, Don't Worry About It)
    // carries a `FilterProp::InZone`/`InAnyZone` that `extract_zones` surfaces.
    // Mirrors `object_count_matching_ids` in `game/quantity.rs`. Using
    // `zone_object_ids` for the battlefield case also (correctly) excludes
    // phased-out permanents per CR 702.26b — they're treated as nonexistent and
    // can never be a legal new host.
    let zones = enchant_filter.extract_zones();
    let zones = if zones.is_empty() {
        vec![Zone::Battlefield]
    } else {
        zones
    };
    let mut targets: Vec<TargetRef> = zones
        .into_iter()
        .flat_map(|zone| crate::game::targeting::zone_object_ids(state, zone))
        // CR 303.4d: an Aura can't enchant itself.
        .filter(|id| *id != aura_id)
        // CR 115.1b + CR 303.4f: this consult is a controller CHOICE, not a
        // targeting event (an Aura permanent doesn't target) — use
        // `matches_target_filter`, never the `find_legal_targets` enumerator, so
        // hexproof (CR 702.11) / shroud (CR 702.18) never remove a legal host.
        .filter(|id| crate::game::filter::matches_target_filter(state, *id, enchant_filter, &ctx))
        // CR 701.3a + CR 702.16c: host-side prohibitions and protection, read
        // against the CR 614.12 entrant projection so a protection or
        // attachment-restriction match is computed from the characteristics the
        // permanent will have on the battlefield.
        .filter(|id| {
            crate::game::effects::attach::can_attach_to_object_projected(
                state, aura_id, entrant, *id,
            )
        })
        .map(TargetRef::Object)
        .collect();

    targets.extend(state.players.iter().filter_map(|player| {
        // Hygiene routing, behaviour-neutral by construction: `is_eliminated ||
        // is_phased_out()` on an iterated member is the negation of what
        // `players::player_exists_for_choice` spells for a member already known to be in
        // `state.players`. Routed so an existence fix propagates here for free.
        if !crate::game::players::player_exists_for_choice(state, player.id) {
            return None;
        }
        // CR 303.4c + CR 702.16c: the player-host mirror of the object-host
        // legality filter above. Without it an illegal player counts as a legal
        // host, which suppresses the CR 303.4g denial: the Curse token copy is
        // created, `attach_to_player` no-ops on the illegality, and CR 704.5m
        // sweeps it — exactly the entered-then-died bug this seam exists to
        // prevent, on the player axis.
        if !crate::game::effects::attach::can_attach_to_player_projected(state, entrant, player.id)
        {
            return None;
        }
        if crate::game::filter::player_matches_target_filter_in_state(
            state,
            enchant_filter,
            player.id,
            Some(controller),
            Some(aura_id),
        ) {
            Some(TargetRef::Player(player.id))
        } else {
            None
        }
    }));

    targets
}

/// CR 303.4g: the fate of an Aura that is entering the battlefield when "there
/// is no legal object or player for it to enchant".
///
/// Three outcomes, because the rule states three — and NONE of them is "enter
/// unattached and let the CR 704.5m state-based action sweep it". The rule
/// denies the entry itself, so a seam that can still decide must decide here;
/// anything the game could observe of that entry is an event the rules say never
/// happened.
pub(crate) enum UnhostedAuraEntry {
    /// CR 303.4g: "If the Aura is a token, it isn't created."
    NotCreated,
    /// CR 303.4g: "the Aura remains in its current zone" — the entry does not
    /// happen and the card stays exactly where it was.
    RemainInCurrentZone,
    /// CR 303.4g: "…unless that zone is the stack. In that case, the Aura is put
    /// into its owner's graveyard instead of entering the battlefield."
    OwnersGraveyard,
}

/// CR 303.4g: select the disposition from the two facts the rule keys on — the
/// entrant's CR 111.1 token-ness, and the zone it is entering from.
///
/// Every entrant this authority answers for HAS a from-zone, because both of the
/// rule's non-token dispositions are phrased against one ("remains in its
/// current zone", "unless that zone is the stack"). That is the whole population
/// of the `ProposedEvent::ZoneChange` entry path. The other entry path,
/// `ProposedEvent::TokenEntry`, carries a `LiminalEntrant::Token` — a CR 111.1
/// token, in no zone at all — for which the rule's token clause is the only
/// applicable disposition, so that seam never asks this question.
pub(crate) fn unhosted_aura_entry(entrant: &GameObject, from: Zone) -> UnhostedAuraEntry {
    // CR 111.1: token-ness is the ONLY discriminator the rule's token clause
    // names, and it outranks the origin — a token is not created regardless of
    // which zone the effect was putting it onto the battlefield from.
    if entrant.is_token {
        return UnhostedAuraEntry::NotCreated;
    }
    match from {
        Zone::Stack => UnhostedAuraEntry::OwnersGraveyard,
        _ => UnhostedAuraEntry::RemainInCurrentZone,
    }
}

/// CR 303.4f + CR 303.4g + CR 614.12: the Aura-host half of the
/// post-replacement battlefield-entry consult. Shared by the legacy mover
/// (`execute_zone_move_with_applied_terminal`) and the staged batch delivery
/// seam, so both apply the same host legality, the same unhosted disposition
/// and the same controller-authorized prompt.
pub(crate) enum AuraHostPostReplaceOutcome {
    /// Attach to the sole legal host (recorded on the event), or no Aura work.
    Deliver,
    /// CR 303.4g: the entry does not happen and the Aura is put into its
    /// owner's graveyard instead — a FRESH, never-consulted move.
    UnhostedOwnersGraveyard,
    /// CR 303.4g: the Aura remains in its current zone; the entry does not happen.
    Remained,
    /// CR 303.4f: several legal hosts, so the Aura's controller chooses one
    /// after the entry has been delivered.
    NeedsHost {
        controller: PlayerId,
        aura_id: ObjectId,
        legal_targets: Vec<TargetRef>,
    },
}

/// CR 303.4f + CR 303.4g: decide the Aura-host half of one approved battlefield
/// entry. Mutates `event`'s `attach_to` for a sole legal host and never touches
/// the game state.
pub(crate) fn consult_aura_host_post_replace(
    state: &GameState,
    event: &mut ProposedEvent,
    _source_id: Option<ObjectId>,
) -> AuraHostPostReplaceOutcome {
    let ProposedEvent::ZoneChange {
        object_id,
        from,
        to: Zone::Battlefield,
        attach_to,
        controller_override,
        ..
    } = event
    else {
        return AuraHostPostReplaceOutcome::Deliver;
    };
    if attach_to.is_some() {
        return AuraHostPostReplaceOutcome::Deliver;
    }
    let Some(enchant_filter) = aura_enchant_filter(state, *object_id) else {
        return AuraHostPostReplaceOutcome::Deliver;
    };
    // CR 614.12: read the entrant's projected characteristics, not the
    // pre-entry object still stored under this id — for a meld the latter is
    // the exiled component card and has the wrong controller as well as the
    // wrong typeline.
    let controller = (*controller_override)
        .or_else(|| entering_object_projection(state, *object_id).map(|obj| obj.controller))
        .unwrap_or(PlayerId(0));
    let legal_targets = legal_aura_attachment_targets(
        state,
        *object_id,
        entering_object_projection(state, *object_id),
        controller,
        &enchant_filter,
    );
    match legal_targets.as_slice() {
        // CR 303.4g: no legal object or player to enchant, so this entry does
        // not happen. Decided BEFORE delivery, i.e. before the object is
        // inserted into the battlefield and before any entry event.
        [] => match entering_object_projection(state, *object_id)
            .map(|entrant| unhosted_aura_entry(entrant, *from))
        {
            Some(UnhostedAuraEntry::OwnersGraveyard) => {
                AuraHostPostReplaceOutcome::UnhostedOwnersGraveyard
            }
            Some(UnhostedAuraEntry::NotCreated)
            | Some(UnhostedAuraEntry::RemainInCurrentZone)
            | None => AuraHostPostReplaceOutcome::Remained,
        },
        [TargetRef::Object(id)] => {
            *attach_to = Some(AttachTarget::Object(*id));
            AuraHostPostReplaceOutcome::Deliver
        }
        [TargetRef::Player(id)] => {
            *attach_to = Some(AttachTarget::Player(*id));
            AuraHostPostReplaceOutcome::Deliver
        }
        _ => AuraHostPostReplaceOutcome::NeedsHost {
            controller,
            aura_id: *object_id,
            legal_targets,
        },
    }
}

/// One shared terminal for a replaced zone change that may surface an Aura
/// host choice.
pub(crate) enum AuraAwareZoneDelivery {
    Completed(ZoneMoveCompletion),
    NeedsChoice(PlayerId),
    /// The entry was delivered and `WaitingFor::ReturnAsAuraTarget` is parked.
    NeedsAuraHostChoice,
}

/// CR 303.4f + CR 303.4g + CR 614.6: deliver one already-approved battlefield
/// entry with the shared Aura-host consult, then the ordinary zone-delivery
/// tail. The single seam both the legacy mover and the staged batch use.
#[allow(clippy::too_many_arguments)]
pub(crate) fn deliver_replaced_zone_change_with_aura_host(
    state: &mut GameState,
    obj_id: ObjectId,
    member: ObjectIncarnationRef,
    mut event: ProposedEvent,
    source_id: Option<ObjectId>,
    duration: Option<&Duration>,
    exile_controller: Option<PlayerId>,
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    explicit_library_index: Option<usize>,
    mut owner: Option<BatchMemberDeliveryOwner>,
    events: &mut Vec<GameEvent>,
) -> AuraAwareZoneDelivery {
    let host = consult_aura_host_post_replace(state, &mut event, source_id);
    // CR 303.4g + CR 614.6: retain entry work only when delivery or a host
    // choice remains possible; an impossible Aura entry drops both pieces.
    let retained_snapshot = owner
        .as_mut()
        .and_then(|owner| owner.population_devour_snapshot.take());
    let retained_drain = owner
        .as_mut()
        .and_then(|owner| owner.approved_post_replacement.take());
    if matches!(
        host,
        AuraHostPostReplaceOutcome::Deliver | AuraHostPostReplaceOutcome::NeedsHost { .. }
    ) {
        let had_snapshot = retained_snapshot.is_some();
        if let Some(snapshot) = retained_snapshot {
            state.push_devour_change_zone_snapshot(snapshot);
        }
        if let Some(drain) = retained_drain {
            state.resolution_stack.push_post_replacement(drain);
        }
        if had_snapshot {
            debug_assert!(state.active_devour_eligible_snapshot().is_some());
        }
    }
    let pending_host = match host {
        AuraHostPostReplaceOutcome::Deliver => None,
        AuraHostPostReplaceOutcome::Remained => {
            return AuraAwareZoneDelivery::Completed(ZoneMoveCompletion::Remained);
        }
        AuraHostPostReplaceOutcome::UnhostedOwnersGraveyard => {
            // CR 303.4g + CR 614.6: the approved battlefield entry never
            // happens and the graveyard placement that replaces it is a FRESH,
            // never-consulted event — so it routes through the pipeline rather
            // than being written as a destination rewrite of the already-
            // approved event. A board-wide `Moved` graveyard→exile redirect
            // (Rest in Peace / Leyline of the Void) therefore fires on it. The
            // already-applied set rides along so no replacement can be spent
            // twice.
            let applied = event.applied_set().clone();
            return match move_batch_member_with_terminal(
                state,
                ZoneMoveRequest::effect(obj_id, Zone::Graveyard, source_id.unwrap_or(obj_id))
                    .with_replacement_applied(applied),
                owner,
                events,
            ) {
                ZoneMoveTerminalResult::Completed(completion) => {
                    AuraAwareZoneDelivery::Completed(completion)
                }
                ZoneMoveTerminalResult::NeedsChoice(player) => {
                    AuraAwareZoneDelivery::NeedsChoice(player)
                }
                ZoneMoveTerminalResult::NeedsAuraAttachmentChoice => {
                    AuraAwareZoneDelivery::NeedsAuraHostChoice
                }
            };
        }
        AuraHostPostReplaceOutcome::NeedsHost {
            controller,
            aura_id,
            legal_targets,
        } => Some((controller, aura_id, legal_targets)),
    };

    let delivery_start = events.len();
    match deliver_replaced_batch_member(
        state,
        event,
        source_id,
        duration,
        exile_controller,
        track_exiled_by_source,
        PostReplacementDrainOwner::DeliveryTail,
        library_placement,
        explicit_library_index,
        owner,
        events,
    ) {
        ZoneDeliveryResult::Done => {}
        ZoneDeliveryResult::NeedsChoice(player) => {
            return AuraAwareZoneDelivery::NeedsChoice(player);
        }
    }
    if let Some((controller, aura_id, legal_targets)) = pending_host {
        state.waiting_for = WaitingFor::ReturnAsAuraTarget {
            player: controller,
            source_id: source_id.unwrap_or(aura_id),
            returned_id: aura_id,
            legal_targets,
            pending_effect: Box::new(ResolvedAbility::new(
                Effect::Attach {
                    attachment: TargetFilter::SelfRef,
                    target: TargetFilter::Any,
                    // Aura-attachment delivery: the entering Aura is the
                    // attachment; only its host is chosen here.
                    selection: AttachSelection::AtResolution {
                        count: AttachCardinality::One,
                    },
                },
                Vec::new(),
                source_id.unwrap_or(aura_id),
                controller,
            )),
        };
        return AuraAwareZoneDelivery::NeedsAuraHostChoice;
    }
    AuraAwareZoneDelivery::Completed(zone_move_completion_from_delivery(
        member,
        &events[delivery_start..],
    ))
}

/// Disposition of an object that has just become an Aura while already on the
/// battlefield (the copy path — see [`resolve_entering_aura_attachment`]).
///
/// `Attached` and `NoLegalHost` are deliberately distinct even though neither
/// raises a prompt: CR 303.4g gives the no-host case its OWN rule ("the Aura
/// remains in its current zone … If the Aura is a token, it isn't created"),
/// which a caller that can still decline to create the entrant must be able to
/// act on. Collapsing them loses exactly that information.
pub(crate) enum EnteringAuraAttachment {
    /// The object is not an Aura needing attachment (not an Aura, an Aura that's
    /// also a creature per CR 303.4d, or already attached).
    NotApplicable,
    /// CR 303.4f: attachment resolved without a player choice — the sole legal
    /// host was auto-attached.
    Attached,
    /// CR 303.4g: there is no legal object or player for the Aura to enchant.
    /// The Aura was left unattached; what that means is the caller's decision
    /// (see the callers' own CR 303.4g/CR 704.5m rationale).
    NoLegalHost,
    /// CR 303.4f: multiple legal hosts, so the controller must choose one.
    NeedsChoice {
        controller: PlayerId,
        legal_targets: Vec<TargetRef>,
    },
}

/// CR 303.4f + CR 303.4g: Resolve the enter-time attachment for an object that
/// has BECOME an Aura while already on the battlefield.
///
/// The normal aura entry attaches during `move_object`, before the permanent is
/// on the battlefield, via the entry event's `attach_to` slot (see the
/// `aura_enchant_filter` consult in `consult_and_deliver_zone_change`). A
/// permanent that enters as a plain enchantment and only becomes an Aura when
/// its `BecomeCopy` replacement resolves (Copy Enchantment, Estrid's Invocation)
/// never passed through that slot — `BecomeCopy` is realized post-entry — so its
/// attachment is resolved here, once the copy is realized and layers are
/// flushed.
///
/// CR 303.4f: because the Aura is entering by a means other than resolving as an
/// Aura spell and the effect doesn't specify a host, its controller chooses what
/// it enchants. CR 303.4g: with no legal host the Aura can't enter — this
/// function reports that as [`EnteringAuraAttachment::NoLegalHost`] and leaves
/// the object untouched, because only the CALLER knows whether the entrant can
/// still be withheld (a token that isn't created) or has already entered and is
/// therefore the CR 704.5m unattached-Aura SBA's problem.
///
/// Composed from [`entering_aura_hosts`] (decide) and
/// [`apply_entering_aura_hosts`] (act), which a caller that must interpose an
/// irreversible step between the two — the liminal token path, whose CR 733
/// birth journal append is append-only and must not be written for a token CR
/// 303.4g says isn't created — invokes separately.
pub(crate) fn resolve_entering_aura_attachment(
    state: &mut GameState,
    object_id: ObjectId,
) -> EnteringAuraAttachment {
    let hosts = entering_aura_hosts(state, object_id);
    apply_entering_aura_hosts(state, object_id, hosts)
}

/// The legal hosts an entering Aura may be attached to, decided but NOT applied.
///
/// `Hosts::legal_targets` may be empty — that IS the CR 303.4g case, and it is
/// reported rather than acted on so a caller can answer CR 303.4g's "if the Aura
/// is a token, it isn't created" BEFORE taking any step it cannot take back.
pub(crate) enum EnteringAuraHosts {
    /// Same disposition as [`EnteringAuraAttachment::NotApplicable`].
    NotApplicable,
    Hosts {
        controller: PlayerId,
        legal_targets: Vec<TargetRef>,
        /// CR 614.12: the object whose characteristics `legal_targets` was
        /// decided against, carried so the act half can judge CR 701.3a legality
        /// against the SAME object rather than re-deriving it from the id.
        entrant: EnteringAuraEntrant,
    },
}

/// CR 614.12: which object an entering Aura's attachment legality is judged
/// against — the decide half's finding, carried to the act half.
///
/// The two halves are separated by at least a function boundary and, on the
/// multi-host route, by a player-choice pause. Deriving the attachment side
/// twice is what let them disagree: CR 303.4f offers a host that is legal for
/// the Aura AS IT ENTERS, and CR 701.3b silently no-ops an attach at a host the
/// gate then judges illegal. Naming the authority instead of re-deriving it
/// makes that disagreement unrepresentable.
#[derive(Debug, Clone)]
pub(crate) enum EnteringAuraEntrant {
    /// The object stored under the Aura's id already IS the entrant, so the act
    /// half reads it LIVE. Deliberately not a snapshot: for these seams the
    /// permanent is already on the battlefield with its final characteristics,
    /// and a stale clone could only mask a legitimate mid-flight change.
    Stored,
    /// The stored object is not the entrant yet — the deciding seam supplied a
    /// projection (a CR 707.9 copy exception applied by a later seam, or a
    /// liminal entry whose id still holds the pre-entry component). The act half
    /// must use it, or it will judge a different object than the chooser was
    /// offered.
    Projected(Box<GameObject>),
}

impl EnteringAuraEntrant {
    /// The borrowed view the CR 701.3a legality gate consumes.
    fn authority(&self) -> crate::game::effects::attach::AttachmentAuthority<'_> {
        match self {
            Self::Stored => crate::game::effects::attach::AttachmentAuthority::Stored,
            Self::Projected(entrant) => {
                crate::game::effects::attach::AttachmentAuthority::Projected(entrant)
            }
        }
    }
}

/// Decide half of [`resolve_entering_aura_attachment`] — pure with respect to
/// the game state.
pub(crate) fn entering_aura_hosts(state: &GameState, object_id: ObjectId) -> EnteringAuraHosts {
    // CR 614.12: a LIVE liminal entry means the id's stored object is not the
    // entrant (a meld's exiled front face, a token whose body is still parked),
    // so the projection has to travel to the act half as well — the same class
    // of decide/act disagreement the copy-token seam hits. No production caller
    // of this function reaches it with a live entry today (the liminal token
    // seam removes its entry before consulting, and every
    // `resolve_entering_aura_attachment` caller runs on a realized battlefield
    // permanent), so this arm closes the class rather than fixing a live bug.
    if let Some(entry) = state.liminal_entries.get(&object_id) {
        let entrant = entry.object.projected().clone();
        return entering_aura_hosts_projected(state, object_id, &entrant);
    }
    let Some(entrant) = state.objects.get(&object_id) else {
        return EnteringAuraHosts::NotApplicable;
    };
    entering_aura_hosts_with(
        state,
        object_id,
        entrant,
        // Read live by the act half: for these seams the stored object IS the
        // entrant, and this preserves their exact pre-existing behaviour.
        EnteringAuraEntrant::Stored,
    )
}

/// CR 614.12: [`entering_aura_hosts`] against an explicitly supplied projection
/// of the ENTRANT.
///
/// The non-liminal copy-token seam owns a projection its stored object does not
/// match yet: on that path the CR 707.9b/9c "except …" exceptions are applied by
/// a later, unjournaled seam (`apply_token_modifications`), so the object under
/// this id still carries the UNMODIFIED copied body. An exception that adds or
/// removes `Creature` (CR 303.4d), adds or removes the `Aura` subtype, or changes
/// the entrant's colors (CR 702.16c protection) flips this verdict — and a wrong
/// verdict here is a token silently never created (CR 303.4g). The liminal seam
/// folds the same exceptions BEFORE its own consult, so passing the projection is
/// what makes the two seams agree on what the entrant is.
pub(crate) fn entering_aura_hosts_projected(
    state: &GameState,
    object_id: ObjectId,
    entrant: &GameObject,
) -> EnteringAuraHosts {
    entering_aura_hosts_with(
        state,
        object_id,
        entrant,
        // The whole point of the projected entry point: the act half must judge
        // CR 701.3a legality against this object, not against the id's stored
        // one, or CR 303.4f can offer a host CR 701.3b then refuses to attach to.
        EnteringAuraEntrant::Projected(Box::new(entrant.clone())),
    )
}

/// Shared body of [`entering_aura_hosts`] and [`entering_aura_hosts_projected`],
/// parameterized by which object the act half must judge legality against.
fn entering_aura_hosts_with(
    state: &GameState,
    object_id: ObjectId,
    entrant: &GameObject,
    authority: EnteringAuraEntrant,
) -> EnteringAuraHosts {
    let Some(enchant_filter) = aura_enchant_filter_of(entrant) else {
        return EnteringAuraHosts::NotApplicable;
    };
    // Existence and battlefield residency are read from the STORED object: they
    // are facts about where the entrant is, which no projection may override.
    let Some(obj) = state.objects.get(&object_id) else {
        return EnteringAuraHosts::NotApplicable;
    };
    // CR 303.4 + CR 704.5m: entry-time attachment only applies to an Aura that is
    // actually on the battlefield. Defensive guard — if an intermediate entry
    // trigger or replacement moved the realized copy off the battlefield before
    // this runs (it is the LAST step of `finish_copy_target_choice_entry`),
    // attaching it or prompting for a host of a non-battlefield Aura would be
    // invalid state; do nothing and let it resolve wherever it now lives.
    if obj.zone != Zone::Battlefield {
        return EnteringAuraHosts::NotApplicable;
    }
    // Only resolve entry attachment for an as-yet-unattached Aura; a copy that
    // was already attached by some other effect must not be re-homed here.
    if obj.attached_to.is_some() {
        return EnteringAuraHosts::NotApplicable;
    }
    // CR 303.4f: "that player" is the entrant's controller — read from the
    // projection, which is where a controller-changing entry effect lands first.
    let controller = entrant.controller;
    EnteringAuraHosts::Hosts {
        legal_targets: legal_aura_attachment_targets(
            state,
            object_id,
            Some(entrant),
            controller,
            &enchant_filter,
        ),
        controller,
        entrant: authority,
    }
}

/// Act half of [`resolve_entering_aura_attachment`]: attach the sole legal host
/// (CR 303.4f), or report the disposition the caller must handle.
///
/// CR 303.4f + CR 701.3b: every attach below goes through the decide half's
/// [`EnteringAuraEntrant`]. The act half must not re-derive the attachment's
/// characteristics from `object_id` — on the non-liminal copy-token seam that id
/// still holds the pre-exception body, so a re-derived CR 701.3a gate can reject
/// a host CR 303.4f legally offered and (CR 701.3b) no-op the attach, leaving the
/// Aura for the CR 704.5m sweep.
pub(crate) fn apply_entering_aura_hosts(
    state: &mut GameState,
    object_id: ObjectId,
    hosts: EnteringAuraHosts,
) -> EnteringAuraAttachment {
    // Any authority parked by an earlier entering-Aura decision is spent or
    // stale by the time another one is being ACTED on: the only way to reach
    // this function with one parked is to have resumed past that pause (which
    // takes it) or to have abandoned it. Cleared here rather than at the pause
    // so no path can leave one behind.
    state.entering_aura_authority = None;
    let EnteringAuraHosts::Hosts {
        controller,
        legal_targets,
        entrant,
    } = hosts
    else {
        return EnteringAuraAttachment::NotApplicable;
    };
    match legal_targets.as_slice() {
        // CR 303.4g: no legal object or player to enchant — report it and let the
        // caller decide the entrant's fate.
        [] => EnteringAuraAttachment::NoLegalHost,
        [TargetRef::Object(id)] => {
            crate::game::effects::attach::attach_to_with_authority(
                state,
                object_id,
                *id,
                entrant.authority(),
            );
            EnteringAuraAttachment::Attached
        }
        [TargetRef::Player(id)] => {
            crate::game::effects::attach::attach_to_player_with_authority(
                state,
                object_id,
                *id,
                entrant.authority(),
            );
            EnteringAuraAttachment::Attached
        }
        _ => {
            // CR 303.4f: the choice returns to the event loop, so the entrant has
            // to outlive this stack frame for the resume path's gate. Only a
            // genuine projection is parked — a `Stored` authority would freeze a
            // snapshot the resume path is better off reading live, and parking
            // nothing is exactly the pre-existing behaviour every non-projected
            // seam (Copy Enchantment's `BecomeCopy`, `ReturnAsAura`, the plain
            // ZoneChange entry) already has.
            if let EnteringAuraEntrant::Projected(entrant) = entrant {
                state.entering_aura_authority = Some(EnteringAuraAuthority {
                    aura_id: object_id,
                    entrant,
                });
            }
            EnteringAuraAttachment::NeedsChoice {
                controller,
                legal_targets,
            }
        }
    }
}

/// CR 303.4f + CR 701.3b: attach an entering Aura to the host its controller
/// chose, judged against the entrant the choice was offered for.
///
/// The single authority behind the `WaitingFor::ReturnAsAuraTarget` resume arm's
/// attach. That arm is shared by seams that park no
/// [`EnteringAuraAuthority`] — `ReturnAsAura` (Old-Growth Troll), the plain
/// non-spell Aura ZoneChange entry, and the on-battlefield `BecomeCopy`
/// realization — and for all of those the absent authority selects
/// [`EnteringAuraEntrant::Stored`], i.e. byte-for-byte the `attach_to` /
/// `attach_to_player` behaviour they had before.
pub(crate) fn attach_chosen_entering_aura_host(
    state: &mut GameState,
    aura_id: ObjectId,
    chosen: &TargetRef,
) -> Option<TargetRef> {
    // Taken unconditionally: a parked authority belongs to exactly one pause, so
    // whichever pause is resuming, it must not survive into a later one. It is
    // then honoured only for the Aura it was parked for.
    let parked = state
        .entering_aura_authority
        .take()
        .filter(|authority| authority.aura_id == aura_id)
        .map(|authority| EnteringAuraEntrant::Projected(authority.entrant));
    let entrant = parked.unwrap_or(EnteringAuraEntrant::Stored);
    match chosen {
        TargetRef::Object(host_id) => crate::game::effects::attach::attach_to_with_authority(
            state,
            aura_id,
            *host_id,
            entrant.authority(),
        ),
        TargetRef::Player(host_player) => {
            crate::game::effects::attach::attach_to_player_with_authority(
                state,
                aura_id,
                *host_player,
                entrant.authority(),
            )
        }
    }
}

#[cfg(test)]
mod entering_aura_attachment_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ChosenAttribute, ControllerRef,
        PostReplacementContinuation, QuantityExpr, ReplacementDefinition, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{
        PostReplacementDrain, PostReplacementDrainStack, ResidentDrainPolicy,
    };
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::keywords::ProtectionTarget;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    fn creature(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(90_100),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).expect("just created");
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(1);
        obj.toughness = Some(1);
        id
    }

    /// An unattached Aura token on the battlefield with `enchant creature`,
    /// controlled by `controller`.
    fn aura(state: &mut GameState, controller: PlayerId, enchant: TargetFilter) -> ObjectId {
        let id = create_object(
            state,
            CardId(90_200),
            controller,
            "Test Aura".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).expect("just created");
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.is_token = true;
        obj.keywords.push(Keyword::Enchant(enchant));
        obj.base_keywords = obj.keywords.clone();
        id
    }

    fn enchant_creature() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))
    }

    /// CR 303.4d: an Aura that's also a creature can't enchant anything, so
    /// entry-time attachment does not apply to it at all. This is the arm that
    /// must NOT be folded into CR 303.4g — the entrant is still created.
    #[test]
    fn an_aura_creature_is_not_applicable() {
        let mut state = GameState::new_two_player(1);
        creature(&mut state, P0, "Host");
        let id = aura(&mut state, P0, enchant_creature());
        state
            .objects
            .get_mut(&id)
            .expect("aura")
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert!(matches!(
            resolve_entering_aura_attachment(&mut state, id),
            EnteringAuraAttachment::NotApplicable
        ));
    }

    /// CR 303.4g: zero legal hosts is its OWN verdict, distinct from `Attached`.
    /// Reported, not acted on — the object is left exactly as it was so the
    /// caller can still decline to create it.
    #[test]
    fn no_legal_host_is_reported_and_nothing_is_attached() {
        let mut state = GameState::new_two_player(1);
        let id = aura(&mut state, P0, enchant_creature());

        assert!(matches!(
            resolve_entering_aura_attachment(&mut state, id),
            EnteringAuraAttachment::NoLegalHost
        ));
        assert!(
            state.objects[&id].attached_to.is_none(),
            "CR 303.4g: the no-host verdict must not attach anything"
        );
        assert!(
            state.objects.contains_key(&id),
            "the decision seam does not itself un-create the entrant — that is the caller's call"
        );
    }

    /// CR 303.4f: one legal host is not a choice — attach it, and say so with a
    /// verdict distinct from "there was nothing to attach to".
    #[test]
    fn a_sole_legal_host_is_attached() {
        let mut state = GameState::new_two_player(1);
        let host = creature(&mut state, P0, "Host");
        let id = aura(&mut state, P0, enchant_creature());

        assert!(matches!(
            resolve_entering_aura_attachment(&mut state, id),
            EnteringAuraAttachment::Attached
        ));
        assert_eq!(
            state.objects[&id].attached_to,
            Some(crate::game::game_object::AttachTarget::Object(host))
        );
    }

    /// CR 303.4f: more than one legal host IS a choice.
    #[test]
    fn multiple_legal_hosts_need_a_choice() {
        let mut state = GameState::new_two_player(1);
        let host_a = creature(&mut state, P0, "Host A");
        let host_b = creature(&mut state, P0, "Host B");
        let id = aura(&mut state, P0, enchant_creature());

        let EnteringAuraAttachment::NeedsChoice {
            controller,
            legal_targets,
        } = resolve_entering_aura_attachment(&mut state, id)
        else {
            panic!("two legal hosts must produce a choice");
        };
        assert_eq!(controller, P0);
        assert_eq!(
            legal_targets,
            vec![TargetRef::Object(host_a), TargetRef::Object(host_b)]
        );
        assert!(
            state.objects[&id].attached_to.is_none(),
            "an unanswered choice attaches nothing"
        );
    }

    /// CR 303.4f: "that player" is the player the Aura is entering under the
    /// control of. It is read off the OBJECT, never off the active player — which
    /// is what makes an opponent-controlled copy token prompt its own controller.
    #[test]
    fn entering_aura_hosts_reports_the_objects_own_controller() {
        let mut state = GameState::new_two_player(1);
        state.active_player = P0;
        creature(&mut state, P1, "Their A");
        creature(&mut state, P1, "Their B");
        // The Aura is P1's; a controller-scoped enchant ability therefore binds
        // to P1's creatures even though P0 is the active player.
        let id = aura(
            &mut state,
            P1,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        );

        let EnteringAuraHosts::Hosts {
            controller,
            legal_targets,
            entrant,
        } = entering_aura_hosts(&state, id)
        else {
            panic!("an unattached Aura on the battlefield has a host verdict");
        };
        assert!(
            matches!(entrant, EnteringAuraEntrant::Stored),
            "no liminal entry and no supplied projection: the act half must read \
             the stored object live, not a snapshot"
        );
        assert_eq!(
            controller, P1,
            "CR 303.4f: the chooser is the Aura's controller, not the active player"
        );
        assert_eq!(legal_targets.len(), 2);
    }

    /// The decide half is pure: asking twice does not attach anything.
    #[test]
    fn entering_aura_hosts_does_not_mutate() {
        let mut state = GameState::new_two_player(1);
        creature(&mut state, P0, "Host");
        let id = aura(&mut state, P0, enchant_creature());

        let before = state.objects[&id].clone();
        let _ = entering_aura_hosts(&state, id);
        let _ = entering_aura_hosts(&state, id);
        assert_eq!(state.objects[&id].attached_to, before.attached_to);
        assert_eq!(state.objects[&id].timestamp, before.timestamp);
    }

    /// An unattached card-backed Aura in `zone`, with `enchant creature`.
    fn card_aura(state: &mut GameState, controller: PlayerId, zone: Zone) -> ObjectId {
        let id = create_object(
            state,
            CardId(90_300),
            controller,
            "Card Aura".to_string(),
            zone,
        );
        let obj = state.objects.get_mut(&id).expect("just created");
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.keywords.push(Keyword::Enchant(enchant_creature()));
        obj.base_keywords = obj.keywords.clone();
        id
    }

    // CR 303.4g + CR 614.6: an impossible Aura entry cannot execute retained
    // replacement work. Deliver and the pending-host choice must both keep it.
    #[test]
    fn aura_host_remained_drops_retained_drain_and_population_snapshot() {
        for (origin, host_count) in [
            (Zone::Graveyard, 0),
            (Zone::Stack, 0),
            (Zone::Graveyard, 1),
            (Zone::Graveyard, 2),
        ] {
            let mut state = GameState::new_two_player(8);
            let aura = card_aura(&mut state, P0, origin);
            let hosts: Vec<_> = (0..host_count)
                .map(|_| creature(&mut state, P0, "Host"))
                .collect();
            let member = ObjectIncarnationRef::from_object(&state.objects[&aura]);
            let group = state
                .allocate_logical_zone_change_group(&[aura])
                .logical_group_id;
            let event = ProposedEvent::zone_change(aura, origin, Zone::Battlefield, Some(aura));
            let mut drains = PostReplacementDrainStack::default();
            assert!(drains.install(
                PostReplacementDrain::ready(PostReplacementContinuation::Resolved(Box::new(
                    ResolvedAbility::new(
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: TargetFilter::Controller,
                        },
                        Vec::new(),
                        aura,
                        P0
                    ),
                ))),
                ResidentDrainPolicy::KeepResident
            ));
            let life = state.players[0].life;
            let mut events = Vec::new();
            let result = deliver_replaced_zone_change_with_aura_host(
                &mut state,
                aura,
                member,
                event.clone(),
                Some(aura),
                None,
                None,
                false,
                None,
                None,
                Some(BatchMemberDeliveryOwner {
                    group,
                    member,
                    expected_event: Some(event),
                    population_devour_snapshot: Some(hosts.iter().copied().collect()),
                    approved_post_replacement: Some(drains),
                    inherited_arrangements: Vec::new(),
                }),
                &mut events,
            );
            if host_count == 0 {
                let expected = if origin == Zone::Stack {
                    Zone::Graveyard
                } else {
                    origin
                };
                assert_eq!(state.objects[&aura].zone, expected);
                assert_eq!(state.players[0].life, life);
                assert!(state.resolution_stack.is_empty());
                assert!(matches!(
                    result,
                    AuraAwareZoneDelivery::Completed(
                        ZoneMoveCompletion::Remained | ZoneMoveCompletion::Moved
                    )
                ));
                assert!(!events.iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == aura)));
            } else {
                assert_eq!(state.objects[&aura].zone, Zone::Battlefield);
                assert_eq!(state.players[0].life, life + 1);
                if host_count == 1 {
                    assert!(matches!(
                        result,
                        AuraAwareZoneDelivery::Completed(ZoneMoveCompletion::Moved)
                    ));
                    assert_eq!(
                        state.objects[&aura].attached_to,
                        Some(AttachTarget::Object(hosts[0]))
                    );
                } else {
                    assert!(matches!(result, AuraAwareZoneDelivery::NeedsAuraHostChoice));
                    assert!(
                        matches!(&state.waiting_for, WaitingFor::ReturnAsAuraTarget { legal_targets, .. }
                        if legal_targets == &hosts.iter().copied().map(TargetRef::Object).collect::<Vec<_>>())
                    );
                }
            }
        }
    }

    /// A Curse-shaped card-backed Aura in `zone`: `enchant player`, so its only
    /// candidate hosts are players.
    fn card_aura_enchanting_players(
        state: &mut GameState,
        controller: PlayerId,
        zone: Zone,
    ) -> ObjectId {
        let id = card_aura(state, controller, zone);
        let obj = state.objects.get_mut(&id).expect("just created");
        obj.name = "Card Curse".to_string();
        obj.keywords = vec![Keyword::Enchant(TargetFilter::Player)];
        obj.base_keywords = obj.keywords.clone();
        id
    }

    /// CR 303.4g: "If the Aura is a token, it isn't created." Token-ness outranks
    /// the origin — a token is never created regardless of which zone the effect
    /// was putting it onto the battlefield from.
    #[test]
    fn a_token_entrant_is_never_created_whatever_its_origin() {
        let mut state = GameState::new_two_player(1);
        let id = aura(&mut state, P0, enchant_creature());
        let entrant = state.objects[&id].clone();

        for from in [Zone::Stack, Zone::Graveyard, Zone::Exile] {
            assert!(matches!(
                unhosted_aura_entry(&entrant, from),
                UnhostedAuraEntry::NotCreated
            ));
        }
    }

    /// CR 303.4g: a card-backed Aura "remains in its current zone, unless that
    /// zone is the stack. In that case, the Aura is put into its owner's
    /// graveyard instead of entering the battlefield."
    ///
    /// Every entrant this authority answers for has a from-zone: it is asked
    /// only from the `ProposedEvent::ZoneChange` entry path. The other entry
    /// path carries a CR 111.1 `LiminalEntrant::Token`, for which the rule's
    /// token clause is the only applicable disposition, so a from-nothing
    /// card-backed entrant is not a state this authority (or the type it is
    /// asked about) can be in.
    #[test]
    fn a_card_backed_entrants_disposition_is_selected_by_its_origin() {
        let mut state = GameState::new_two_player(1);
        let id = card_aura(&mut state, P0, Zone::Graveyard);
        let entrant = state.objects[&id].clone();

        assert!(matches!(
            unhosted_aura_entry(&entrant, Zone::Graveyard),
            UnhostedAuraEntry::RemainInCurrentZone
        ));
        assert!(matches!(
            unhosted_aura_entry(&entrant, Zone::Exile),
            UnhostedAuraEntry::RemainInCurrentZone
        ));
        assert!(matches!(
            unhosted_aura_entry(&entrant, Zone::Stack),
            UnhostedAuraEntry::OwnersGraveyard
        ));
    }

    /// CR 303.4g through the real pipeline: an Aura card put onto the battlefield
    /// from a NON-stack zone with no legal host remains where it was.
    #[test]
    fn unhosted_card_aura_from_a_non_stack_zone_remains_in_that_zone() {
        let mut state = GameState::new_two_player(1);
        let id = card_aura(&mut state, P0, Zone::Graveyard);
        let mut events = Vec::new();

        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(id, Zone::Battlefield, id),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        assert_eq!(
            state.objects[&id].zone,
            Zone::Graveyard,
            "CR 303.4g: the Aura remains in its current zone"
        );
        assert!(!state.battlefield.iter().any(|&bid| bid == id));
    }

    /// CR 303.4g's stack exception, through the real pipeline: an Aura put onto
    /// the battlefield FROM THE STACK with no legal host cannot remain there — it
    /// goes to its owner's graveyard instead of entering.
    ///
    /// This is the assertion that flips when the exception is removed: before the
    /// fix this path took the same unconditional `Remained` arm as the graveyard
    /// case above and left the Aura sitting on the stack forever.
    #[test]
    fn unhosted_card_aura_from_the_stack_goes_to_its_owners_graveyard() {
        let mut state = GameState::new_two_player(1);
        let id = card_aura(&mut state, P0, Zone::Stack);
        let mut events = Vec::new();

        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(id, Zone::Battlefield, id),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        assert_eq!(
            state.objects[&id].zone,
            Zone::Graveyard,
            "CR 303.4g: a stack-origin unhosted Aura is put into its owner's graveyard"
        );
        assert!(
            state.players[0].graveyard.iter().any(|&gid| gid == id),
            "the owner's graveyard actually holds it"
        );
        assert!(
            !state.battlefield.iter().any(|&bid| bid == id),
            "it is put into the graveyard INSTEAD OF entering the battlefield"
        );
    }

    /// CR 400.7 + CR 603.6a: the CR 303.4g graveyard placement is a real zone
    /// change, so it EMITS a `ZoneChanged` that "whenever a card is put into a
    /// graveyard from anywhere" triggers can see.
    ///
    /// Honest scope: this does NOT discriminate against the destination-rewrite
    /// form this arm replaced — that delivered the same event and emitted the same
    /// pair. It pins the property against the OTHER regression available here, a
    /// raw `zones::` placement, which emits nothing at all and so fires no "put
    /// into a graveyard from anywhere" trigger. (The liminal seam's card-backed
    /// sibling was exactly such a raw placement; it no longer exists — a
    /// `ProposedEvent::TokenEntry` entrant is a CR 111.1 token by construction,
    /// so this path is the only place a CR 303.4g graveyard placement happens.)
    /// The revert-discriminating assertion for the routing change itself is in
    /// the redirect test below.
    #[test]
    fn the_stack_origin_graveyard_placement_emits_a_zone_changed() {
        let mut state = GameState::new_two_player(1);
        let id = card_aura(&mut state, P0, Zone::Stack);
        let mut events = Vec::new();

        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(id, Zone::Battlefield, id),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        assert!(
            events.iter().any(|event| matches!(
                event,
                crate::types::events::GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Stack),
                    to: Zone::Graveyard,
                    ..
                } if *object_id == id
            )),
            "CR 400.7: the graveyard placement must be observable (got {events:?})"
        );
        assert!(
            !events.iter().any(|event| matches!(
                event,
                crate::types::events::GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } if *object_id == id
            )),
            "CR 303.4g: nothing may observe the denied battlefield entry"
        );
    }

    /// CR 614.6 discriminating regression: the CR 303.4g graveyard placement is a
    /// FRESH event, so a board-wide `Moved` graveyard→exile redirect (Rest in
    /// Peace / Leyline of the Void) fires on it and the Aura ends in EXILE.
    ///
    /// The revert-failing assertion is `zone == Exile`. Rewriting the approved
    /// entry event's destination — the shape this replaced — skipped a second
    /// consult entirely, so the Aura landed in the graveyard with Rest in Peace on
    /// the battlefield. Structural twin of
    /// `engine_replacement::prevented_etb_graveyard_fallback_consults_moved_redirects`.
    #[test]
    fn the_stack_origin_graveyard_placement_consults_moved_redirects() {
        let mut state = GameState::new_two_player(1);
        // Rest in Peace-class redirect. Deliberately NOT a creature: a creature
        // would be a legal host for `enchant creature` and CR 303.4g would never
        // be reached.
        let rip = create_object(
            &mut state,
            CardId(90_400),
            P1,
            "Rest in Peace".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&rip)
            .expect("just created")
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .destination_zone(Zone::Graveyard)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Exile,
                            target: TargetFilter::SelfRef,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    )),
            );

        let id = card_aura(&mut state, P0, Zone::Stack);
        let mut events = Vec::new();

        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(id, Zone::Battlefield, id),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        assert_eq!(
            state.objects[&id].zone,
            Zone::Exile,
            "CR 614.6: the CR 303.4g graveyard placement is a fresh, replaceable \
             event — a graveyard→exile redirect must fire on it"
        );
        assert!(
            !state.players[0].graveyard.iter().any(|&gid| gid == id),
            "the Aura must not reach the graveyard with Rest in Peace out"
        );
        assert!(
            !state.battlefield.iter().any(|&bid| bid == id),
            "CR 303.4g: it still never enters the battlefield"
        );
    }

    /// Reach-guard for the redirect regression: with Rest in Peace out but a
    /// legal host present, the Aura enters normally. Without this, the exile
    /// assertion above could pass for the wrong reason (an entry blocked upstream
    /// and swept by some other rule).
    #[test]
    fn a_moved_redirect_does_not_disturb_a_hosted_entry() {
        let mut state = GameState::new_two_player(1);
        let rip = create_object(
            &mut state,
            CardId(90_400),
            P1,
            "Rest in Peace".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&rip)
            .expect("just created")
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .destination_zone(Zone::Graveyard)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Exile,
                            target: TargetFilter::SelfRef,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    )),
            );
        let host = creature(&mut state, P0, "Host");
        let id = card_aura(&mut state, P0, Zone::Stack);
        let mut events = Vec::new();

        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(id, Zone::Battlefield, id),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&id].attached_to,
            Some(crate::game::game_object::AttachTarget::Object(host))
        );
    }

    /// CR 303.4c + CR 702.16c: a player who cannot legally be enchanted is not a
    /// legal host, so an Aura whose only candidate is that player takes the
    /// CR 303.4g arm rather than entering and being swept by CR 704.5m.
    ///
    /// Revert-failing assertion: `zone == Zone::Graveyard`. Without the
    /// `can_attach_to_player` filter the protected player counted as legal, the
    /// entry was allowed, and `attach_to_player` silently no-opped on the
    /// illegality.
    #[test]
    fn a_player_host_that_cannot_be_enchanted_is_not_a_legal_host() {
        let mut state = GameState::new_two_player(1);
        let id = card_aura_enchanting_players(&mut state, P0, Zone::Stack);
        // CR 702.16j: protection from everything on BOTH players, so no player is
        // a legal host and the enchant-player filter's population is empty.
        for player in [P0, P1] {
            state.add_transient_continuous_effect(
                id,
                P0,
                crate::types::ability::Duration::UntilEndOfTurn,
                TargetFilter::SpecificPlayer { id: player },
                vec![crate::types::ability::ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(
                        crate::types::keywords::ProtectionTarget::Everything,
                    ),
                }],
                None,
            );
        }

        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(id, Zone::Battlefield, id),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        assert_eq!(
            state.objects[&id].zone,
            Zone::Graveyard,
            "CR 303.4g: no legal player host, so the stack-origin Aura is put into \
             its owner's graveyard instead of entering"
        );
        assert!(!state.battlefield.iter().any(|&bid| bid == id));
    }

    /// Reach-guard twin of the test above: the same Aura with an unprotected
    /// player present enters and attaches to that player, so the negative there
    /// is not passing because enchant-player hosts are never offered at all.
    #[test]
    fn an_unprotected_player_is_still_a_legal_host() {
        let mut state = GameState::new_two_player(1);
        let id = card_aura_enchanting_players(&mut state, P0, Zone::Stack);
        state.add_transient_continuous_effect(
            id,
            P0,
            crate::types::ability::Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: P0 },
            vec![crate::types::ability::ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(crate::types::keywords::ProtectionTarget::Everything),
            }],
            None,
        );

        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(id, Zone::Battlefield, id),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&id].attached_to,
            Some(crate::game::game_object::AttachTarget::Player(P1)),
            "CR 303.4f: the sole legal player host is attached as the Aura enters"
        );
    }

    /// Reach-guard for the two pipeline tests above: the same move with a legal
    /// host on the battlefield DOES enter and attach, so their negatives are not
    /// passing because the entry was blocked somewhere upstream of CR 303.4f/g.
    #[test]
    fn a_hosted_card_aura_from_the_stack_enters_and_attaches() {
        let mut state = GameState::new_two_player(1);
        let host = creature(&mut state, P0, "Host");
        let id = card_aura(&mut state, P0, Zone::Stack);
        let mut events = Vec::new();

        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(id, Zone::Battlefield, id),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&id].attached_to,
            Some(crate::game::game_object::AttachTarget::Object(host)),
            "CR 303.4f: the sole legal host is attached as the Aura enters"
        );
    }

    /// A Serra's Emissary-shaped permanent: its controller has CR 702.16c
    /// protection from the card type it chose as it entered (CR 205.2).
    fn chosen_card_type_protection(
        state: &mut GameState,
        controller: PlayerId,
        chosen: CoreType,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(90_400),
            controller,
            format!("Emissary vs {chosen:?}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).expect("just created");
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.chosen_attributes
            .push(ChosenAttribute::CardType(chosen));
        let protection = crate::types::ability::StaticDefinition::new(
            StaticMode::PlayerProtection(ProtectionTarget::ChosenCardType),
        )
        .affected(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
        obj.static_definitions.push(protection.clone());
        obj.base_static_definitions = std::sync::Arc::new(vec![protection]);
        crate::game::layers::mark_layers_full(state);
        crate::game::layers::flush_layers(state);
        id
    }

    /// CR 303.4f + CR 701.3b + CR 303.4i on the PLAYER half of the act half: the
    /// attach must be judged against the entrant the decide half was given, not
    /// against the object stored under the Aura's id.
    ///
    /// `attach_to_player` carries its own CR 303.4i legality gate, so the object
    /// half's fix does not cover it. This is a SEAM test rather than a
    /// production-pipeline one, and deliberately so: `player_protection_from_object`
    /// is the only projection-sensitive input to that gate, and of the qualities it
    /// implements at the player level only `ChosenCardType` reads the attachment's
    /// characteristics — while every copy exception the parser produces moves the
    /// card-type set in the RESTRICTIVE direction. No production input can
    /// therefore reach this arm today; the fixture states the seam contract
    /// directly instead of inventing a card. See the note on
    /// `yenna_aura_token_copy::chosen_player_host_resume_survives_the_color_exception`.
    ///
    /// P1 is protected from artifacts, P0 from enchantments. The STORED body is an
    /// artifact enchantment (illegal for both); the ENTRANT is a plain enchantment
    /// (illegal for P0, legal for P1) — the shape a `SetCardTypes` copy exception
    /// yields. The revert-failing assertion is `attached_to == Some(Player(P1))`:
    /// with the act half reading the stored body, P1's protection from artifacts
    /// rejects the attach and CR 701.3b leaves the Aura unattached.
    #[test]
    fn player_host_attach_uses_the_supplied_entrant() {
        let mut state = GameState::new_two_player(1);
        chosen_card_type_protection(&mut state, P0, CoreType::Enchantment);
        chosen_card_type_protection(&mut state, P1, CoreType::Artifact);
        let id = card_aura_enchanting_players(&mut state, P0, Zone::Battlefield);
        {
            let obj = state.objects.get_mut(&id).expect("aura");
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
        }
        // The CR 614.12 entrant: the same object without the artifact type.
        let mut entrant = state.objects[&id].clone();
        entrant
            .card_types
            .core_types
            .retain(|t| *t != CoreType::Artifact);
        entrant.base_card_types = entrant.card_types.clone();

        let hosts = entering_aura_hosts_projected(&state, id, &entrant);
        let EnteringAuraHosts::Hosts { legal_targets, .. } = &hosts else {
            panic!("an unattached Curse on the battlefield has a host verdict");
        };
        assert_eq!(
            legal_targets,
            &vec![TargetRef::Player(P1)],
            "reach-guard: judged against the ENTRANT, P1 is the sole legal player \
             host (P0 is protected from enchantments either way)"
        );

        assert!(matches!(
            apply_entering_aura_hosts(&mut state, id, hosts),
            EnteringAuraAttachment::Attached
        ));
        assert_eq!(
            state.objects[&id].attached_to,
            Some(crate::game::game_object::AttachTarget::Player(P1)),
            "CR 303.4i: the player gate must read the ENTRANT — the stored body's \
             artifact type is not the Aura that is entering"
        );
    }

    /// CR 303.4f: the multi-host pause parks the entrant, and only a real
    /// projection — never a `Stored` authority — is parked.
    ///
    /// Parking a snapshot for a seam whose stored object already IS the entrant
    /// would freeze characteristics the resume is better off reading live, and
    /// would change behaviour for the three pre-existing `ReturnAsAuraTarget`
    /// producers that share the resume arm.
    #[test]
    fn only_a_projected_entrant_is_parked_across_the_host_choice() {
        let mut state = GameState::new_two_player(1);
        creature(&mut state, P0, "Host A");
        creature(&mut state, P0, "Host B");
        let id = aura(&mut state, P0, enchant_creature());

        let stored_hosts = entering_aura_hosts(&state, id);
        assert!(matches!(
            apply_entering_aura_hosts(&mut state, id, stored_hosts),
            EnteringAuraAttachment::NeedsChoice { .. }
        ));
        assert!(
            state.entering_aura_authority.is_none(),
            "a `Stored` authority is never parked — the resume reads the object live"
        );

        let entrant = state.objects[&id].clone();
        let projected_hosts = entering_aura_hosts_projected(&state, id, &entrant);
        assert!(matches!(
            apply_entering_aura_hosts(&mut state, id, projected_hosts),
            EnteringAuraAttachment::NeedsChoice { .. }
        ));
        let parked = state
            .entering_aura_authority
            .as_ref()
            .expect("a projected entrant is parked for the resume");
        assert_eq!(parked.aura_id, id);

        // Spent by the resume, and honoured only for its own Aura.
        let other = aura(&mut state, P0, enchant_creature());
        assert!(
            attach_chosen_entering_aura_host(&mut state, other, &TargetRef::Object(id)).is_none()
                || state.entering_aura_authority.is_none(),
            "a resume for a different Aura must not consume the parked entrant as its own"
        );
        assert!(
            state.entering_aura_authority.is_none(),
            "the parked authority never survives a `ReturnAsAuraTarget` resume"
        );
    }
}

/// CR 708.3 + CR 708.2a: Turn an object face down as part of its battlefield
/// entry — snapshot the real face into `back_face`, then overwrite the live
/// characteristics with the face-down profile (the morph/manifest vanilla 2/2
/// plus any effect-specified extra types/subtypes) so the original is
/// restorable by `turn_face_up`. Mirrors `manifest_card`'s historical sequence.
///
/// Single authority shared by the normal delivery tail
/// (`deliver_replaced_zone_change`) and the replacement-choice resume arm
/// (`engine_replacement::handle_replacement_choice`). The resume arm previously
/// discarded the event's `face_down_profile`, so a face-down entry that parked
/// on a CR 616.1 ordering prompt (two external enter-tapped effects — Authority
/// of the Consuls + Imposing Sovereign class) resumed FACE UP, leaking the
/// morpher's hidden card.
pub(crate) fn apply_face_down_entry_profile(
    state: &mut GameState,
    object_id: ObjectId,
    profile: &FaceDownProfile,
) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        let original = crate::game::printed_cards::snapshot_object_face(obj);
        crate::game::morph::apply_face_down_creature_characteristics(obj, profile);
        // CR 708.2a: this object is now face down. `apply_face_down_creature_characteristics`
        // already raises the flag, but re-assert it here so the single authority is
        // self-sufficient: an Exile -> Battlefield entry runs `apply_zone_exit_cleanup`
        // *during* `move_to_zone`, which clears `face_down` on every exile exit
        // (CR 400.7, the foretold/exile reset). Without an explicit assertion the
        // restored face-down state would depend on a side effect of the characteristics
        // helper, so a future change to that helper could silently leak the entrant
        // face up. The early pre-flag in `deliver_replaced_zone_change` only has to
        // survive the entry guard (which runs before exit cleanup); this is the
        // authoritative final assertion that survives it.
        obj.face_down = true;
        // The public record of WHICH keyword action put this permanent face
        // down. Re-stamped on every face-down entry, and only meaningful while
        // `face_down` is true — the many turn-face-up paths leave it alone
        // rather than each having to remember to clear it.
        obj.face_down_cause = Some(profile.cause);
        obj.back_face = Some(original);
    }
}

/// CR 730.3e (second clause) + CR 730.2d + CR 614.6: compute the card-component
/// routing override for a merged permanent's leave.
///
/// `survivor_dest` is the merged permanent's already-consulted destination (the
/// survivor's post-replacement `to`). For a NON-token survivor every component
/// followed `survivor_dest` (clause 1, CR 730.3d) and this returns `None`. For a
/// TOKEN survivor (CR 730.2d: token iff the topmost component is a token), a
/// card-scoped (`NonToken`) `Moved` redirect did NOT match the survivor — so
/// `survivor_dest` is the pre-replacement default zone — but it DOES move the
/// merged permanent's CARD components. We discover where by running ONE
/// component-aware consult for a representative card component: a single
/// `replace_event` over a `ZoneChange { from: Battlefield, to: survivor_dest }`
/// proposal for that card. This is NOT a per-component re-consult — CR 616.1
/// ordering is resolved once for the card partition, never per card — and it
/// only READS the resolved destination (replacement does not move the object).
///
/// Returns `Some` only when the card consult diverges from `survivor_dest`
/// (i.e. a card-scoped redirect genuinely applies to cards but not the token
/// survivor); otherwise `None` (no override — the existing single-`to` routing
/// is already correct).
///
/// LIMITATION (homogeneous card partition): the representative-component consult
/// applies one card component's resolved destination to the ENTIRE card
/// partition. This is exact when every card component matches the card-scoped
/// redirect identically — true for the common case (RIP/Leyline "a card …"
/// matches every non-token) and for Mutate piles versus type-level filters (all
/// components are creatures). It can misroute only a heterogeneous partition
/// under a subtype/color-scoped card redirect (e.g. a green creature card merged
/// with a red creature card under a TOKEN survivor, versus "if a green creature
/// card would be put into a graveyard"): the off-filter card component would
/// follow the representative's redirect instead of its own default. Fully
/// correct per-component routing would evaluate each card component's filter
/// individually while resolving CR 616.1 ordering only once — deferred, because
/// per-component re-consults re-burn that ordering choice (the OQ#5
/// single-consult mandate) and the misroute requires a token-survivor Mutate
/// pile with mixed card characteristics under a scoped graveyard-redirect, which
/// no current card produces.
///
/// `// strict-failure: a one-shot ("the next time ... instead") leave redirect
/// would be consumed by this extra read-only consult; no such depletion-style
/// def is in the merged-leave class (the graveyard-redirect hosers are
/// continuous statics), so the double-stamp is benign.`
fn compute_merged_card_component_route(
    state: &mut GameState,
    survivor_id: ObjectId,
    survivor_dest: Zone,
    events: &mut Vec<GameEvent>,
) -> Option<MergedCardComponentRoute> {
    let survivor = state.objects.get(&survivor_id)?;
    // Clause 1 (CR 730.3d) already routed every component to `survivor_dest`
    // for a non-token survivor; only the token-survivor case needs the split.
    if !survivor.is_token || survivor.merged_components.is_empty() {
        return None;
    }
    // A representative CARD (non-token) component, excluding the survivor.
    let card_component = survivor
        .merged_components
        .iter()
        .copied()
        .find(|&id| id != survivor_id && state.objects.get(&id).is_some_and(|o| !o.is_token))?;

    // Single component-aware consult for the card partition. The card component
    // is still absorbed (on the battlefield via the survivor), so its leave
    // origin is the battlefield.
    let proposed = ProposedEvent::zone_change(
        card_component,
        Zone::Battlefield,
        survivor_dest,
        Some(survivor_id),
    );
    let card_dest = match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(ProposedEvent::ZoneChange { to, .. }) => to,
        // Prevented / NeedsChoice / non-ZoneChange: no usable redirect for the
        // card partition — fall back to the survivor's destination (no split).
        // strict-failure: a NeedsChoice here means the card partition matched an
        // Optional-mode def or a CR 616.1 ordering choice between multiple Moved
        // candidates; the fallback skips that genuine choice (rules-wrong for
        // the rare multi-candidate case) as the safe floor versus pausing
        // mid-delivery. `pipeline_loop` parks `pending_replacement` BEFORE
        // returning NeedsChoice — clear it, or the stranded record silently
        // truncates every SBA pass (sba.rs gates on `pending_replacement`) for
        // the rest of the game and serializes as garbage into saves.
        _ => {
            state.pending_replacement = None;
            return None;
        }
    };

    (card_dest != survivor_dest).then_some(MergedCardComponentRoute {
        default_dest: survivor_dest,
        card_dest,
    })
}

/// The player who owns `object_id` once it enters `to`, when that differs from
/// its current owner.
///
/// Under `EntryOwnership::ActorOwns` the performer becomes the owner of a card
/// it takes into hand out of a container both seats read as one (CR 400.3
/// otherwise routes it to its owner's hand), of a spell it casts (CR 601.2a),
/// and of a land it plays (CR 305.1). A Battlefield entry rebinds only when the
/// event has no source object: effect moves (Reanimate, blink) carry one and
/// keep the owner, the land-play special action (CR 116.1) carries none.
fn entry_receiver(
    state: &GameState,
    object_id: ObjectId,
    to: Zone,
    performer: Option<PlayerId>,
    cause: Option<ObjectId>,
) -> Option<PlayerId> {
    let performer = performer?;
    match state.format_config.format.entry_ownership() {
        EntryOwnership::OwnerKept => return None,
        EntryOwnership::ActorOwns => {}
    }
    let object = state.objects.get(&object_id)?;
    let rebinds = match to {
        Zone::Hand => state.object_in_players_zone(object, object.zone, performer),
        Zone::Stack => true,
        Zone::Battlefield => cause.is_none(),
        Zone::Library | Zone::Graveyard | Zone::Exile | Zone::Command => false,
    };
    (rebinds && object.owner != performer).then_some(performer)
}

/// Deliver a zone-change event that has already passed through replacement.
///
/// The event's `library_placement` (CR 701.24a): when its delivered destination is
/// the library AND a specific position was requested, the object is placed at
/// that index and the library is NOT shuffled — a placement instruction is not a
/// shuffle instruction (CR 701.24a defines shuffling). `None` = the zone-default
/// placement, which the tail's auto-shuffle convention then randomizes. A
/// `Moved` replacement may have redirected the event to a non-library zone; the
/// placement then has no effect (the index/shuffle gates both key on
/// `to == Zone::Library`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn deliver_replaced_zone_change(
    state: &mut GameState,
    event: ProposedEvent,
    source_id: Option<ObjectId>,
    duration: Option<&Duration>,
    exile_controller: Option<PlayerId>,
    track_exiled_by_source: bool,
    drain: PostReplacementDrainOwner,
    _legacy_library_placement: Option<LibraryPosition>,
    events: &mut Vec<GameEvent>,
) -> ZoneDeliveryResult {
    deliver_replaced_batch_member(
        state,
        event,
        source_id,
        duration,
        exile_controller,
        track_exiled_by_source,
        drain,
        _legacy_library_placement,
        None,
        None,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
fn deliver_replaced_batch_member(
    state: &mut GameState,
    event: ProposedEvent,
    source_id: Option<ObjectId>,
    duration: Option<&Duration>,
    exile_controller: Option<PlayerId>,
    track_exiled_by_source: bool,
    drain: PostReplacementDrainOwner,
    _legacy_library_placement: Option<LibraryPosition>,
    // Exact insertion index for a grouped fixed-position library member.
    explicit_library_index: Option<usize>,
    owner: Option<BatchMemberDeliveryOwner>,
    events: &mut Vec<GameEvent>,
) -> ZoneDeliveryResult {
    let component_parent = owner.map(|owner| PendingComponentDeliveryParent {
        parent_group_id: owner.group,
        member: owner.member,
        expected_event: owner.expected_event.unwrap_or_else(|| event.clone()),
        approved_event: event.clone(),
        inherited_arrangements: owner.inherited_arrangements,
    });
    if let ProposedEvent::ZoneChange {
        object_id,
        from,
        to,
        library_placement,
        cause,
        attach_to,
        enter_transformed: should_transform,
        enter_tapped: should_tap,
        enters_attacking,
        enter_with_counters,
        controller_override: ctrl_override,
        face_down_profile,
        face_down_in_exile,
        chain_referent,
        enter_as_copy,
        enter_with_choices,
        discard_frame,
        performed_by,
        applied,
        ..
    } = event
    {
        if let Some(entry) = state.liminal_entries.get_mut(&object_id) {
            entry.replacement_applied = applied.clone();
        }
        let exile_tracking = if track_exiled_by_source {
            ZoneDeliveryExileTracking::TrackBySource
        } else {
            ZoneDeliveryExileTracking::None
        };
        // CR 701.9a + CR 400.7: Capture the card while it is still in hand.
        // The discard frame, not a current-zone lookup, owns the eventual
        // contingent condition's facts through redirects and replacement pauses.
        let discard_lki = discard_frame.and_then(|_| {
            (from == Zone::Hand)
                .then(|| {
                    state
                        .objects
                        .get(&object_id)
                        .map(|object| object.snapshot_for_mana_spent())
                })
                .flatten()
        });

        let merged_permanent_leave = from == Zone::Battlefield
            && state
                .objects
                .get(&object_id)
                .is_some_and(|object| !object.merged_components.is_empty());
        if merged_permanent_leave {
            // CR 730.3d + CR 903.9c: the merged permanent's already-approved
            // event is expanded into a single pausable batch. Each component
            // inherits `applied`, so a replacement that affected the merged
            // event is not consulted again; the batch nevertheless consults
            // component-specific replacements, including CR 903.9b.
            state.merged_card_component_route = None;
            return match crate::game::merge::move_merged_permanent_for_parent(
                state,
                object_id,
                to,
                library_placement.as_ref(),
                &applied,
                component_parent,
                events,
            ) {
                BatchMoveResult::Done => apply_zone_delivery_tail(
                    state,
                    object_id,
                    from,
                    to,
                    cause,
                    source_id,
                    duration,
                    exile_controller,
                    exile_tracking,
                    drain,
                    library_placement.as_ref(),
                    events,
                ),
                BatchMoveResult::NeedsChoice => replacement_pause_delivery_result(state),
            };
        }

        let split_component_survivor = state.objects.get(&object_id).and_then(|object| {
            (from == Zone::Battlefield
                && object.zone == Zone::Battlefield
                && !state.battlefield.contains(&object_id))
            .then_some(object.split_from_merge_survivor)
            .flatten()
        });

        // CR 614.1c: Static replacement effects that modify how an object enters
        // must already be functioning before that object enters. Snapshot the
        // definitions before `move_to_zone` so a newly-entered permanent cannot
        // retroactively supply its own replacement effect.
        let enters_with_additional_counter_statics: Vec<_> = if to == Zone::Battlefield {
            crate::game::functioning_abilities::game_active_statics(state)
                .filter(|(_, def)| {
                    matches!(
                        def.mode,
                        crate::types::statics::StaticMode::EntersWithAdditionalCounters { .. }
                    )
                })
                .map(|(source_obj, def)| (source_obj.id, def.clone()))
                .collect()
        } else {
            Vec::new()
        };

        // CR 614.12a + CR 614.13a: snapshot the pre-entry eligible pool the instant
        // before the FIRST co-entering devourer enters; persisted (is_none gate) so all
        // co-entering devourers share it. Excludes self + every co-arriver.
        if to == Zone::Battlefield
            && state.active_devour_eligible_snapshot().is_none()
            && crate::game::engine_replacement::object_has_devour_replacement(state, object_id)
        {
            state.push_devour_change_zone_snapshot(state.battlefield.iter().copied().collect());
        }

        // CR 400.7d + CR 608.3: a permanent spell's resolution turns the spell
        // into the permanent, and an ability of that permanent may reference
        // information about the spell that became it — including what costs
        // were paid (kicker, additional costs, convoke) and how it was cast.
        // `reset_for_battlefield_entry` (CR 400.7) clears that cast-link family
        // on entry, so snapshot it from the pre-move STACK object and restore
        // it right after the move. Gated on `from == Stack`: establishment is
        // exclusive to the cast pathway (`finalize_cast_to_stack` stamps the
        // stack object), and an effect-driven put (Reanimate class) must NOT
        // resurrect stale cast provenance — its entry is a new object with no
        // cast linkage (CR 400.7, no exception applies).
        let cast_link = (from == Zone::Stack && to == Zone::Battlefield)
            .then(|| {
                state.objects.get(&object_id).map(|obj| CastLinkSnapshot {
                    cast_from_zone: obj.cast_from_zone,
                    cast_controller: obj.cast_controller,
                    cast_timing_permission: obj.cast_timing_permission.map(|(p, _)| p),
                    kickers_paid: obj.kickers_paid.clone(),
                    gift_recipient: obj.gift_recipient,
                    additional_cost_payment_count: obj.additional_cost_payment_count,
                    additional_cost_payments: obj.additional_cost_payments.clone(),
                    convoked_creatures: obj.convoked_creatures.clone(),
                    cast_cost_paid_object: obj.cast_cost_paid_object.clone(),
                })
            })
            .flatten();

        // CR 730.3e (second clause): if a TOKEN merged permanent leaves the
        // battlefield while a card-scoped (`NonToken`) `Moved` redirect is
        // active, the redirect did NOT match the token survivor (so `to` above
        // is the pre-replacement default zone for the survivor + its token
        // components), but it DOES move the merged permanent's CARD components.
        // Run ONE additional component-aware consult here (NOT per component —
        // a single `replace_event` for the card-component partition, so CR 616.1
        // ordering is computed once for the partition, not re-burned per card),
        // and stash the resulting `card_dest` so the survivor split routes card
        // components there while the token survivor + token components take the
        // default zone. A no-op (no route stashed) for non-token survivors
        // (clause 1, already handled — every component followed the survivor's
        // redirected `to`) and when no card-scoped redirect diverges.
        state.merged_card_component_route =
            compute_merged_card_component_route(state, object_id, to, events);

        // CR 701.24a: deliver to a specific library index when the event's
        // destination is the library and a position was requested (a placement is
        // not a shuffle); otherwise the zone-default `move_to_zone` (which the
        // tail then auto-shuffles per CR 701.24a — shuffling = randomizing so no
        // player knows the order). `move_to_library_at_index` performs the same full
        // cross-zone cleanup (LKI, transform revert, layer pruning) as
        // `move_to_zone` — it differs only in placing at an index instead of
        // shuffling. A `Moved` redirect may have changed `to` away from Library,
        // in which case the placement is inert and the default mover runs.
        // CR 708.2a + CR 304.4 / CR 400.4a: A card put onto the battlefield face
        // down enters as a 2/2 creature, so the instant/sorcery battlefield-entry
        // guard in `move_to_zone` must not reject it. The full face-down profile
        // is applied just after the move (below), but that guard runs *inside*
        // `move_to_zone` and only reads `face_down` — which is still false there.
        // Flag the object face down up front so a non-permanent (instant/sorcery)
        // manifest/morph entry isn't bounced back to its origin zone. A
        // Library/Hand -> Battlefield manifest never hits the face_down-clearing
        // reset branches (those key on `from` == Exile/Battlefield/Stack), so the
        // flag survives until the profile is applied.
        // Snapshot the pre-move `face_down` so the preflight flag set below can be
        // rolled back if the battlefield entry is ultimately rejected: a
        // `CantEnterBattlefieldFrom` static such as Grafdigger's Cage makes
        // `move_to_zone` early-return WITHOUT moving the object (CR 614.1d), and a
        // blocked manifest/morph entry must not strand the card face down in its
        // origin zone.
        let face_down_preflight = to == Zone::Battlefield && face_down_profile.is_some();
        let prior_face_down = if face_down_preflight {
            state.objects.get(&object_id).map(|obj| obj.face_down)
        } else {
            None
        };
        if face_down_preflight {
            if let Some(obj) = state.objects.get_mut(&object_id) {
                obj.face_down = true;
                // CR 708.3: turned face down before it enters, so the origin-exit LKI and the
                // zone-change record `move_to_zone` captures next carry no printing. A rejected
                // entry rolls back only the flag.
                obj.conceal_printing();
            }
        }
        // pod-lab loop-3 Q5: tracks whether this delivery took the plain,
        // non-merge, non-library-placement `move_to_zone` branch — the ONLY
        // branch whose own internal dirty-mark decision (see the carve-out
        // added to `move_to_zone` above) is trustworthy enough to let the
        // redundant check below skip re-marking `Full`. `false` for both the
        // library-placement branch and the merge-survivor branch, neither of
        // which is analyzed by that carve-out.
        let took_plain_zone_transfer;
        match (split_component_survivor, to, library_placement.as_ref()) {
            (Some(_), _, position) => {
                // CR 730.3 + CR 730.3c: an absorbed component arrives without
                // independently leaving the battlefield; retain its successor
                // link and sole incarnation bump in the existing component mover.
                took_plain_zone_transfer = false;
                crate::game::merge::put_component_into_zone(state, object_id, to, events);
                if let (Zone::Library, Some(position)) = (to, position) {
                    let owner = state.objects[&object_id].owner;
                    let index = explicit_library_index.or_else(|| library_position_index(position));
                    zones::reorder_within_library(state, owner, &[object_id], index);
                }
            }
            (None, Zone::Library, Some(position)) => {
                took_plain_zone_transfer = false;
                let index = explicit_library_index.or_else(|| library_position_index(position));
                zones::move_to_library_at_index(state, object_id, index, events);
            }
            (None, _, _) => {
                took_plain_zone_transfer = true;
                // CR 712.14a: carry the effect-driven "enters transformed"
                // intent into the battlefield-entry guard so a non-permanent
                // FRONT face (e.g. instant/sorcery) may enter as its PERMANENT
                // back face. `should_transform` is destructured from
                // `ProposedEvent::ZoneChange.enter_transformed` above; the
                // flag is inert for any non-battlefield destination (the guard
                // gates on `to == Battlefield`).
                let receiver = entry_receiver(state, object_id, to, performed_by, cause);
                zones::move_to_zone_with_entry_flags(
                    state,
                    object_id,
                    to,
                    events,
                    should_transform,
                    receiver,
                );
            }
        }
        if face_down_in_exile.is_face_down() && to == Zone::Exile {
            zones::mark_face_down_in_exile(state, events, object_id);
            crate::game::exile_links::link_search_look(state, object_id, source_id);
        }
        // CR 730.3e: the survivor split (inside `move_to_zone` above) has consumed
        // any clause-2 routing override; clear it so it never leaks into a later
        // unrelated move. Purely synchronous lifetime (set → consumed → cleared in
        // this one delivery), so it never crosses a pause.
        state.merged_card_component_route = None;
        // CR 406.6 + CR 400.8 + CR 608.2c: an object that settles in exile —
        // including one exiled again from exile, which becomes a new object
        // that has just been exiled — records the player who performed this
        // move as the player who exiled it. `move_to_zone` cleared any prior
        // record on the way out (CR 400.7), so only this delivery's performer
        // survives.
        if to == Zone::Exile {
            if let Some(player) = performed_by {
                crate::game::exile_links::record_exiling_player(state, object_id, player);
            }
        }
        // CR 614.17d: determine whether the object actually entered the battlefield.
        // `move_to_zone` rejects a battlefield entry without moving the object when
        // a `CantEnterBattlefieldFrom` static (e.g. Grafdigger's Cage) matches, so
        // a `to == Battlefield` request can leave the object in its origin zone.
        let entered_battlefield = to == Zone::Battlefield
            && state
                .objects
                .get(&object_id)
                .is_some_and(|obj| obj.zone == Zone::Battlefield);
        // CR 701.9a + CR 614.1: The inner move has now completed with its
        // final replacement-selected destination. Append one operation-owned
        // result exactly once; a prevented move never reaches this delivery.
        if let (Some(frame_id), Some(lki), Some(final_zone)) = (
            discard_frame,
            discard_lki,
            state.objects.get(&object_id).map(|object| object.zone),
        ) {
            if final_zone != Zone::Hand {
                let (recorded, source_id) = {
                    let frame = state
                        .resolution_stack
                        .active_discard_or_direct_continuation_parent_mut(frame_id)
                        .expect("discard provenance must name the active discard operation");
                    let recorded = frame.results.is_empty();
                    let source_id = frame.source_id;
                    if recorded {
                        frame
                            .results
                            .push(crate::types::ability::DiscardedCardResult {
                                object_id,
                                lki: lki.clone(),
                                final_zone,
                            });
                    }
                    (recorded, source_id)
                };
                if recorded {
                    crate::game::restrictions::record_discard(state, lki.owner);
                    if final_zone == Zone::Graveyard {
                        crate::game::restrictions::record_card_discarded(state, object_id);
                    }
                    events.push(GameEvent::Discarded {
                        player_id: lki.owner,
                        object_id,
                        source_id,
                    });
                }
            }
        }
        // Roll back the face-down preflight flag when the entry was rejected, so a
        // blocked manifest/morph leaves the card unchanged in its origin zone
        // rather than stranded face down (corrupting hidden state for a move that
        // never happened). On a successful entry the flag is re-asserted by
        // `apply_face_down_entry_profile` below, so this restore is inert.
        if face_down_preflight && !entered_battlefield {
            if let (Some(prior), Some(obj)) = (prior_face_down, state.objects.get_mut(&object_id)) {
                obj.face_down = prior;
            }
        }
        // CR 400.7d: restore the cast link immediately after the entry reset —
        // BEFORE the face-down / counter blocks, so a counter-replacement pause
        // (CR 616.1) cannot strand the resumed permanent without its kicker /
        // convoke / cast-timing memory (the pre-pipeline stack.rs epilogue ran
        // after the counter blocks and was skipped by their early returns).
        if let Some(link) = cast_link {
            if let Some(obj) = state.objects.get_mut(&object_id) {
                obj.cast_from_zone = link.cast_from_zone;
                obj.cast_controller = link.cast_controller;
                // CR 603.4: trigger conditions compare the stamp against the
                // CURRENT turn (`triggers.rs` reads `(permission, turn)`), so
                // re-stamp with the resolution turn — mirroring the
                // `apply_pending_spell_resolution` restore. Cast turn and
                // resolution turn are always equal (the stack empties before a
                // turn ends), so this also preserves the captured value.
                if let Some(permission) = link.cast_timing_permission {
                    obj.cast_timing_permission = Some((permission, state.turn_number));
                }
                obj.kickers_paid = link.kickers_paid;
                obj.gift_recipient = link.gift_recipient;
                obj.additional_cost_payment_count = link.additional_cost_payment_count;
                obj.additional_cost_payments = link.additional_cost_payments;
                obj.convoked_creatures = link.convoked_creatures;
                obj.cast_cost_paid_object = link.cast_cost_paid_object;
            }
        }
        // CR 707.10f + CR 608.3f: The is_copy→is_token flip for a resolving
        // permanent-spell copy now happens UPSTREAM in `stack.rs::resolve_top`,
        // at the top of the `dest == Zone::Battlefield` block — BEFORE the
        // ProposedEvent is built, before `replace_event` matches the ZoneChange,
        // and before the zone-change record snapshots is_token. That is the sole
        // path a copy (only ever created on the stack by `Effect::CastCopyOfCard`)
        // reaches the battlefield, so no un-flipped copy can arrive here.
        // pod-lab loop-3 Q5: `move_to_zone` (above) already made the correct,
        // precise dirty-mark decision for a plain transfer that actually
        // landed on the battlefield — this check no longer re-clobbers it to
        // `Full`. Gated on `entered_battlefield && took_plain_zone_transfer`
        // together, not `took_plain_zone_transfer` alone: a rejected entry
        // (Grafdigger's Cage-class `CantEnterBattlefieldFrom`, CR 614.1d) has
        // `entered_battlefield == false` and must keep this unconditional
        // mark, since `move_to_zone` never reached its own mark block for a
        // rejected entry at all. A merge-survivor delivery or a
        // library-placement delivery is `took_plain_zone_transfer == false`
        // and is untouched, exactly as before.
        if from == Zone::Battlefield
            || (to == Zone::Battlefield && !(entered_battlefield && took_plain_zone_transfer))
        {
            crate::game::layers::mark_layers_full(state);
        }
        // CR 708.3: An object put onto the battlefield face down is turned face
        // down BEFORE it enters, so its ETB abilities don't trigger and its
        // characteristics are the face-down profile (CR 708.2a), not the real
        // card's. Done before the controller-override and ETB-counter/trigger
        // blocks below so triggers (if any later applied) see the face-down
        // state. Shared single authority with the replacement-choice resume arm
        // (`engine_replacement::handle_replacement_choice`), so a paused
        // face-down entry cannot resume face-up.
        //
        // Gated on `entered_battlefield` (not merely `to == Battlefield`): if a
        // `CantEnterBattlefieldFrom` static rejected the entry, the object is still
        // in its origin zone, and applying the face-down profile there would morph
        // a card that never moved (CR 614.1d). Combined with the preflight rollback
        // above, a blocked manifest/morph leaves the card fully unchanged.
        if entered_battlefield {
            if let Some(profile) = &face_down_profile {
                apply_face_down_entry_profile(state, object_id, profile);
            }
            // CR 608.2c: a permanent the instruction just produced is the
            // chain's most-recent created referent, so a following "it" / "that
            // creature" anaphor (`TargetFilter::LastCreated`) binds to it —
            // "manifest dread, then attach this Equipment to that creature"
            // (#7531).
            //
            // Keyed on the intent the REQUEST carried, not on any property of
            // the entrant: two effects can deliver an identical face-down
            // permanent and only one of them be the producer the sentence
            // refers back to. Published here rather than at the producing
            // effect so the synchronous arm, the manifest-dread continuation
            // and the CR 616.1 parked-entry resume all reach it — the intent
            // rides the parked event with the rest of the request — and only
            // once the entry has actually settled (`entered_battlefield`), so a
            // `CantEnterBattlefieldFrom` rejection publishes nothing.
            if chain_referent.publishes() {
                crate::game::morph::publish_face_down_entry_referent(state, object_id);
            }
        }
        // CR 614.12a + CR 400.7: the entrant's as-enters choices were made
        // before it entered and rode the event; install them on the new object
        // (its `chosen_attributes` were reset as it entered) before ETB counters
        // and triggers observe it. CR 613.1: they can gate layer effects.
        if let (true, Some(choices)) = (entered_battlefield, enter_with_choices) {
            if let Some(object) = state.objects.get_mut(&object_id) {
                object.chosen_attributes.extend(choices.0);
            }
            crate::game::layers::mark_layers_full(state);
        }
        // CR 614.12a + CR 616.1c + CR 707.2: An enter-as-copy replacement
        // selected its copy source before this delivery and carried those
        // copiable values on the proposed event. Install the copy effect before
        // ETB counters/triggers run so the permanent is observed as the copied
        // object as it enters, without overwriting its printed/base identity.
        if entered_battlefield {
            if let Some(copy) = enter_as_copy {
                let copy = *copy;
                let payload = crate::game::effects::become_copy::PrecomputedCopyValues {
                    source_id: copy.source_id,
                    controller: copy.controller,
                    // CR 400.7 + CR 611.2b: this effect applies to the
                    // entering permanent, not to the copied object (which can
                    // be absent by the time this replacement is delivered,
                    // e.g. Mystic Reflection's resolved source).
                    duration_subject: ObjectIncarnationRef::from_object(&state.objects[&object_id]),
                    duration: copy.sacrifice_at.unwrap_or(Duration::Permanent),
                    values: *copy.values,
                    // The recipient keeps its own base (Clone is a 0/0
                    // Shapeshifter underneath), so it rides the source's
                    // captured descriptor — or the legacy live-field search
                    // when the source had none — exactly like the exact refs.
                    // Created copy-tokens never pass through here; their
                    // descriptor is derived by the creation injectors.
                    display: copy.display,
                    additional_modifications: copy.additional_modifications,
                    effect_kind: EffectKind::BecomeCopy,
                };
                let _ = crate::game::effects::become_copy::apply_precomputed_copy_values(
                    state, object_id, payload, events,
                );
            }
        }
        // CR 712.14a: A permanent put onto the battlefield transformed enters back face up.
        if should_transform && to == Zone::Battlefield {
            if let Some(obj) = state.objects.get(&object_id) {
                if obj.back_face.is_some() && !obj.transformed {
                    let _ = crate::game::transform::enter_back_face_up(state, object_id);
                }
            }
        }
        // CR 614.1: Apply enter-tapped if the effect or replacement set it.
        // CR 701.26a: Only an untapped permanent can be tapped, so route the
        // entry tap through the single object-status authority — it captures
        // the exact incarnation and prior state as a resolved command instead
        // of writing `tapped` raw. The existence guard preserves the prior
        // silent skip when the object is no longer present.
        if should_tap.resolve(false)
            && to == Zone::Battlefield
            && state.objects.contains_key(&object_id)
        {
            crate::game::object_state::resolve_and_apply_object_edit(
                state,
                object_id,
                crate::types::resolved_commands::ResolvedObjectStatus::Tapped,
                true,
            )
            .expect("an entering permanent must satisfy the resolved tap precondition");
        }
        // CR 603.6a + CR 400.7: Record which ability placed this permanent so
        // anti-recursion intervening-ifs ("if it wasn't put onto the battlefield
        // with this ability") can exclude permanents this very ability placed.
        // `move_to_zone` already ran `reset_for_battlefield_entry` (clearing the
        // field to None); set it only for ability-effect-driven entries. This is
        // synchronous and lands before `process_triggers`, so the field is
        // visible at ETB trigger fire-time (CR 603.4).
        if to == Zone::Battlefield {
            if let Some(src) = source_id {
                // CR 733: route the stamp through its single authority so the
                // provenance is captured as a resolved command instead of written
                // raw. The authority keeps the prior silent skip when the object
                // is no longer present.
                zones::stamp_battlefield_entry_provenance(state, object_id, src);
            }
        }
        // CR 110.2a: Apply controller override if the effect specifies
        // "under your control" — set before triggers fire.
        if let Some(new_controller) = ctrl_override {
            if to == Zone::Battlefield {
                zones::apply_battlefield_entry_controller_override(
                    state,
                    events,
                    object_id,
                    new_controller,
                );
            }
        }
        // CR 303.4f + CR 701.3a: A non-spell Aura entry carries its chosen
        // enchant host through the ZoneChange event so it is attached before
        // the effect finishes resolving.
        if to == Zone::Battlefield {
            if let Some(target) = attach_to {
                match target {
                    crate::game::game_object::AttachTarget::Object(target_id) => {
                        let _ =
                            crate::game::effects::attach::attach_to(state, object_id, target_id);
                    }
                    crate::game::game_object::AttachTarget::Player(player_id) => {
                        let _ = crate::game::effects::attach::attach_to_player(
                            state, object_id, player_id,
                        );
                    }
                }
            }
        }
        // CR 614.1c: Apply counters from replacement pipeline (e.g., saga lore counters,
        // planeswalker intrinsic loyalty, battle intrinsic defense).
        // CR 614.17c + CR 614.17d + CR 122.6: gated on `entered_battlefield`,
        // not `to == Battlefield`. When `move_to_zone` refused the entry (a
        // "can't enter the battlefield" static of the Grafdigger's Cage class,
        // or any other rejection that leaves the object in its origin zone), no
        // event put it onto the battlefield for an enters-with replacement to
        // modify: the object gets none of these counters, and its
        // `pending_etb_counters` are not consumed.
        if entered_battlefield {
            let mut counters_to_apply = enter_with_counters;
            // CR 614.1c + CR 122.1: Apply additional counters from continuous
            // "[scope] creatures you control enter with an additional [counter]
            // counter on them" statics (Kalain, Bard Class, Gorma the Gullet,
            // Master Chef). These are replacement effects whose affected filter
            // matches the entering object; folded through the shared resolver so
            // counter-doubling replacements (Doubling Season, Hardened Scales)
            // see them too.
            let additional = enters_with_additional_counters_for_entry(
                state,
                object_id,
                &enters_with_additional_counter_statics,
            );
            counters_to_apply.extend(additional);
            // CR 614.1c: Apply pending ETB counters from delayed triggers
            // (e.g., "that creature enters with an additional +1/+1 counter").
            let pending: Vec<_> = state
                .pending_etb_counters
                .iter()
                .filter(|(oid, _, _)| *oid == object_id)
                .map(|(_, ct, n)| (ct.clone(), *n))
                .collect();
            let pending_etb_cleanup = if pending.is_empty() {
                None
            } else {
                Some(object_id)
            };
            counters_to_apply.extend(pending);
            if !counters_to_apply.is_empty()
                && !crate::game::engine_replacement::apply_etb_counters(
                    state,
                    object_id,
                    &counters_to_apply,
                    events,
                )
            {
                return append_zone_delivery_tail_after_counter_pause(
                    state,
                    object_id,
                    from,
                    to,
                    cause,
                    source_id,
                    duration,
                    exile_controller,
                    exile_tracking,
                    drain,
                    enters_attacking,
                    pending_etb_cleanup,
                );
            }
            if pending_etb_cleanup.is_some() {
                state
                    .pending_etb_counters
                    .retain(|(oid, _, _)| *oid != object_id);
            }
        } else if to != Zone::Battlefield && !enter_with_counters.is_empty() {
            // CR 122.1: Effect-driven counters for non-battlefield
            // destinations — e.g., "exile it with three egg counters
            // on it" (Darigaaz Reincarnated), Delay's three time
            // counters (issue #8795). Apply directly via the shared
            // single-authority resolver so event emission and the
            // replacement consult stay consistent — where CR 109.2 /
            // CR 110.1 make "a permanent" replacements (Doubling
            // Season, Hardened Scales) decline a card that is not on
            // the battlefield (`replacement_valid_card_matches`, and
            // `object_replacement_candidate_applies` for a replacement
            // without a `valid_card`).
            if !crate::game::engine_replacement::apply_etb_counters(
                state,
                object_id,
                &enter_with_counters,
                events,
            ) {
                return append_zone_delivery_tail_after_counter_pause(
                    state,
                    object_id,
                    from,
                    to,
                    cause,
                    source_id,
                    duration,
                    exile_controller,
                    exile_tracking,
                    drain,
                    enters_attacking,
                    None,
                );
            }
        }
        let result = apply_zone_delivery_tail(
            state,
            object_id,
            from,
            to,
            cause,
            source_id,
            duration,
            exile_controller,
            exile_tracking,
            drain,
            library_placement.as_ref(),
            events,
        );
        if matches!(result, ZoneDeliveryResult::Done) && enters_attacking && entered_battlefield {
            let controller = state
                .objects
                .get(&object_id)
                .map(|object| object.controller)
                .expect("a settled battlefield entrant must exist");
            if let Some(player) = crate::game::combat::choose_entry_attack_target_or_enter(
                state, object_id, controller,
            ) {
                return ZoneDeliveryResult::NeedsChoice(player);
            }
        }
        return result;
    }
    ZoneDeliveryResult::Done
}

/// CR 401.7: a stated library depth is bounded by the insertion/reorder authority.
fn library_position_index(position: &LibraryPosition) -> Option<usize> {
    match position {
        LibraryPosition::Top => Some(0),
        LibraryPosition::Bottom => None,
        LibraryPosition::NthFromTop { n } => Some(n.saturating_sub(1) as usize),
        // Evaluated depths arrive from the originating resolver. Preserve the
        // existing fallback for forms that never use this terminal path.
        LibraryPosition::BeneathTop { depth } => match depth {
            crate::types::ability::QuantityExpr::Fixed { value } => Some((*value).max(0) as usize),
            _ => None,
        },
        LibraryPosition::RandomWithinTop { .. } => None,
    }
}

fn replacement_pause_delivery_result(state: &GameState) -> ZoneDeliveryResult {
    match &state.waiting_for {
        WaitingFor::ReplacementChoice { player, .. }
        | WaitingFor::EntryControllerChoice { player, .. }
        // CR 614.12a: a Devour as-enters sacrifice surfaced its own
        // `EffectZoneChoice`; carry its chooser so the caller's `park_waiting_for`
        // doesn't clobber the already-surfaced prompt.
        | WaitingFor::EffectZoneChoice { player, .. }
        // CR 707.9 + CR 614.12a: enter-as-copy and other mid-entry choices
        // surface their own `WaitingFor` variant with the correct chooser.
        | WaitingFor::CopyTargetChoice { player, .. }
        | WaitingFor::ChooseOneOfBranch { player, .. }
        | WaitingFor::NamedChoice { player, .. }
        | WaitingFor::ReturnAsAuraTarget { player, .. } => ZoneDeliveryResult::NeedsChoice(*player),
        _ => ZoneDeliveryResult::NeedsChoice(state.active_player),
    }
}

/// Execute a single object zone-change through the full pipeline:
/// ProposedEvent → replacement → move → ExileLink → shuffle → layers_dirty.
///
/// Shared by both `resolve()` (targeted) and `resolve_all()` (mass) to ensure
/// identical behavior for replacement effects, exile tracking, and auto-shuffle.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_zone_move(
    state: &mut GameState,
    obj_id: ObjectId,
    from_zone: Zone,
    dest_zone: Zone,
    source_id: ObjectId,
    duration: Option<&Duration>,
    enter_transformed: bool,
    enter_tapped: EtbTapState,
    enters_attacking: bool,
    controller_override: Option<PlayerId>,
    effect_enter_with_counters: &[(CounterType, u32)],
    face_down_profile: Option<&crate::types::ability::FaceDownProfile>,
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    execute_zone_move_with_terminal(
        state,
        obj_id,
        from_zone,
        dest_zone,
        source_id,
        duration,
        enter_transformed,
        enter_tapped,
        enters_attacking,
        controller_override,
        effect_enter_with_counters,
        face_down_profile,
        track_exiled_by_source,
        library_placement,
        enter_attached_to,
        events,
    )
    .into_zone_move_result()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_zone_move_with_controller(
    state: &mut GameState,
    obj_id: ObjectId,
    from_zone: Zone,
    dest_zone: Zone,
    source_id: ObjectId,
    duration: Option<&Duration>,
    enter_transformed: bool,
    enter_tapped: EtbTapState,
    enters_attacking: bool,
    controller_override: Option<PlayerId>,
    effect_enter_with_counters: &[(CounterType, u32)],
    face_down_profile: Option<&crate::types::ability::FaceDownProfile>,
    face_down_in_exile: ExileConcealment,
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
    exile_controller: Option<PlayerId>,
    performed_by: Option<PlayerId>,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    execute_zone_move_with_terminal_and_controller(
        state,
        obj_id,
        from_zone,
        dest_zone,
        source_id,
        duration,
        enter_transformed,
        enter_tapped,
        enters_attacking,
        controller_override,
        effect_enter_with_counters,
        face_down_profile,
        face_down_in_exile,
        track_exiled_by_source,
        library_placement,
        enter_attached_to,
        exile_controller,
        performed_by,
        events,
    )
    .into_zone_move_result()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_zone_move_with_terminal(
    state: &mut GameState,
    obj_id: ObjectId,
    from_zone: Zone,
    dest_zone: Zone,
    source_id: ObjectId,
    duration: Option<&Duration>,
    enter_transformed: bool,
    enter_tapped: EtbTapState,
    enters_attacking: bool,
    controller_override: Option<PlayerId>,
    effect_enter_with_counters: &[(CounterType, u32)],
    face_down_profile: Option<&crate::types::ability::FaceDownProfile>,
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveTerminalResult {
    execute_zone_move_with_terminal_and_controller(
        state,
        obj_id,
        from_zone,
        dest_zone,
        source_id,
        duration,
        enter_transformed,
        enter_tapped,
        enters_attacking,
        controller_override,
        effect_enter_with_counters,
        face_down_profile,
        ExileConcealment::Public,
        track_exiled_by_source,
        library_placement,
        enter_attached_to,
        None,
        None,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_zone_move_with_terminal_and_controller(
    state: &mut GameState,
    obj_id: ObjectId,
    from_zone: Zone,
    dest_zone: Zone,
    source_id: ObjectId,
    duration: Option<&Duration>,
    enter_transformed: bool,
    enter_tapped: EtbTapState,
    enters_attacking: bool,
    controller_override: Option<PlayerId>,
    effect_enter_with_counters: &[(CounterType, u32)],
    face_down_profile: Option<&crate::types::ability::FaceDownProfile>,
    face_down_in_exile: ExileConcealment,
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
    exile_controller: Option<PlayerId>,
    performed_by: Option<PlayerId>,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveTerminalResult {
    execute_zone_move_with_applied_terminal(
        state,
        obj_id,
        from_zone,
        dest_zone,
        source_id,
        duration,
        enter_transformed,
        enter_tapped,
        enters_attacking,
        controller_override,
        effect_enter_with_counters,
        face_down_profile,
        face_down_in_exile,
        crate::types::zones::ChainReferentIntent::Silent,
        track_exiled_by_source,
        library_placement,
        enter_attached_to,
        exile_controller,
        performed_by,
        HashSet::new(),
        None,
        events,
    )
}

/// CR 306.5b + CR 310.4b + CR 614.1c: Seed the intrinsic "enters with N
/// counters" replacement onto a proposed non-cast battlefield `ZoneChange`
/// when a planeswalker or battle enters the battlefield (effect-driven entry
/// — bounce-return, reanimate, blink, etc. — and the sandbox entry). The
/// counters ride the proposal, so `replacement::replace_event` sees them (an
/// enters-as-copy replacement retargets them) and the delivery tail's
/// `apply_etb_counters` applies Doubling Season class replacements
/// (CR 614.1a) and any CR 616.1 order choice to them.
///
/// Called before `replacement::replace_event` by the effect-driven pipeline
/// (`execute_zone_move_with_applied_terminal`) and the sandbox entry
/// (`engine_debug::enter_battlefield_with_etb`). Spell resolution
/// (`stack::resolve_top`) and land play (`engine::commit_land_play`) seed
/// their own proposals; the former binds the resolving spell's X (CR 107.3m).
///
/// CR 708.2a + CR 708.3: the INTRINSIC seeding is skipped for a face-down
/// entry — the object enters as the profile's body (a 2/2 creature) with no
/// loyalty/defense characteristic, so a manifested planeswalker card must not
/// enter with loyalty counters (issue #7822). Only the intrinsic half is
/// gated: an effect explicitly instructing entry counters on a face-down
/// entrant is a separate instruction whose counters are markers on the
/// resulting object (CR 122.1) and must survive; callers add those after this
/// seeding. The gate reads the proposal, so the caller must stamp
/// `face_down_profile` (and `enter_transformed`) on it first.
///
/// CR 712.14a + CR 712.18: A permanent entering transformed (e.g. a
/// double-faced card exiled and returned with its back face up, like a
/// creature-front // planeswalker-back DFC) will have its back face's
/// characteristics on the battlefield. The physical face swap happens later in
/// `deliver_replaced_zone_change`, so the entrant still shows its front face
/// here — read the back face's printed loyalty/defense directly so
/// CR 306.5b/310.4b seeds the counter map (the source of truth per CR 306.5c).
/// Without this a transforming planeswalker enters with 0 loyalty counters and
/// dies immediately to CR 704.5i. Ravenous (front-face cast-time) does not
/// apply to an effect-driven transformed entry, so only face counters are
/// seeded. CR 714.3a: a back-face Saga's lore counter comes from its own
/// replacement via the CR 614.12 projection
/// (`replacement::stage_transformed_entry_projection`), not from this seeding.
pub(crate) fn seed_intrinsic_entry_counters(state: &GameState, proposed: &mut ProposedEvent) {
    let ProposedEvent::ZoneChange {
        object_id,
        to: Zone::Battlefield,
        enter_transformed,
        face_down_profile: None,
        enter_with_counters,
        ..
    } = proposed
    else {
        return;
    };
    let Some(obj) = state
        .liminal_entries
        .get(object_id)
        .map(|entry| entry.object.projected())
        .or_else(|| state.objects.get(object_id))
    else {
        return;
    };
    let intrinsic = match (*enter_transformed, obj.back_face.as_ref()) {
        (true, Some(back)) => crate::game::printed_cards::intrinsic_face_entry_counters(
            back.printed_loyalty,
            back.loyalty,
            None,
            back.defense,
        ),
        _ => crate::game::printed_cards::intrinsic_etb_counters(obj, None),
    };
    enter_with_counters.extend(intrinsic);
}

/// CR 712.14a + CR 400.7 + CR 701.24c + CR 614.1 + CR 306.5b: build the exact
/// pre-consult proposal for one effect-driven zone move. Shared by the legacy
/// mover and the staged batch member so both apply the same guards, entry
/// modifications, intrinsic loyalty/defense seeding and effect counters.
///
/// Returns `None` when a pre-consult guard proves the move cannot happen: a
/// single-faced object cannot enter transformed, and a library card put into
/// its own library at no stated position does not change zones.
#[allow(clippy::too_many_arguments)]
fn prepare_zone_move_proposal(
    state: &GameState,
    obj_id: ObjectId,
    from_zone: Zone,
    dest_zone: Zone,
    source_id: ObjectId,
    enter_transformed: bool,
    enter_tapped: EtbTapState,
    enters_attacking: bool,
    controller_override: Option<PlayerId>,
    effect_enter_with_counters: &[(CounterType, u32)],
    face_down_profile: Option<&crate::types::ability::FaceDownProfile>,
    face_down_in_exile: ExileConcealment,
    chain_referent: crate::types::zones::ChainReferentIntent,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
    performed_by: Option<PlayerId>,
    replacement_applied: HashSet<AppliedReplacementKey>,
) -> Option<ProposedEvent> {
    // CR 712.14a: A single-faced object instructed to enter transformed
    // cannot enter the battlefield. A single-faced copy of a transforming
    // Saga therefore remains in exile after its final chapter resolves.
    if dest_zone == Zone::Battlefield
        && enter_transformed
        && state
            .objects
            .get(&obj_id)
            .is_some_and(|obj| obj.back_face.is_none())
    {
        return None;
    }
    // CR 400.7 + CR 701.24c: a library card put into its own library at no stated
    // position does not change zones; a shuffle the instruction names is its own step.
    if from_zone == Zone::Library && dest_zone == Zone::Library && library_placement.is_none() {
        return None;
    }
    let mut proposed = ProposedEvent::zone_change(obj_id, from_zone, dest_zone, Some(source_id));
    if let ProposedEvent::ZoneChange {
        applied,
        library_placement: placement,
        face_down_in_exile: ref mut conceal_in_exile,
        chain_referent: ref mut intent,
        performed_by: ref mut performer,
        ..
    } = &mut proposed
    {
        *placement = library_placement;
        *applied = replacement_applied;
        *conceal_in_exile = face_down_in_exile;
        *intent = chain_referent;
        *performer = performed_by;
    }

    // CR 712.14a: Set enter_transformed on the proposed event so replacement
    // effects preserve it through the pipeline.
    if enter_transformed {
        if let ProposedEvent::ZoneChange {
            enter_transformed: ref mut et,
            ..
        } = proposed
        {
            *et = true;
        }
    }

    // CR 614.1: Seed the three-state ETB tap-state directly onto the proposed
    // event so the replacement pipeline preserves it.
    if !enter_tapped.is_unspecified() {
        if let ProposedEvent::ZoneChange {
            enter_tapped: ref mut et,
            ..
        } = proposed
        {
            *et = enter_tapped;
        }
    }

    if enters_attacking {
        if let ProposedEvent::ZoneChange {
            enters_attacking: ref mut entering_attacking,
            ..
        } = proposed
        {
            *entering_attacking = true;
        }
    }

    // CR 110.2a: Set controller_override on the proposed event so replacement
    // effects see the correct controller through the pipeline.
    if let Some(ctrl) = controller_override {
        if let ProposedEvent::ZoneChange {
            controller_override: ref mut co,
            ..
        } = proposed
        {
            *co = Some(ctrl);
        }
    }

    // CR 708.2a + CR 708.3: Carry the face-down profile on the proposed event.
    if let Some(profile) = face_down_profile {
        if let ProposedEvent::ZoneChange {
            face_down_profile: ref mut fdp,
            ..
        } = proposed
        {
            *fdp = Some(Box::new(profile.clone()));
        }
    }

    if let Some(attach_to) = enter_attached_to {
        if let ProposedEvent::ZoneChange {
            attach_to: ref mut at,
            ..
        } = proposed
        {
            *at = Some(attach_to);
        }
    }

    // CR 306.5b + CR 310.4b + CR 614.1c: seed the intrinsic loyalty/defense
    // counters onto the proposal; the transformed and face-down stamps above
    // are read by the helper's gates.
    seed_intrinsic_entry_counters(state, &mut proposed);
    // CR 122.1 + CR 614.1c: effect-driven enter-with-counters apply to every
    // entry; non-battlefield destinations carry them to `apply_etb_counters`.
    if !effect_enter_with_counters.is_empty() {
        if let ProposedEvent::ZoneChange {
            enter_with_counters,
            ..
        } = &mut proposed
        {
            enter_with_counters.extend(effect_enter_with_counters.iter().cloned());
        }
    }

    Some(proposed)
}

#[allow(clippy::too_many_arguments)]
fn execute_zone_move_with_applied_terminal(
    state: &mut GameState,
    obj_id: ObjectId,
    from_zone: Zone,
    dest_zone: Zone,
    source_id: ObjectId,
    duration: Option<&Duration>,
    enter_transformed: bool,
    enter_tapped: EtbTapState,
    enters_attacking: bool,
    controller_override: Option<PlayerId>,
    effect_enter_with_counters: &[(CounterType, u32)],
    face_down_profile: Option<&crate::types::ability::FaceDownProfile>,
    face_down_in_exile: ExileConcealment,
    chain_referent: crate::types::zones::ChainReferentIntent,
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
    exile_controller: Option<PlayerId>,
    performed_by: Option<PlayerId>,
    replacement_applied: HashSet<AppliedReplacementKey>,
    owner: Option<BatchMemberDeliveryOwner>,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveTerminalResult {
    let Some(member) = state
        .objects
        .get(&obj_id)
        .map(ObjectIncarnationRef::from_object)
    else {
        return ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained);
    };
    let Some(proposed) = prepare_zone_move_proposal(
        state,
        obj_id,
        from_zone,
        dest_zone,
        source_id,
        enter_transformed,
        enter_tapped,
        enters_attacking,
        controller_override,
        effect_enter_with_counters,
        face_down_profile,
        face_down_in_exile,
        chain_referent,
        library_placement.clone(),
        enter_attached_to,
        performed_by,
        replacement_applied,
    ) else {
        return ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained);
    };

    // KNOWN GAP (CR 614.12, documented deferral): for a FACE-DOWN battlefield
    // entry (the proposal carries `face_down_profile`), this consult runs the
    // replacement matchers against the object's PRINTED characteristics, but
    // CR 614.12 requires checking "the characteristics of the permanent as it
    // would exist on the battlefield" — for a morph/manifest entry that is the
    // face-down 2/2 with no name, types, or subtypes (CR 708.2a). A type- or
    // name-keyed entry replacement (e.g. a Wizard-scoped "Wizards you control
    // enter with a +1/+1 counter") therefore wrongly matches a face-down
    // printed Wizard, and a name/type-scoped redirect wrongly applies to an
    // entry that should look like a blank 2/2. Narrow class today (the common
    // enter-tapped/counter statics are type-agnostic or creature-scoped, which
    // the face-down 2/2 still satisfies); fixing it requires the matcher pass
    // to evaluate filters against the profile-projected characteristics when
    // `face_down_profile` is present.
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            // R4c (b): the Aura-host post-replace consult and the delivery tail
            // are one shared seam (`deliver_replaced_zone_change_with_aura_host`)
            // used by both this legacy mover and the staged ChangeZone batch, so
            // the prompt, the unhosted disposition and the sole-host attach
            // cannot drift between the two producers.
            match deliver_replaced_zone_change_with_aura_host(
                state,
                obj_id,
                member,
                event,
                Some(source_id),
                duration,
                exile_controller,
                track_exiled_by_source,
                library_placement.clone(),
                None,
                owner,
                events,
            ) {
                AuraAwareZoneDelivery::Completed(completion) => {
                    ZoneMoveTerminalResult::Completed(completion)
                }
                AuraAwareZoneDelivery::NeedsChoice(player) => {
                    ZoneMoveTerminalResult::NeedsChoice(player)
                }
                AuraAwareZoneDelivery::NeedsAuraHostChoice => {
                    ZoneMoveTerminalResult::NeedsAuraAttachmentChoice
                }
            }
        }
        ReplacementResult::Prevented => {
            ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Prevented)
        }
        ReplacementResult::NeedsChoice(player) => {
            // CR 616.1: `replace_event` sets only `pending_replacement` — the
            // wait-state was historically each caller's to set, and callers that
            // forgot stranded the object as a zone ghost (move parked in
            // `pending_replacement`, prompt never surfaced because the engine
            // gates `ChooseReplacement` on the wait state). Park HERE, at the
            // single unparked origin, so every single-move caller (counter,
            // bounce, seek, and all future migrations) is safe by construction.
            //
            // Idempotence: callers that still set the wait state themselves
            // (change_zone's `park_waiting_for` arms, end_phase /
            // exile_from_top_until's `replacement_choice_waiting_for`) recompute
            // the identical value from the same `pending_replacement`.
            // `park_waiting_for` also keeps the CR 614.12a devour guard: it
            // never clobbers an already-surfaced `EffectZoneChoice`. The
            // delivery-tail NeedsChoice path above is NOT parked here — its
            // wait state is already set by the counter-pause / devour machinery
            // (`replacement_pause_delivery_result` reads it).
            if let Some(pending) = state.pending_replacement.as_mut() {
                // Legacy wire mirror only; the proposal owns the disposition
                // through every pause and terminal delivery.
                pending.library_placement = library_placement;
                pending.exile_controller = exile_controller;
                pending.exile_duration = duration.cloned();
                pending.exile_tracking = if track_exiled_by_source {
                    ZoneDeliveryExileTracking::TrackBySource
                } else {
                    ZoneDeliveryExileTracking::None
                };
            }
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            ZoneMoveTerminalResult::NeedsChoice(player)
        }
    }
}

#[cfg(test)]
mod announced_spell_residency_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility};
    use crate::types::game_state::{StackEntry, StackEntryKind};
    use crate::types::identifiers::CardId;

    #[test]
    fn casting_to_stack_rejects_same_id_activated_ability_entry() {
        let mut state = GameState::new_two_player(42);
        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Activated Source".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&object_id).unwrap().is_token = true;
        state.stack.push_back(StackEntry {
            id: object_id,
            source_id: object_id,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: object_id,
                ability: Box::new(ResolvedAbility::new(
                    Effect::NoOp,
                    vec![],
                    object_id,
                    PlayerId(0),
                )),
            },
        });
        assert_eq!(state.objects[&object_id].zone, Zone::Exile);
        assert!(state.stack.iter().any(|entry| {
            entry.id == object_id && matches!(entry.kind, StackEntryKind::ActivatedAbility { .. })
        }));

        // CR 109.1 / CR 602.2a: A same-id activated ability is a distinct
        // noncard stack object, so it cannot satisfy the spell-residency gate.
        let mut events = Vec::new();
        let result = move_object_with_terminal(
            &mut state,
            ZoneMoveRequest::casting_to_stack(object_id, object_id, PlayerId(0)),
            &mut events,
        );

        assert!(matches!(
            result,
            ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained)
        ));
        assert_eq!(state.objects[&object_id].zone, Zone::Exile);
        assert!(events.is_empty());
    }
}

#[cfg(test)]
mod w3_library_placement_tests {
    use super::*;
    use crate::game::effects::resolve_ability_chain;
    use crate::game::engine::{apply, apply_as_current};
    use crate::game::scenario::{GameScenario, P0};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, QuantityExpr, ReplacementDefinition, ReplacementMode,
        SearchSelectionConstraint, TargetFilter,
    };
    use crate::types::ability::{ReplacementCondition, TypeFilter, TypedFilter};
    use crate::types::actions::{AlternativeCastDecision, GameAction};
    use crate::types::game_state::{CastingVariant, PersistedGameState};
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::phase::Phase;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::resolution::ResolutionFrame;
    use crate::types::resolution::ResolutionStateWire;

    fn event_placement(event: &ProposedEvent) -> Option<LibraryPosition> {
        let ProposedEvent::ZoneChange {
            library_placement, ..
        } = event
        else {
            panic!("expected a zone-change proposal");
        };
        library_placement.clone()
    }

    fn assert_finished(state: &GameState) {
        assert!(state.pending_replacement.is_none());
        assert!(state.active_batch_delivery().is_none());
        assert!(state.stack.is_empty());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    fn assert_no_shuffle(events: &[GameEvent]) {
        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                action: crate::types::events::PlayerActionKind::ShuffledLibrary,
                ..
            }
        )));
    }

    fn library_fixture(count: usize) -> (GameState, ObjectId, Vec<ObjectId>) {
        let mut state = GameState::new_two_player(42);
        let sentinels: Vec<_> = (0..count)
            .map(|n| {
                create_object(
                    &mut state,
                    CardId(n as u64 + 1),
                    PlayerId(0),
                    format!("Sentinel {n}"),
                    Zone::Library,
                )
            })
            .collect();
        state.players[0].library = sentinels.iter().copied().collect();
        let card = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Placed".into(),
            Zone::Graveyard,
        );
        (state, card, sentinels)
    }

    fn install_optional_library_redirect(state: &mut GameState) -> ObjectId {
        let source = create_object(
            state,
            CardId(101),
            PlayerId(0),
            "Synthetic positional consult".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(optional_zone_redirect(Zone::Library, Zone::Exile));
        source
    }

    fn assert_parked_position(
        state: &GameState,
        card: ObjectId,
        position: Option<LibraryPosition>,
    ) {
        let pending = state
            .pending_replacement
            .as_ref()
            .expect("required replacement pause");
        assert!(
            matches!(&pending.proposed, ProposedEvent::ZoneChange { object_id, to: Zone::Library, .. } if *object_id == card)
        );
        assert_eq!(event_placement(&pending.proposed), position);
        assert!(!pending.candidates.is_empty());
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice {
                player: PlayerId(0),
                ..
            }
        ));
    }

    /// Verify the active component child exposes its exact original parent,
    /// then prove the same relation survives a full save/restore.
    fn check_and_restore_component_parent(state: &mut GameState, survivor: ObjectIncarnationRef) {
        let relation = state
            .active_batch_delivery()
            .and_then(|batch| batch.component_delivery_parent.clone())
            .expect("component child retains its original parent relation");
        assert_eq!(relation.member, survivor);
        assert!(matches!(
            &relation.expected_event,
            ProposedEvent::ZoneChange { object_id, .. } if *object_id == survivor.object_id
        ));
        assert!(matches!(
            &relation.approved_event,
            ProposedEvent::ZoneChange { object_id, .. } if *object_id == survivor.object_id
        ));
        let parent = state
            .resolution_stack
            .batch_delivery_owner_mut(relation.parent_group_id)
            .expect("child exposes its identified original parent");
        assert!(parent.paused_current.as_ref().is_some_and(|paused| paused
            .captures(relation.member, &relation.expected_event)
            && paused.terminal_completion.is_none()));
        let wire = serde_json::to_value(&*state).expect("component pause serializes");
        let mut restored: GameState =
            serde_json::from_value(wire).expect("component pause restores");
        let restored_relation = restored
            .active_batch_delivery()
            .and_then(|batch| batch.component_delivery_parent.clone())
            .expect("restored component child retains its relation");
        assert_eq!(restored_relation, relation);
        restored
            .resolution_stack
            .batch_delivery_owner_mut(relation.parent_group_id)
            .expect("restored child exposes its identified original parent");
        *state = restored;
    }

    #[test]
    fn canonical_placement_is_seeded_by_both_producers_before_consultation() {
        for position in [
            LibraryPosition::Top,
            LibraryPosition::Bottom,
            LibraryPosition::NthFromTop { n: 2 },
        ] {
            let (mut state, card, sentinels) = library_fixture(3);
            let source = install_optional_library_redirect(&mut state);
            let mut other = state.clone();
            assert!(matches!(
                move_object(
                    &mut state,
                    ZoneMoveRequest::effect(card, Zone::Library, source)
                        .at_library_position(position.clone()),
                    &mut Vec::new()
                ),
                ZoneMoveResult::NeedsChoice(PlayerId(0))
            ));
            assert!(matches!(
                execute_zone_move_with_terminal_and_controller(
                    &mut other,
                    card,
                    Zone::Graveyard,
                    Zone::Library,
                    source,
                    None,
                    false,
                    EtbTapState::Unspecified,
                    false,
                    None,
                    &[],
                    None,
                    ExileConcealment::Public,
                    false,
                    Some(position.clone()),
                    None,
                    None,
                    None,
                    &mut Vec::new(),
                ),
                ZoneMoveTerminalResult::NeedsChoice(PlayerId(0))
            ));
            for state in [&mut state, &mut other] {
                assert_parked_position(state, card, Some(position.clone()));
                assert_eq!(state.objects[&card].zone, Zone::Graveyard);
                assert_eq!(
                    state.players[0].library.iter().copied().collect::<Vec<_>>(),
                    sentinels
                );
                let result =
                    apply_as_current(state, GameAction::ChooseReplacement { index: 1 }).unwrap();
                let mut expected = sentinels.clone();
                expected.insert(
                    library_position_index(&position)
                        .unwrap_or(expected.len())
                        .min(expected.len()),
                    card,
                );
                assert_eq!(
                    state.players[0].library.iter().copied().collect::<Vec<_>>(),
                    expected
                );
                assert_no_shuffle(&result.events);
                assert_finished(state);
            }
        }
        let (mut state, card, _) = library_fixture(0);
        assert!(matches!(
            move_object_with_terminal(
                &mut state,
                ZoneMoveRequest::effect(ObjectId(u64::MAX), Zone::Library, card)
                    .at_library_position(LibraryPosition::Top),
                &mut Vec::new()
            ),
            ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained)
        ));
        assert!(matches!(
            move_object(
                &mut state,
                ZoneMoveRequest::effect(card, Zone::Library, card)
                    .at_library_position(LibraryPosition::Top),
                &mut Vec::new()
            ),
            ZoneMoveResult::Done
        ));
        assert_eq!(state.players[0].library.front(), Some(&card));
    }

    #[test]
    fn approved_event_is_the_only_terminal_placement_authority() {
        for count in [0, 1, 3] {
            for position in [
                Some(LibraryPosition::Top),
                Some(LibraryPosition::Bottom),
                Some(LibraryPosition::NthFromTop { n: 2 }),
                None,
            ] {
                let (mut state, card, sentinels) = library_fixture(count);
                let mut event =
                    ProposedEvent::zone_change(card, Zone::Graveyard, Zone::Library, Some(card));
                if let ProposedEvent::ZoneChange {
                    library_placement, ..
                } = &mut event
                {
                    *library_placement = position.clone();
                }
                assert_eq!(event_placement(&event), position);
                let mut events = Vec::new();
                let approved = ApprovedZoneChange::approve_post_replacement(event).unwrap();
                assert!(matches!(
                    deliver(
                        &mut state,
                        approved,
                        DeliveryCtx {
                            source_id: Some(card),
                            exile_links: ExileLinkSpec::default(),
                            drain: PostReplacementDrainOwner::DeliveryTail,
                            library_placement: Some(LibraryPosition::Top),
                        },
                        &mut events
                    ),
                    ZoneDeliveryResult::Done
                ));
                assert_eq!(state.objects[&card].zone, Zone::Library);
                assert_eq!(events.iter().filter(|e| matches!(e, GameEvent::ZoneChanged { object_id, .. } if *object_id == card)).count(), 1);
                if let Some(position) = position {
                    let mut expected = sentinels;
                    expected.insert(
                        library_position_index(&position)
                            .unwrap_or(expected.len())
                            .min(expected.len()),
                        card,
                    );
                    assert_eq!(
                        state.players[0].library.iter().copied().collect::<Vec<_>>(),
                        expected
                    );
                    assert_no_shuffle(&events);
                } else {
                    // Current null selects the existing placement-less delivery
                    // convention even when the legacy caller still says Top.
                    assert!(events.iter().any(|e| matches!(
                        e,
                        GameEvent::PlayerPerformedAction {
                            action: crate::types::events::PlayerActionKind::ShuffledLibrary,
                            ..
                        }
                    )));
                }
            }
        }
        let (mut state, card, sentinels) = library_fixture(3);
        let event = ProposedEvent::zone_change(card, Zone::Graveyard, Zone::Exile, Some(card));
        assert_eq!(event_placement(&event), None);
        let mut events = Vec::new();
        deliver_replaced_zone_change(
            &mut state,
            event,
            Some(card),
            None,
            None,
            false,
            PostReplacementDrainOwner::DeliveryTail,
            Some(LibraryPosition::Top),
            &mut events,
        );
        assert_eq!(state.objects[&card].zone, Zone::Exile);
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            sentinels
        );
        assert_no_shuffle(&events);
    }

    #[test]
    fn paused_producer_restoration_distinguishes_absent_null_and_current_position() {
        let (mut state, card, sentinels) = library_fixture(3);
        let source = install_optional_library_redirect(&mut state);
        move_object(
            &mut state,
            ZoneMoveRequest::effect(card, Zone::Library, source)
                .at_library_position(LibraryPosition::Top),
            &mut Vec::new(),
        );
        assert_parked_position(&state, card, Some(LibraryPosition::Top));
        for current in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::to_value(LibraryPosition::Bottom).unwrap()),
        ] {
            let expected = match &current {
                None => Some(LibraryPosition::Top),
                Some(serde_json::Value::Null) => None,
                Some(_) => Some(LibraryPosition::Bottom),
            };
            let mut wire = serde_json::to_value(&state).unwrap();
            let proposed = wire["pending_replacement"]["proposed"]["ZoneChange"]
                .as_object_mut()
                .expect("paused proposal wire");
            match current {
                None => {
                    proposed.remove("library_placement");
                }
                Some(value) => {
                    proposed.insert("library_placement".into(), value);
                }
            }
            let mut restored: GameState = serde_json::from_value(wire).unwrap();
            assert_parked_position(&restored, card, expected.clone());
            let saved = serde_json::to_value(&restored).unwrap();
            assert!(saved["pending_replacement"]["proposed"]["ZoneChange"]
                .get("library_placement")
                .is_some());
            let twice: GameState = serde_json::from_value(saved.clone()).unwrap();
            assert_eq!(serde_json::to_value(twice).unwrap(), saved);
            let result =
                apply_as_current(&mut restored, GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
            assert_eq!(restored.objects[&card].zone, Zone::Library);
            if let Some(position) = expected {
                let mut order = sentinels.clone();
                order.insert(
                    library_position_index(&position).unwrap_or(order.len()),
                    card,
                );
                assert_eq!(
                    restored.players[0]
                        .library
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    order
                );
                assert_no_shuffle(&result.events);
            } else {
                assert!(result.events.iter().any(|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        action: crate::types::events::PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                )));
            }
            assert_finished(&restored);
        }
    }

    /// Infrastructure choices only: two distinct optional redirects are present
    /// before consultation; no board mutation occurs between answers.
    #[test]
    fn canonical_placement_survives_public_repeated_choices() {
        let (mut state, card, sentinels) = library_fixture(3);
        let first = install_optional_library_redirect(&mut state);
        let second = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Other synthetic redirect".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&second)
            .unwrap()
            .replacement_definitions
            .push(optional_zone_redirect(Zone::Library, Zone::Hand));
        move_object(
            &mut state,
            ZoneMoveRequest::effect(card, Zone::Library, first)
                .at_library_position(LibraryPosition::Bottom),
            &mut Vec::new(),
        );
        assert_parked_position(&state, card, Some(LibraryPosition::Bottom));
        let pending = state.pending_replacement.as_ref().unwrap();
        assert_eq!(pending.candidates.len(), 2);
        assert!(!pending.is_optional);
        let index = pending
            .candidates
            .iter()
            .position(|rid| rid.source == first)
            .unwrap();
        let first_id = pending.candidates[index];
        apply_as_current(&mut state, GameAction::ChooseReplacement { index }).unwrap();
        assert_eq!(
            state.pending_replacement.as_ref().unwrap().candidates,
            vec![first_id]
        );
        assert!(state.pending_replacement.as_ref().unwrap().is_optional);
        assert_parked_position(&state, card, Some(LibraryPosition::Bottom));
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 }).unwrap();
        assert_parked_position(&state, card, Some(LibraryPosition::Bottom));
        let pending = state.pending_replacement.as_ref().unwrap();
        assert_eq!(pending.candidates.len(), 1);
        assert_eq!(pending.candidates[0].source, second);
        assert!(pending
            .proposed
            .applied_set()
            .contains(&AppliedReplacementKey::Object {
                source: first,
                index: first_id.index
            }));
        assert_eq!(state.objects[&card].zone, Zone::Graveyard);
        let result =
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 }).unwrap();
        let mut expected = sentinels;
        expected.push(card);
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            result
                .events
                .iter()
                .filter(
                    |e| matches!(e, GameEvent::ZoneChanged { object_id, .. } if *object_id == card)
                )
                .count(),
            1
        );
        assert_no_shuffle(&result.events);
        assert_finished(&state);
    }

    fn totally_lost_mutate_fixture() -> (
        crate::game::scenario::GameRunner,
        ObjectId,
        ObjectId,
        ObjectId,
        Vec<ObjectId>,
    ) {
        // Verbatim AtomicCards Oracle, including the complete mutate reminder.
        const OCTOPUS: &str = "Mutate {1}{U} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nFlash\nWhenever this creature deals combat damage to a player, draw a card.";
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bears = scenario
            .add_creature(P0, "Grizzly Bears", 2, 2)
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let octopus = scenario
            .add_creature_to_hand(P0, "Sea-Dasher Octopus", 2, 2)
            .with_subtypes(vec!["Octopus"])
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            })
            .from_oracle_text_with_keywords(&["Mutate", "Flash"], OCTOPUS)
            .id();
        let lost = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Totally Lost",
                true,
                "Put target nonland permanent on top of its owner's library.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Blue],
            })
            .id();
        scenario.with_library_top(P0, &["Forest", "Island", "Mountain"]);
        scenario.with_mana_pool(
            P0,
            (0..7)
                .map(|_| ManaUnit::new(ManaType::Blue, bears, false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        let sentinels = runner.state().players[0].library.iter().copied().collect();
        let outcome = runner
            .cast(octopus)
            .alternative_cast(AlternativeCastDecision::Alternative)
            .casting_variant(CastingVariant::Mutate)
            .target_object(bears)
            .resolve();
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::MutateMergeChoice { .. }
        ));
        runner
            .act(GameAction::ChooseMutateMergeSide {
                side: crate::game::merge::MergeSide::Bottom,
            })
            .unwrap();
        // CR 702.140c + CR 730.2c: the legal cast merged into the original host.
        assert_eq!(
            runner.state().objects[&bears].merged_components,
            vec![bears, octopus]
        );
        assert!(!runner.state().battlefield.contains(&octopus));
        assert_finished(runner.state());
        (runner, bears, octopus, lost, sentinels)
    }

    fn assert_split_provenance(
        state: &GameState,
        events: &[GameEvent],
        survivor: ObjectIncarnationRef,
        component: ObjectIncarnationRef,
        record_start: usize,
        component_destination: Zone,
    ) {
        // CR 730.3: only one permanent leaves; the absorbed card has no separate
        // battlefield departure, in both the emitted events and stored ledger.
        for (id, origin, destination) in [
            (survivor.object_id, Some(Zone::Battlefield), Zone::Library),
            (component.object_id, None, component_destination),
        ] {
            let movements: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::ZoneChanged {
                        object_id,
                        from,
                        to,
                        ..
                    } if *object_id == id => Some((*from, *to)),
                    _ => None,
                })
                .collect();
            assert_eq!(movements, vec![(origin, destination)]);
            let records: Vec<_> = state
                .zone_changes_this_turn
                .iter()
                .skip(record_start)
                .filter(|record| record.object_id == id)
                .collect();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].from_zone, origin);
            assert_eq!(records[0].to_zone, destination);
        }
        // CR 400.7 + CR 730.3c: one new incarnation per component and a link to
        // the survivor's resulting object, including when destinations diverge.
        assert_eq!(
            state.objects[&survivor.object_id].incarnation,
            survivor.incarnation + 1
        );
        assert_eq!(
            state.objects[&component.object_id].incarnation,
            component.incarnation + 1
        );
        assert_eq!(
            state.objects[&component.object_id].split_from_merge_survivor,
            Some(ObjectIncarnationRef::from_object(
                &state.objects[&survivor.object_id]
            ))
        );
        assert_no_shuffle(events);
        assert_finished(state);
    }

    fn assert_component_top_set(
        state: &GameState,
        components: &[ObjectId],
        sentinels: &[ObjectId],
    ) {
        // CR 730.3a: this helper checks component membership only; callers
        // separately assert that the owner can submit a private arrangement.
        let library: Vec<_> = state.players[0].library.iter().copied().collect();
        assert_eq!(&library[components.len()..], sentinels);
        assert_eq!(
            library[..components.len()]
                .iter()
                .copied()
                .collect::<HashSet<_>>(),
            components.iter().copied().collect::<HashSet<_>>()
        );
    }

    #[test]
    fn totally_lost_places_both_mutate_components_without_extra_departure_or_shuffle() {
        let (mut runner, bears, octopus, lost, sentinels) = totally_lost_mutate_fixture();
        let survivor = ObjectIncarnationRef::from_object(&runner.state().objects[&bears]);
        let component = ObjectIncarnationRef::from_object(&runner.state().objects[&octopus]);
        let start = runner.state().zone_changes_this_turn.len();
        let outcome = runner.cast(lost).target_object(bears).resolve();
        let mut events = outcome.events().to_vec();
        let mut order = Vec::new();
        if let WaitingFor::DigRestSplitChoice { cards, .. } = &runner.state().waiting_for {
            order = cards.clone();
            order.reverse();
            let result = runner
                .act(crate::types::actions::GameAction::SelectCards {
                    cards: order.clone(),
                })
                .expect("the owner submits a nonidentity component order");
            events.extend(result.events);
        }
        assert!(
            !order.is_empty(),
            "merged components must offer the owner arrangement"
        );
        let state = runner.state();
        let library: Vec<_> = state.players[0].library.iter().copied().collect();
        assert_eq!(
            &library[..order.len()],
            order.as_slice(),
            "the submitted owner order is the final top order"
        );
        assert_component_top_set(state, &[bears, octopus], &sentinels);
        assert_split_provenance(state, &events, survivor, component, start, Zone::Library);
        assert_eq!(
            crate::game::merge::co_split_components(state, bears, &[bears]),
            vec![octopus]
        );
    }

    #[test]
    fn positioned_mutate_component_pause_preserves_applied_identity_and_split_provenance() {
        let (mut runner, bears, octopus, lost, sentinels) = totally_lost_mutate_fixture();
        let inherited_source = install_optional_library_redirect(runner.state_mut());
        let component_source = create_object(
            runner.state_mut(),
            CardId(103),
            PlayerId(0),
            "Synthetic Octopus consult".into(),
            Zone::Battlefield,
        );
        runner
            .state_mut()
            .objects
            .get_mut(&component_source)
            .unwrap()
            .replacement_definitions
            .push(
                optional_zone_redirect(Zone::Library, Zone::Exile).valid_card(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature).subtype("Octopus".into()),
                )),
            );
        let survivor = ObjectIncarnationRef::from_object(&runner.state().objects[&bears]);
        let component = ObjectIncarnationRef::from_object(&runner.state().objects[&octopus]);
        let start = runner.state().zone_changes_this_turn.len();
        let outcome = runner.cast(lost).target_object(bears).resolve();
        assert_parked_position(outcome.state(), bears, Some(LibraryPosition::Top));
        let first_id = outcome
            .state()
            .pending_replacement
            .as_ref()
            .unwrap()
            .candidates[0];
        assert_eq!(first_id.source, inherited_source);
        let mut events = outcome.events().to_vec();
        *runner.state_mut() =
            serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap();
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index: 1 })
                .unwrap()
                .events,
        );
        assert_parked_position(runner.state(), octopus, Some(LibraryPosition::Top));
        let pending = runner.state().pending_replacement.as_ref().unwrap();
        assert_eq!(pending.candidates.len(), 1);
        assert_eq!(pending.candidates[0].source, component_source);
        assert!(pending
            .proposed
            .applied_set()
            .contains(&AppliedReplacementKey::Object {
                source: inherited_source,
                index: first_id.index
            }));
        assert_eq!(
            runner.state().objects[&octopus].incarnation,
            component.incarnation
        );
        check_and_restore_component_parent(runner.state_mut(), survivor);
        // The relation and its approved event are internal continuation
        // authority; neither public seat projection may expose or repair them.
        for viewer in [PlayerId(0), PlayerId(1)] {
            let projected =
                crate::game::visibility::filter_state_for_viewer(runner.state(), viewer);
            assert!(projected.active_batch_delivery().is_none());
            assert!(
                !serde_json::to_string(&projected)
                    .expect("projection serializes")
                    .contains("component_delivery_parent"),
                "the executable relation must not reach a public projection"
            );
        }
        assert!(runner
            .state()
            .active_batch_delivery()
            .and_then(|batch| batch.component_delivery_parent.as_ref())
            .is_some());
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index: 1 })
                .unwrap()
                .events,
        );
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::DigRestSplitChoice { .. }
            ),
            "merged components must offer the owner arrangement after replacement"
        );
        if let WaitingFor::DigRestSplitChoice { cards, .. } = &runner.state().waiting_for {
            let mut order = cards.clone();
            order.reverse();
            events.extend(
                runner
                    .act(GameAction::SelectCards { cards: order })
                    .expect("the owner submits a nonidentity component order")
                    .events,
            );
        }
        assert_component_top_set(runner.state(), &[bears, octopus], &sentinels);
        assert_split_provenance(
            runner.state(),
            &events,
            survivor,
            component,
            start,
            Zone::Library,
        );
    }

    #[test]
    fn positioned_commander_component_redirect_keeps_ordinary_sibling_position() {
        // Infrastructure sibling: commander designation is fixture metadata,
        // not a claim that Sea-Dasher Octopus is a legal printed commander.
        let (mut runner, bears, octopus, lost, sentinels) = totally_lost_mutate_fixture();
        // A built-in format fixes command-zone usage by CR; use Commander so the
        // fixture's real save/restore roundtrip does not deserialize an illegal
        // Standard-plus-command-zone configuration.
        runner.state_mut().format_config = crate::types::format::FormatConfig::commander();
        runner
            .state_mut()
            .objects
            .get_mut(&octopus)
            .unwrap()
            .is_commander = true;
        let survivor = ObjectIncarnationRef::from_object(&runner.state().objects[&bears]);
        let component = ObjectIncarnationRef::from_object(&runner.state().objects[&octopus]);
        let start = runner.state().zone_changes_this_turn.len();
        let outcome = runner.cast(lost).target_object(bears).resolve();
        assert_parked_position(outcome.state(), octopus, Some(LibraryPosition::Top));
        let mut events = outcome.events().to_vec();
        check_and_restore_component_parent(runner.state_mut(), survivor);
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .unwrap()
                .events,
        );
        assert_eq!(runner.state().objects[&octopus].zone, Zone::Command);
        assert_component_top_set(runner.state(), &[bears], &sentinels);
        assert_split_provenance(
            runner.state(),
            &events,
            survivor,
            component,
            start,
            Zone::Command,
        );
    }

    #[test]
    fn merged_component_carry_ignores_position_for_nonlibrary_destination_and_empty_merge() {
        let (mut runner, bears, octopus, _, _) = totally_lost_mutate_fixture();
        let redirect = create_object(
            runner.state_mut(),
            CardId(104),
            PlayerId(0),
            "Synthetic exile consult".into(),
            Zone::Battlefield,
        );
        runner
            .state_mut()
            .objects
            .get_mut(&redirect)
            .unwrap()
            .replacement_definitions
            .push(optional_zone_redirect(Zone::Exile, Zone::Hand));
        let applied = HashSet::from([AppliedReplacementKey::Object {
            source: ObjectId(999),
            index: 0,
        }]);
        assert!(matches!(
            crate::game::merge::move_merged_permanent_on_leave(
                runner.state_mut(),
                bears,
                Zone::Exile,
                Some(&LibraryPosition::Top),
                &applied,
                &mut Vec::new(),
            ),
            BatchMoveResult::NeedsChoice
        ));
        let pending = runner.state().pending_replacement.as_ref().unwrap();
        assert!(
            matches!(&pending.proposed, ProposedEvent::ZoneChange { object_id, to: Zone::Exile, .. } if [bears, octopus].contains(object_id))
        );
        assert_eq!(event_placement(&pending.proposed), None);
        assert_eq!(pending.proposed.applied_set(), &applied);

        let (mut empty, card, _) = library_fixture(1);
        let before = empty.clone();
        let mut events = Vec::new();
        for id in [card, ObjectId(u64::MAX)] {
            assert!(matches!(
                crate::game::merge::move_merged_permanent_on_leave(
                    &mut empty,
                    id,
                    Zone::Library,
                    Some(&LibraryPosition::Top),
                    &HashSet::new(),
                    &mut events,
                ),
                BatchMoveResult::Done
            ));
        }
        assert_eq!(empty, before);
        assert!(events.is_empty());
    }

    /// Install a board-wide `Moved` replacement: "any object that would be put
    /// into a library is exiled instead" (synthetic — no such card exists in the
    /// pool today, which is why a non-exempt library placement was a guaranteed
    /// no-op before W3). The redirect's destination is the match condition; the
    /// `.execute(ChangeZone { destination: Exile })` is the lowered effect.
    fn install_library_to_exile_redirect(state: &mut GameState) -> ObjectId {
        let source = create_object(
            state,
            CardId(90001),
            PlayerId(0),
            "Library Exile Redirect".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Exile,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
                    },
                ))
                .destination_zone(Zone::Library),
        );
        source
    }

    fn optional_zone_redirect(from: Zone, to: Zone) -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .mode(ReplacementMode::Optional { decline: None })
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: to,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ))
            .destination_zone(from)
    }

    fn face_down_self_search(source: ObjectId, controller: PlayerId) -> ResolvedAbility {
        let mut exile_step = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            source,
            controller,
        );
        exile_step.context.face_down_in_exile = crate::types::ability::ExileConcealment::FaceDown;
        ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
            vec![],
            source,
            controller,
        )
        .sub_ability(exile_step)
    }

    #[test]
    fn face_down_exile_single_replacement_enters_battlefield_face_up_and_public() {
        let mut state = GameState::new_two_player(42);
        let redirect_source = create_object(
            &mut state,
            CardId(90100),
            PlayerId(0),
            "Exile to Battlefield Redirect".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&redirect_source)
            .unwrap()
            .replacement_definitions
            .push(optional_zone_redirect(Zone::Exile, Zone::Battlefield));

        let card = create_object(
            &mut state,
            CardId(90101),
            PlayerId(0),
            "Printed Creature".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&card).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(4);
            obj.base_power = Some(4);
            obj.toughness = Some(5);
            obj.base_toughness = Some(5);
        }

        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(card, Zone::Exile, redirect_source).face_down_in_exile(),
            &mut events,
        );
        let ZoneMoveResult::NeedsChoice(chooser) = result else {
            panic!("expected the face-down exile redirect to park for a choice");
        };
        state.priority_player = chooser;
        let resumed = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept the Exile-to-Battlefield replacement");
        events.extend(resumed.events);

        let obj = state.objects.get(&card).unwrap();
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(
            !obj.face_down,
            "the replacement destination is public Battlefield"
        );
        assert_eq!(obj.name, "Printed Creature");
        assert_eq!(obj.power, Some(4));
        assert_eq!(obj.toughness, Some(5));
        assert!(obj.face_down_cause.is_none());

        let public_record = events.iter().find_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Graveyard),
                to: Zone::Battlefield,
                record,
            } if *object_id == card => Some(record),
            _ => None,
        });
        assert!(
            public_record.is_some(),
            "the settled replacement must emit a public event"
        );
        assert!(public_record.is_some_and(|record| {
            record
                .trigger_source_context
                .as_ref()
                .is_none_or(|context| !context.face_down)
        }));
        let opponent_view =
            crate::game::visibility::filter_events_for_viewer(&events, &state, PlayerId(1));
        assert!(opponent_view.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Graveyard),
                to: Zone::Battlefield,
                ..
            } if *object_id == card
        )));
    }

    #[test]
    fn face_down_exile_chained_away_and_back_keeps_exact_hidden_authority() {
        let mut state = GameState::new_two_player(42);
        let first_redirect = create_object(
            &mut state,
            CardId(90110),
            PlayerId(0),
            "Exile to Graveyard Redirect".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&first_redirect)
            .unwrap()
            .replacement_definitions
            .push(optional_zone_redirect(Zone::Exile, Zone::Graveyard));
        let second_redirect = create_object(
            &mut state,
            CardId(90111),
            PlayerId(0),
            "Graveyard to Exile Redirect".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&second_redirect)
            .unwrap()
            .replacement_definitions
            .push(optional_zone_redirect(Zone::Graveyard, Zone::Exile));
        let card = create_object(
            &mut state,
            CardId(90112),
            PlayerId(0),
            "Hidden Away And Back".to_string(),
            Zone::Library,
        );

        let search = face_down_self_search(first_redirect, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &search, &mut events, 0).unwrap();
        let selection = apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![card] },
        )
        .unwrap();
        events.extend(selection.events);

        let first_chooser = match &state.waiting_for {
            WaitingFor::ReplacementChoice { player, .. } => *player,
            other => panic!("expected first replacement choice, got {other:?}"),
        };
        state.priority_player = first_chooser;
        let first_resume = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept Exile-to-Graveyard replacement");
        events.extend(first_resume.events);

        let second_chooser = match &state.waiting_for {
            WaitingFor::ReplacementChoice { player, .. } => *player,
            other => panic!("expected second replacement choice, got {other:?}"),
        };
        state.priority_player = second_chooser;
        let second_resume =
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                .expect("accept Graveyard-to-Exile replacement");
        events.extend(second_resume.events);

        assert_eq!(state.objects[&card].zone, Zone::Exile);
        assert!(state.objects[&card].face_down);
        let hidden_record = events.iter().find_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Library),
                to: Zone::Exile,
                record,
            } if *object_id == card => Some(record),
            _ => None,
        });
        assert!(
            hidden_record.is_some(),
            "the final Exile event must be recorded"
        );
        assert!(hidden_record.is_some_and(|record| {
            record
                .trigger_source_context
                .as_ref()
                .is_some_and(|context| context.face_down)
        }));

        let authorized =
            crate::game::visibility::filter_events_for_viewer(&events, &state, PlayerId(0));
        let unauthorized =
            crate::game::visibility::filter_events_for_viewer(&events, &state, PlayerId(1));
        assert!(authorized.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Library),
                to: Zone::Exile,
                ..
            } if *object_id == card
        )));
        assert!(!unauthorized.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Library),
                to: Zone::Exile,
                ..
            } if *object_id == card
        )));
        assert!(!unauthorized
            .iter()
            .any(|event| matches!(event, GameEvent::HiddenSearchViewed { .. })));
    }

    #[test]
    fn face_down_exile_chained_battlefield_replacements_preserve_printed_face() {
        let mut state = GameState::new_two_player(42);
        let first_redirect = create_object(
            &mut state,
            CardId(90120),
            PlayerId(0),
            "Exile to Battlefield Redirect".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&first_redirect)
            .unwrap()
            .replacement_definitions
            .push(optional_zone_redirect(Zone::Exile, Zone::Battlefield));
        let second_redirect = create_object(
            &mut state,
            CardId(90121),
            PlayerId(0),
            "Battlefield Replacement".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&second_redirect)
            .unwrap()
            .replacement_definitions
            .push(optional_zone_redirect(Zone::Battlefield, Zone::Battlefield));
        let card = create_object(
            &mut state,
            CardId(90122),
            PlayerId(0),
            "Printed Battlefield Creature".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&card).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(6);
            obj.base_power = Some(6);
            obj.toughness = Some(7);
            obj.base_toughness = Some(7);
        }

        let search = face_down_self_search(first_redirect, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &search, &mut events, 0).unwrap();
        let selection = apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![card] },
        )
        .unwrap();
        events.extend(selection.events);

        for _ in 0..2 {
            let chooser = match &state.waiting_for {
                WaitingFor::ReplacementChoice { player, .. } => *player,
                other => panic!("expected chained replacement choice, got {other:?}"),
            };
            state.priority_player = chooser;
            let resumed = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                .expect("accept chained Battlefield replacement");
            events.extend(resumed.events);
        }

        let obj = state.objects.get(&card).unwrap();
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(!obj.face_down);
        assert!(obj.face_down_cause.is_none());
        assert_eq!(obj.name, "Printed Battlefield Creature");
        assert_eq!(obj.power, Some(6));
        assert_eq!(obj.toughness, Some(7));
        let public_record = events.iter().find_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Library),
                to: Zone::Battlefield,
                record,
            } if *object_id == card => Some(record),
            _ => None,
        });
        assert!(public_record.is_some());
        assert!(public_record.is_some_and(|record| {
            record
                .trigger_source_context
                .as_ref()
                .is_none_or(|context| !context.face_down)
        }));
        let opponent_view =
            crate::game::visibility::filter_events_for_viewer(&events, &state, PlayerId(1));
        assert!(opponent_view.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Library),
                to: Zone::Battlefield,
                ..
            } if *object_id == card
        )));
        assert!(!opponent_view
            .iter()
            .any(|event| matches!(event, GameEvent::HiddenSearchViewed { .. })));
    }

    /// W3 (CR 614.6): a NON-EXEMPT library placement now runs the replacement
    /// consult. Before W3 the placement arm skipped `replace_event` and delivered
    /// straight to the library index, so the redirect below was silently dropped
    /// and the card landed in the library. With the consult running, the
    /// board-wide "put into library → exile instead" redirect fires and the card
    /// lands in EXILE — the discriminating behavior change.
    #[test]
    fn library_placement_consults_moved_redirect() {
        let mut state = GameState::new_two_player(42);
        let redirect_source = install_library_to_exile_redirect(&mut state);
        let card = create_object(
            &mut state,
            CardId(90002),
            PlayerId(0),
            "Redirected Card".to_string(),
            Zone::Graveyard,
        );

        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(card, Zone::Library, redirect_source)
                .at_library_position(LibraryPosition::Top),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        // The redirect sent the card to exile instead of the library.
        assert_eq!(state.objects[&card].zone, Zone::Exile);
        assert!(!state.players[0].library.contains(&card));
    }

    /// W3 (CR 701.24a): a NON-EXEMPT library placement with no redirect places the
    /// object at the requested index and does NOT shuffle the library — a placement
    /// instruction is not a shuffle instruction (CR 701.24a defines shuffling).
    /// Seeds a deterministic three-card library and asserts the placed card lands
    /// on top with the existing order preserved AND that no shuffle event fired
    /// (so a seed-identity permutation cannot false-pass).
    #[test]
    fn library_placement_does_not_shuffle() {
        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Library,
        );
        let c = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "C".to_string(),
            Zone::Library,
        );
        // Deterministic order: [A, B, C] (index 0 = top).
        state.players[0].library = crate::im::vector![a, b, c];

        let placed = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Placed".to_string(),
            Zone::Graveyard,
        );
        let mover = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Mover".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(placed, Zone::Library, mover)
                .at_library_position(LibraryPosition::Top),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        // Placed on top; the existing order is untouched (no shuffle).
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            vec![placed, a, b, c]
        );
        // CR 701.24a robustness: assert no shuffle event fired. The order check
        // above could false-pass under a seed-identity permutation; the absence of
        // a `ShuffledLibrary` event proves the placement suppressed the tail's
        // auto-shuffle convention rather than a shuffle merely landing on the same
        // order.
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::PlayerPerformedAction {
                    action: crate::types::events::PlayerActionKind::ShuffledLibrary,
                    ..
                }
            )),
            "a placement must not emit a shuffle event (CR 701.24a: a placement is not a shuffle)"
        );
    }

    #[test]
    fn library_card_into_its_own_library_moves_only_with_a_placement() {
        let mut state = GameState::new_two_player(42);
        let [a, b, c] = [1, 2, 3].map(|n| {
            create_object(
                &mut state,
                CardId(n),
                PlayerId(0),
                format!("Card {n}"),
                Zone::Library,
            )
        });
        state.players[0].library = crate::im::vector![a, b, c];
        let mover = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Mover".to_string(),
            Zone::Battlefield,
        );
        let library =
            |state: &GameState| state.players[0].library.iter().copied().collect::<Vec<_>>();

        let mut events = Vec::new();
        move_object(
            &mut state,
            ZoneMoveRequest::effect(b, Zone::Library, mover),
            &mut events,
        );
        assert_eq!(library(&state), vec![a, b, c]);
        assert!(events.is_empty(), "{events:?}");

        move_object(
            &mut state,
            ZoneMoveRequest::effect(c, Zone::Library, mover)
                .at_library_position(LibraryPosition::Top),
            &mut events,
        );
        assert_eq!(library(&state), vec![c, a, b]);
        assert!(
            events.is_empty(),
            "same-library reorder emits no zone change"
        );
    }

    /// F1 (CR 701.24a): a library placement whose replacement consult PARKS on a
    /// player choice must survive the park/resume round-trip — the resumed
    /// delivery must place the object at the requested index, NOT let the tail
    /// auto-shuffle the position away.
    ///
    /// Synthetic, because no pool `Moved` def targets the library, so a placement
    /// consult never reaches a real choice today. Install an OPTIONAL library →
    /// exile redirect: the optional accept/decline prompt forces `move_object` to
    /// park (`NeedsChoice`); DECLINING (index 1) leaves the event as the original
    /// plain library `ZoneChange`, so the resume delivers it to the library — and
    /// must honor the parked `LibraryPosition::Top`. Before the placement was
    /// threaded onto `PendingReplacement`, the resume hardcoded
    /// `library_placement: None` and the tail shuffled the library, randomizing
    /// the requested position.
    #[test]
    fn library_placement_parked_resume_honors_position() {
        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Library,
        );
        // Deterministic order [A, B] (index 0 = top).
        state.players[0].library = crate::im::vector![a, b];

        // Optional library→exile redirect (parks for the accept/decline choice).
        let redirect_source = create_object(
            &mut state,
            CardId(90003),
            PlayerId(0),
            "Optional Library Redirect".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&redirect_source)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Exile,
                            target: TargetFilter::Any,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    ))
                    .destination_zone(Zone::Library),
            );

        let placed = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Placed".to_string(),
            Zone::Graveyard,
        );

        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(placed, Zone::Library, redirect_source)
                .at_library_position(LibraryPosition::Top),
            &mut events,
        );

        // The optional redirect parked the placement on a player choice.
        let ZoneMoveResult::NeedsChoice(chooser) = result else {
            panic!("expected the optional redirect to park, got a non-pausing result");
        };
        assert_parked_position(&state, placed, Some(LibraryPosition::Top));
        assert_eq!(state.objects[&placed].zone, Zone::Graveyard);
        assert!(
            state.pending_replacement.is_some(),
            "the parked record must carry the placement for the resume to thread back"
        );
        assert_eq!(
            state
                .pending_replacement
                .as_ref()
                .and_then(|p| event_placement(&p.proposed)),
            Some(LibraryPosition::Top),
            "the parked record must stash the requested library placement"
        );

        // DECLINE the redirect (index 1) — the event resolves as the original
        // plain library ZoneChange, so the resume delivers to the library.
        state.priority_player = chooser;
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("resume replacement choice");

        // Placed at the requested top index; the existing order is preserved.
        assert_eq!(state.objects[&placed].zone, Zone::Library);
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            vec![placed, a, b],
            "the resumed delivery must honor LibraryPosition::Top, not shuffle the position away"
        );
        assert_finished(&state);
    }

    /// First wire key occurrence, depth-first across objects and arrays.
    fn find_key<'a>(node: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
        match node {
            serde_json::Value::Object(object) => object
                .get(key)
                .or_else(|| object.values().find_map(|value| find_key(value, key))),
            serde_json::Value::Array(items) => items.iter().find_map(|value| find_key(value, key)),
            _ => None,
        }
    }

    /// Mutable counterpart of [`find_key`].
    fn find_key_mut<'a>(
        node: &'a mut serde_json::Value,
        key: &str,
    ) -> Option<&'a mut serde_json::Value> {
        match node {
            serde_json::Value::Object(object) => {
                if object.contains_key(key) {
                    return object.get_mut(key);
                }
                object
                    .values_mut()
                    .find_map(|value| find_key_mut(value, key))
            }
            serde_json::Value::Array(items) => {
                items.iter_mut().find_map(|value| find_key_mut(value, key))
            }
            _ => None,
        }
    }

    /// F-B fixture: a two-member positioned batch whose first move parks on an
    /// optional Library→Exile redirect. Returns the untouched library sentinels
    /// so callers can assert the final bottom order.
    fn positioned_batch_fixture() -> (GameState, ObjectId, ObjectId, Vec<ObjectId>) {
        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Library,
        );
        state.players[0].library = crate::im::vector![a, b];

        let redirect_source = create_object(
            &mut state,
            CardId(90006),
            PlayerId(0),
            "Optional Library Redirect".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&redirect_source)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Exile,
                            target: TargetFilter::Any,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    ))
                    .destination_zone(Zone::Library),
            );

        let first = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "First".to_string(),
            Zone::Graveyard,
        );
        let second = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Second".to_string(),
            Zone::Graveyard,
        );
        (state, first, second, vec![a, b])
    }

    /// F-B (CR 616.1 + CR 701.24a): a batch tail must preserve explicit library
    /// placement across a pause. The first card parks on an optional
    /// Library→Exile redirect; the undelivered tail is stashed in
    /// `PendingBatchDeliveries`. Declining the first redirect drains the tail,
    /// which parks again on the second card. Both the stashed tail and the second
    /// parked replacement must carry `LibraryPosition::Bottom`; otherwise the
    /// second final delivery becomes a plain Library move and auto-shuffles.
    #[test]
    fn batch_library_placement_tail_survives_pause() {
        let (mut state, first, second, sentinels) = positioned_batch_fixture();
        let reqs = vec![
            ZoneMoveRequest::effect(first, Zone::Library, first)
                .at_library_position(LibraryPosition::Bottom),
            ZoneMoveRequest::effect(second, Zone::Library, second)
                .at_library_position(LibraryPosition::Bottom),
        ];

        let mut events = Vec::new();
        assert!(matches!(
            move_objects_simultaneously(&mut state, reqs, &mut events),
            BatchMoveResult::NeedsChoice
        ));
        assert_parked_position(&state, first, Some(LibraryPosition::Bottom));
        assert_eq!(
            state
                .active_batch_delivery()
                .map(|pending| pending.remaining.clone()),
            Some(vec![second]),
            "the first park must stash the undelivered tail"
        );
        assert_eq!(
            state
                .active_batch_delivery()
                .and_then(|pending| pending.library_placement.clone()),
            Some(LibraryPosition::Bottom),
            "the stashed tail must preserve bottom placement"
        );
        let logical_group_id = state
            .active_batch_delivery()
            .expect("the first paused member retains a batch owner")
            .logical_zone_change_group
            .logical_group_id;

        let first_resume = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("decline first optional redirect");
        assert_parked_position(&state, second, Some(LibraryPosition::Bottom));
        let final_member_pause = state
            .active_batch_delivery()
            .expect("a pause on the final member still retains the batch owner");
        assert_eq!(
            final_member_pause
                .logical_zone_change_group
                .logical_group_id,
            logical_group_id,
            "a re-park must carry the original logical group rather than a tail-only owner"
        );
        assert!(
            final_member_pause.remaining.is_empty() && final_member_pause.paused_current.is_some(),
            "the final paused member retains an owner even with no undelivered tail"
        );
        assert_eq!(
            state
                .pending_replacement
                .as_ref()
                .and_then(|pending| event_placement(&pending.proposed)),
            Some(LibraryPosition::Bottom),
            "the second card's re-parked replacement must preserve bottom placement"
        );

        let second_resume =
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
                .expect("decline second optional redirect");
        assert!(
            !second_resume.events.iter().any(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: crate::types::events::PlayerActionKind::ShuffledLibrary,
                    ..
                }
            )),
            "explicit bottom placement must not become an auto-shuffled library move"
        );
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            sentinels
                .iter()
                .copied()
                .chain([first, second])
                .collect::<Vec<_>>(),
            "both declined batch moves must land on the bottom in request order"
        );
        for card in [first, second] {
            assert_eq!(first_resume.events.iter().chain(&second_resume.events)
                .filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == card)).count(), 1);
        }
        assert_finished(&state);
    }

    /// CR 616.1 + save compatibility: an authentic old positioned batch save
    /// loses the canonical placement from the replacement proposal, its pinned
    /// boundary (when present) and the exact paused owner's expected event. All
    /// three copies must be normalized from the effective boundary before typed
    /// decoding, for raw, trusted and versioned ingress, and the resumed public
    /// choices must still place both members exactly once.
    #[test]
    fn legacy_positioned_batch_migration_normalizes_every_exact_owner_copy() {
        fn strip_event_placement(event: &mut serde_json::Value) {
            if let Some(zone) = event
                .get_mut("ZoneChange")
                .and_then(serde_json::Value::as_object_mut)
            {
                zone.remove("library_placement");
            }
        }

        fn strip_wire_placement(node: &mut serde_json::Value) {
            match node {
                serde_json::Value::Array(items) => {
                    items.iter_mut().for_each(strip_wire_placement);
                }
                serde_json::Value::Object(object) => {
                    for key in ["pending_replacement", "pending_entry", "outer_replacement"] {
                        if let Some(carrier) = object
                            .get_mut(key)
                            .and_then(serde_json::Value::as_object_mut)
                        {
                            if let Some(proposed) = carrier.get_mut("proposed") {
                                strip_event_placement(proposed);
                            }
                            if let Some(boundary) = carrier.get_mut("delivery_boundary") {
                                strip_event_placement(boundary);
                            }
                        }
                    }
                    if let Some(expected) = object
                        .get_mut("paused_current")
                        .and_then(|paused| paused.get_mut("expected_event"))
                    {
                        strip_event_placement(expected);
                    }
                    for child in object.values_mut() {
                        strip_wire_placement(child);
                    }
                }
                _ => {}
            }
        }

        let bottom = serde_json::json!({"type": "Bottom"});
        let (mut state, first, second, sentinels) = positioned_batch_fixture();
        let reqs = vec![
            ZoneMoveRequest::effect(first, Zone::Library, first)
                .at_library_position(LibraryPosition::Bottom),
            ZoneMoveRequest::effect(second, Zone::Library, second)
                .at_library_position(LibraryPosition::Bottom),
        ];
        let mut parked_events = Vec::new();
        assert!(matches!(
            move_objects_simultaneously(&mut state, reqs, &mut parked_events),
            BatchMoveResult::NeedsChoice
        ));
        assert_parked_position(&state, first, Some(LibraryPosition::Bottom));

        let raw = serde_json::to_value(&state).expect("positioned batch serializes");
        let trusted = serde_json::to_value(PersistedGameState::capture(state.clone()))
            .expect("trusted positioned batch serializes");
        let versioned = serde_json::to_value(ResolutionStateWire::from_game_state(state.clone()))
            .expect("versioned positioned batch serializes");

        for (ingress, wire) in [("raw", raw), ("trusted", trusted), ("versioned", versioned)] {
            let mut wire = wire;
            strip_wire_placement(&mut wire);
            let carrier = find_key(&wire, "pending_replacement")
                .and_then(serde_json::Value::as_object)
                .expect("the authentic pause retains its replacement carrier");
            assert!(carrier["proposed"]["ZoneChange"]
                .get("library_placement")
                .is_none());
            let owner = find_key(&wire, "paused_current")
                .and_then(serde_json::Value::as_object)
                .expect("the authentic pause retains its exact owner");
            assert!(owner["expected_event"]["ZoneChange"]
                .get("library_placement")
                .is_none());

            let mut restored = match ingress {
                "raw" => serde_json::from_value::<GameState>(wire)
                    .expect("legacy raw positioned batch restores"),
                "trusted" => serde_json::from_value::<PersistedGameState>(wire)
                    .expect("legacy trusted positioned batch restores")
                    .into_game_state()
                    .expect("legacy trusted positioned batch materializes"),
                _ => serde_json::from_value::<ResolutionStateWire>(wire)
                    .expect("legacy versioned positioned batch restores")
                    .into_game_state(),
            };
            assert_parked_position(&restored, first, Some(LibraryPosition::Bottom));

            let rewritten = serde_json::to_value(&restored).expect("restored pause serializes");
            let carrier = find_key(&rewritten, "pending_replacement")
                .and_then(serde_json::Value::as_object)
                .expect("restored pause retains its replacement carrier");
            assert_eq!(
                carrier["proposed"]["ZoneChange"]["library_placement"], bottom,
                "{ingress}: the proposal imports the boundary placement"
            );
            let owner = find_key(&rewritten, "paused_current")
                .and_then(serde_json::Value::as_object)
                .expect("restored pause retains its exact owner");
            assert_eq!(
                owner["expected_event"]["ZoneChange"]["library_placement"], bottom,
                "{ingress}: the exact owner copy imports the same placement"
            );

            let mut resumed_events = Vec::new();
            resumed_events.extend(
                apply_as_current(&mut restored, GameAction::ChooseReplacement { index: 1 })
                    .expect("decline the first legacy-resumed redirect")
                    .events,
            );
            assert_parked_position(&restored, second, Some(LibraryPosition::Bottom));
            resumed_events.extend(
                apply_as_current(&mut restored, GameAction::ChooseReplacement { index: 1 })
                    .expect("decline the second legacy-resumed redirect")
                    .events,
            );
            assert_eq!(
                restored.players[0]
                    .library
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                sentinels
                    .iter()
                    .copied()
                    .chain([first, second])
                    .collect::<Vec<_>>(),
                "{ingress}: both restored members land on the bottom in request order"
            );
            for card in [first, second] {
                assert_eq!(
                    resumed_events
                        .iter()
                        .filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == card))
                        .count(),
                    1,
                    "{ingress}: each original member has one terminal occurrence"
                );
            }
            assert_no_shuffle(&resumed_events);
            assert_finished(&restored);
        }
    }

    /// CR 616.1: an explicit current placement is authoritative over the legacy
    /// carrier mirror, and the exact owner copy is filled from the boundary the
    /// proposal itself records, not from that mirror.
    #[test]
    fn legacy_positioned_batch_current_placement_wins_over_mirror() {
        let (state, first, _, _) = positioned_batch_fixture();
        // Build the pause from the same production route, then inspect its
        // carrier without mutating the proposal's explicit Bottom.
        let mut parked = state;
        let mut events = Vec::new();
        assert!(matches!(
            move_objects_simultaneously(
                &mut parked,
                vec![ZoneMoveRequest::effect(first, Zone::Library, first)
                    .at_library_position(LibraryPosition::Bottom)],
                &mut events
            ),
            BatchMoveResult::NeedsChoice
        ));
        let mut wire = serde_json::to_value(&parked).expect("positioned pause serializes");
        let top = serde_json::json!({"type": "Top"});
        let bottom = serde_json::json!({"type": "Bottom"});
        let carrier = find_key_mut(&mut wire, "pending_replacement")
            .and_then(serde_json::Value::as_object_mut)
            .expect("the pause retains its replacement carrier");
        assert_eq!(
            carrier["proposed"]["ZoneChange"]["library_placement"],
            bottom
        );
        carrier["library_placement"] = top.clone();
        let owner = find_key_mut(&mut wire, "paused_current")
            .and_then(serde_json::Value::as_object_mut)
            .expect("the pause retains its exact owner");
        owner["expected_event"]["ZoneChange"]
            .as_object_mut()
            .expect("owner expected event is a ZoneChange")
            .remove("library_placement");

        let restored: GameState =
            serde_json::from_value(wire).expect("mirror is not authoritative");
        assert_parked_position(&restored, first, Some(LibraryPosition::Bottom));
        let rewritten = serde_json::to_value(&restored).expect("restored pause serializes");
        let carrier = find_key(&rewritten, "pending_replacement").expect("carrier");
        assert_eq!(
            carrier["library_placement"], top,
            "legacy mirror is preserved verbatim"
        );
        assert_eq!(
            carrier["proposed"]["ZoneChange"]["library_placement"], bottom,
            "the explicit proposal value survives"
        );
        let owner = find_key(&rewritten, "paused_current").expect("owner");
        assert_eq!(
            owner["expected_event"]["ZoneChange"]["library_placement"], bottom,
            "an absent owner copy follows the boundary's explicit value, not the mirror"
        );
    }

    /// CR 616.1: one identified owner cannot be normalized from two different
    /// exact copies; the save boundary rejects instead of silently choosing.
    #[test]
    fn legacy_positioned_batch_rejects_conflicting_owner_copies() {
        let (state, first, _, _) = positioned_batch_fixture();
        let mut parked = state;
        let mut events = Vec::new();
        assert!(matches!(
            move_objects_simultaneously(
                &mut parked,
                vec![ZoneMoveRequest::effect(first, Zone::Library, first)
                    .at_library_position(LibraryPosition::Bottom)],
                &mut events
            ),
            BatchMoveResult::NeedsChoice
        ));
        let mut wire = serde_json::to_value(&parked).expect("positioned pause serializes");
        {
            let carrier = find_key_mut(&mut wire, "pending_replacement")
                .and_then(serde_json::Value::as_object_mut)
                .expect("the pause retains its replacement carrier");
            carrier["proposed"]["ZoneChange"]
                .as_object_mut()
                .expect("proposal is a ZoneChange")
                .remove("library_placement");
            carrier["library_placement"] = serde_json::json!({"type": "Bottom"});
        }
        {
            let owner = find_key_mut(&mut wire, "paused_current")
                .and_then(serde_json::Value::as_object_mut)
                .expect("the pause retains its exact owner");
            owner["expected_event"]["ZoneChange"]
                .as_object_mut()
                .expect("owner expected event is a ZoneChange")
                .remove("library_placement");
        }
        // A second exact carrier copy identifies the same owner but carries a
        // different legacy mirror, so the missing owner key is ambiguous.
        let mut other = find_key(&wire, "pending_replacement")
            .expect("the pause retains its replacement carrier")
            .clone();
        other["library_placement"] = serde_json::json!({"type": "Top"});
        wire["pending_entry"] = other;
        assert!(
            serde_json::from_value::<GameState>(wire).is_err(),
            "two exact owner copies with different placements must be rejected"
        );
    }

    /// CR 616.1 + save compatibility: a real pinned delivery boundary can be
    /// older than a redirected proposal. The migration follows the boundary copy
    /// and never transplants the proposal's new destination or placement onto it.
    #[test]
    fn legacy_positioned_batch_pinned_boundary_survives_redirected_proposal() {
        let (state, first, _, _) = positioned_batch_fixture();
        let mut parked = state;
        let mut events = Vec::new();
        assert!(matches!(
            move_objects_simultaneously(
                &mut parked,
                vec![ZoneMoveRequest::effect(first, Zone::Library, first)
                    .at_library_position(LibraryPosition::Bottom)],
                &mut events
            ),
            BatchMoveResult::NeedsChoice
        ));
        parked
            .pending_replacement
            .as_mut()
            .expect("authentic positioned pause")
            .pin_delivery_boundary();
        let pinned = parked
            .pending_replacement
            .as_ref()
            .expect("authentic positioned pause")
            .delivery_boundary_event()
            .clone();
        assert!(
            matches!(
                &pinned,
                ProposedEvent::ZoneChange {
                    to: Zone::Library,
                    library_placement: Some(LibraryPosition::Bottom),
                    ..
                }
            ),
            "the production pin retains the original positioned boundary"
        );

        let bottom = serde_json::json!({"type": "Bottom"});
        let mut wire = serde_json::to_value(&parked).expect("positioned pause serializes");
        // A redirect applied after the pin leaves the proposal at a different
        // destination, while the owner still parked on the original boundary.
        {
            let carrier = find_key_mut(&mut wire, "pending_replacement")
                .and_then(serde_json::Value::as_object_mut)
                .expect("the pause retains its replacement carrier");
            carrier["proposed"]["ZoneChange"]["to"] = serde_json::json!("Exile");
            carrier["proposed"]["ZoneChange"]
                .as_object_mut()
                .expect("proposal is a ZoneChange")
                .remove("library_placement");
            carrier["delivery_boundary"]["ZoneChange"]
                .as_object_mut()
                .expect("the pinned boundary is a ZoneChange")
                .remove("library_placement");
        }
        {
            let owner = find_key_mut(&mut wire, "paused_current")
                .and_then(serde_json::Value::as_object_mut)
                .expect("the pause retains its exact owner");
            owner["expected_event"]["ZoneChange"]
                .as_object_mut()
                .expect("owner expected event is a ZoneChange")
                .remove("library_placement");
        }
        let carrier = find_key(&wire, "pending_replacement").expect("carrier");
        assert_ne!(
            carrier["proposed"], carrier["delivery_boundary"],
            "the legacy shape has a redirected proposal older than its pinned boundary"
        );

        let restored: GameState =
            serde_json::from_value(wire).expect("pinned-boundary save restores");
        let pending = restored
            .pending_replacement
            .as_ref()
            .expect("pause restores");
        assert!(matches!(
            &pending.proposed,
            ProposedEvent::ZoneChange {
                to: Zone::Exile,
                library_placement: None,
                ..
            }
        ));
        assert_eq!(
            pending.delivery_boundary_event(),
            &pinned,
            "the typed pinned boundary is unchanged by normalization"
        );

        let rewritten = serde_json::to_value(&restored).expect("restored pause serializes");
        let carrier = find_key(&rewritten, "pending_replacement").expect("carrier");
        assert_eq!(
            carrier["proposed"]["ZoneChange"]["library_placement"],
            serde_json::Value::Null,
            "the exiling proposal keeps an explicit null placement"
        );
        assert_eq!(
            carrier["delivery_boundary"]["ZoneChange"]["library_placement"], bottom,
            "the older library boundary imports the mirror placement"
        );
        let owner = find_key(&rewritten, "paused_current").expect("owner");
        assert_eq!(
            owner["expected_event"]["ZoneChange"]["library_placement"], bottom,
            "the exact owner copy follows the pinned boundary, not the proposal"
        );

        let again: GameState =
            serde_json::from_value(serde_json::to_value(&restored).expect("state serializes"))
                .expect("pinned-boundary save restores idempotently");
        assert_eq!(again, restored);
    }

    /// F-A (CR 616.1 + CR 701.24a): the library placement must survive a SECOND
    /// sequential park on the same event. The first optional redirect parks (the
    /// placement is stashed onto `PendingReplacement` by the W3 arm); declining
    /// it re-enters `pipeline_loop`, which finds a SECOND optional redirect that
    /// became applicable in the interim and re-parks a fresh `PendingReplacement`
    /// — created with `library_placement: None`. `handle_replacement_choice` must
    /// thread the captured placement onto that re-park so the FINAL delivery
    /// (after declining both) still places the card at the requested index
    /// instead of the tail auto-shuffling it away.
    ///
    /// Supplemental synthetic defensive fixture, not a legal public sequence:
    /// the second redirect is gated by `UnlessControlsMatching` on a sentinel
    /// creature so it is suppressed on the first scan and becomes applicable once
    /// the sentinel is manually removed between the two choices. The separate
    /// `canonical_placement_survives_public_repeated_choices` regression covers
    /// the public sequence without post-prompt edits. Before the fix the re-park reset the
    /// placement to `None`, so the final delivery shuffled — the order assertion
    /// below fails (and the `ShuffledLibrary` absence assertion guards against a
    /// seed-identity permutation false-pass).
    #[test]
    fn library_placement_survives_two_sequential_parks() {
        fn optional_library_exile_redirect(
            condition: Option<ReplacementCondition>,
        ) -> ReplacementDefinition {
            let mut def = ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Exile,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
                    },
                ))
                .destination_zone(Zone::Library);
            if let Some(condition) = condition {
                def = def.condition(condition);
            }
            def
        }

        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Library,
        );
        state.players[0].library = crate::im::vector![a, b];

        // Sentinel creature that suppresses the second redirect until removed.
        let sentinel = create_object(
            &mut state,
            CardId(90010),
            PlayerId(0),
            "Sentinel".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&sentinel)
            .unwrap()
            .card_types
            .core_types = vec![crate::types::card_type::CoreType::Creature];

        // Redirect #1: always applicable. Redirect #2: suppressed while the
        // controller controls a creature (the sentinel).
        let r1 = create_object(
            &mut state,
            CardId(90004),
            PlayerId(0),
            "Redirect One".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&r1)
            .unwrap()
            .replacement_definitions
            .push(optional_library_exile_redirect(None));

        let r2 = create_object(
            &mut state,
            CardId(90005),
            PlayerId(0),
            "Redirect Two".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&r2)
            .unwrap()
            .replacement_definitions
            .push(optional_library_exile_redirect(Some(
                ReplacementCondition::UnlessControlsMatching {
                    filter: TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Creature)
                            .controller(crate::types::ability::ControllerRef::You),
                    ),
                },
            )));

        let placed = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Placed".to_string(),
            Zone::Graveyard,
        );

        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(placed, Zone::Library, placed)
                .at_library_position(LibraryPosition::Top),
            &mut events,
        );

        // Only redirect #1 applies (the sentinel suppresses #2), so this is a
        // single-candidate optional park that stashes the placement.
        let ZoneMoveResult::NeedsChoice(chooser) = result else {
            panic!("expected the first optional redirect to park, got a non-pausing result");
        };
        assert_eq!(
            state
                .pending_replacement
                .as_ref()
                .and_then(|p| event_placement(&p.proposed)),
            Some(LibraryPosition::Top),
            "the first parked record must stash the requested library placement"
        );

        // Remove the sentinel so redirect #2 becomes applicable on the re-scan.
        state.battlefield.retain(|id| *id != sentinel);
        state.objects.remove(&sentinel);

        // Decline the first redirect — the resume re-enters pipeline_loop, finds
        // redirect #2 now applicable, and re-parks. Without the fix this re-park
        // carries `library_placement: None`.
        state.priority_player = chooser;
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("resume first replacement choice");

        assert!(
            state.pending_replacement.is_some(),
            "the second optional redirect must re-park after the sentinel is removed"
        );
        assert_eq!(
            state
                .pending_replacement
                .as_ref()
                .and_then(|p| event_placement(&p.proposed)),
            Some(LibraryPosition::Top),
            "the re-parked record must still carry the placement threaded from the first park",
        );

        // Decline the second redirect — the event resolves as the original plain
        // library ZoneChange and delivers to the library at the requested index.
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("resume second replacement choice");

        // The discriminating assertion: the placed card must land at the requested
        // top index with the existing order preserved. Before the fix the second
        // park reset the placement to `None` and the delivery tail auto-shuffled
        // the requested position away.
        assert_eq!(state.objects[&placed].zone, Zone::Library);
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            vec![placed, a, b],
            "after two declined parks the placement must still honor LibraryPosition::Top"
        );
        assert_finished(&state);
    }

    /// A newly produced simultaneous move must park as a child of an existing
    /// BatchDelivery owner, not replace that parent. The real batch producer
    /// below reaches a CR 616.1 ordering prompt; this fails if initial parking
    /// uses the re-pause transition instead of `push_batch_delivery`.
    #[test]
    fn new_batch_delivery_parks_inside_an_existing_batch_parent() {
        let mut state = GameState::new_two_player(43);
        let mut parent_group = state.allocate_logical_zone_change_group(&[]);
        parent_group
            .latch_immediately_before(Vec::new(), Vec::new())
            .expect("parent batch retains its pre-delivery latch");
        let parent_group_id = parent_group.logical_group_id;
        state.push_batch_delivery(PendingBatchDeliveries {
            component_delivery_parent: None,
            logical_zone_change_group: parent_group,
            paused_current: None,
            remaining: Vec::new(),
            destination: Zone::Graveyard,
            source_id: None,
            enter_tapped: EtbTapState::Unspecified,
            exile_tracking: ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: None,
            replacement_applied: HashSet::new(),
            requests: Vec::new(),
            attempted: Vec::new(),
            zone_change_record_start: 0,
            deferred_events: Vec::new(),
            staged: None,
        });
        install_library_to_exile_redirect(&mut state);
        install_library_to_exile_redirect(&mut state);
        let first = create_object(
            &mut state,
            CardId(90031),
            PlayerId(0),
            "Nested batch first".to_string(),
            Zone::Graveyard,
        );
        let second = create_object(
            &mut state,
            CardId(90032),
            PlayerId(0),
            "Nested batch second".to_string(),
            Zone::Graveyard,
        );

        assert!(matches!(
            move_objects_simultaneously(
                &mut state,
                vec![
                    ZoneMoveRequest::effect(first, Zone::Library, first),
                    ZoneMoveRequest::effect(second, Zone::Library, second),
                ],
                &mut Vec::new(),
            ),
            BatchMoveResult::NeedsChoice
        ));
        let frames = state.resolution_stack.iter().collect::<Vec<_>>();
        assert!(matches!(
            frames.as_slice(),
            [ResolutionFrame::BatchDelivery(parent), ResolutionFrame::BatchDelivery(child)]
                if parent.logical_zone_change_group.logical_group_id == parent_group_id
                    && child.remaining == vec![second]
        ));
    }
}

#[cfg(test)]
mod parsed_leyline_card_scoping_tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0, P1};
    use crate::game::triggers::process_triggers;
    use crate::parser::oracle_replacement::parse_replacement_line;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerDefinition,
    };
    use crate::types::triggers::TriggerMode;

    /// End-to-end pin of the live Leyline of the Void bug (zone pipeline
    /// tranche 3, parser card-scoping): the def installed here is the REAL
    /// PARSED output of Leyline's oracle line — not a hand-built mirror — so
    /// any parser-shape drift that breaks the matcher path turns this red.
    ///
    /// CR 111.1: tokens are not cards, so Leyline's "a card" subject must NOT
    /// match a dying token: the opponent's token reaches the GRAVEYARD (its
    /// dies-trigger fires per CR 603.6c look-back, then CR 111.7 ceases it),
    /// while an opponent's dying nontoken CARD is exiled instead (CR 614.6).
    #[test]
    fn parsed_leyline_token_dies_to_graveyard_card_is_exiled() {
        let mut sc = GameScenario::new();
        let leyline = sc.add_creature(P0, "Leyline of the Void", 0, 0).id();
        let token = sc.add_creature(P1, "Zombie Token", 2, 2).id();
        let card_creature = sc.add_creature(P1, "Zombie", 2, 2).id();
        let mut state = sc.state;
        state.objects.get_mut(&token).unwrap().is_token = true;

        let def = parse_replacement_line(
            "If a card would be put into an opponent's graveyard from anywhere, exile it instead.",
            "Leyline of the Void",
        )
        .expect("Leyline of the Void's replacement line must parse");
        state
            .objects
            .get_mut(&leyline)
            .unwrap()
            .replacement_definitions
            .push(def);

        // Blood Artist-class observable: a self-scoped dies trigger on the token.
        state
            .objects
            .get_mut(&token)
            .unwrap()
            .trigger_definitions
            .push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .valid_card(TargetFilter::SelfRef)
                    .origin(Zone::Battlefield)
                    .destination(Zone::Graveyard)
                    .trigger_zones(vec![Zone::Battlefield])
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: TargetFilter::Controller,
                        },
                    ))
                    .description("When this creature dies, you gain 1 life.".to_string()),
            );

        // The opponent's TOKEN dies through the real pipeline.
        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(token, Zone::Graveyard, token),
            &mut events,
        );
        assert!(matches!(result, ZoneMoveResult::Done));
        assert_eq!(
            state.objects[&token].zone,
            Zone::Graveyard,
            "CR 111.1: 'a card' excludes tokens — the dying token must reach the \
             graveyard, not be exiled (the pre-tranche-3 live bug)"
        );
        process_triggers(&mut state, &events);
        assert!(
            !state.stack.is_empty(),
            "the token's dies-trigger must fire (CR 603.6c look-back) — exiling \
             it instead suppressed Blood Artist-class triggers"
        );

        // Contrast: the opponent's nontoken CARD is exiled by the same def.
        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(card_creature, Zone::Graveyard, card_creature),
            &mut events,
        );
        assert!(matches!(result, ZoneMoveResult::Done));
        assert_eq!(
            state.objects[&card_creature].zone,
            Zone::Exile,
            "CR 614.6: the opponent's dying nontoken card is exiled instead"
        );
    }
}

#[cfg(test)]
mod face_down_exile_entry_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        FaceDownProfile, FilterProp, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::statics::StaticMode;

    /// CR 708.2a + CR 400.4a + CR 400.7: a NON-permanent (instant/sorcery) card
    /// put onto the battlefield face down from EXILE must still enter as a
    /// face-down 2/2 creature.
    ///
    /// This pins the Exile-origin corner of the manifest/face-down entry path.
    /// `move_to_zone` runs the instant/sorcery battlefield-entry guard BEFORE
    /// `apply_zone_exit_cleanup`, so the early pre-flag in
    /// `deliver_replaced_zone_change` is what carries the non-permanent past the
    /// guard. But `apply_zone_exit_cleanup` then clears `face_down` on every
    /// exile exit (the CR 400.7 foretold/exile reset), so the final face-down
    /// state must be re-asserted by `apply_face_down_entry_profile` after the
    /// move. Without that authoritative re-assertion the card would land on the
    /// battlefield face UP, leaking the hidden card. A Library/Hand origin never
    /// hits that exile reset, so Exile is the discriminating origin to test.
    #[test]
    fn nonpermanent_manifested_from_exile_enters_face_down() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(70001),
            PlayerId(0),
            "Manifest Source".to_string(),
            Zone::Battlefield,
        );
        let card = create_object(
            &mut state,
            CardId(70002),
            PlayerId(0),
            "Hidden Instant".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&card).unwrap();
            obj.card_types.core_types = vec![CoreType::Instant];
            obj.base_card_types = obj.card_types.clone();
        }

        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(card, Zone::Battlefield, source)
                .face_down(FaceDownProfile::vanilla_2_2()),
            &mut events,
        );

        assert!(matches!(result, ZoneMoveResult::Done));
        let obj = state.objects.get(&card).expect("manifested object");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "a non-permanent put onto the battlefield face down from exile must \
             enter, not be bounced by the instant/sorcery guard"
        );
        assert!(
            obj.face_down,
            "the exile-exit cleanup clears face_down mid-move (CR 400.7); the \
             entry must re-assert it so the card does not leak face up"
        );
        assert_eq!(obj.power, Some(2), "a face-down card is a 2/2 (CR 708.2a)");
        assert_eq!(
            obj.toughness,
            Some(2),
            "a face-down card is a 2/2 (CR 708.2a)"
        );
        assert!(
            obj.card_types.core_types.contains(&CoreType::Creature),
            "a face-down card presents as a creature regardless of its hidden type"
        );
        assert!(
            obj.back_face.is_some(),
            "the real (hidden) card must be preserved in back_face for turn-face-up"
        );
    }

    /// CR 614.1d regression: a face-down (manifest/morph) entry BLOCKED by a
    /// `CantEnterBattlefieldFrom` static (Grafdigger's Cage) must leave the card
    /// completely unchanged in its origin zone — never stranded face down.
    ///
    /// `deliver_replaced_zone_change` flags the object face down up front so the
    /// instant/sorcery battlefield-entry guard accepts a manifested non-permanent.
    /// But `move_to_zone` separately rejects the entry (returning without moving)
    /// when Grafdigger's Cage blocks a creature card in a graveyard/library. The
    /// preflight flag must then be rolled back AND the face-down profile must not
    /// be applied — otherwise a blocked manifest would corrupt the hidden card
    /// left behind in the library (it would be marked face down / morphed in place
    /// for a move that never happened).
    #[test]
    fn blocked_battlefield_entry_does_not_strand_card_face_down() {
        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(70101),
            PlayerId(0),
            "Manifest Source".to_string(),
            Zone::Battlefield,
        );

        // Grafdigger's Cage: "Creature cards in graveyards and libraries can't
        // enter the battlefield." Affected = creature cards in graveyard/library.
        let cage = create_object(
            &mut state,
            CardId(70102),
            PlayerId(0),
            "Grafdigger's Cage".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&cage).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::CantEnterBattlefieldFrom).affected(
                    TargetFilter::Typed(
                        TypedFilter::default()
                            .with_type(TypeFilter::Creature)
                            .properties(vec![FilterProp::InAnyZone {
                                zones: vec![Zone::Graveyard, Zone::Library],
                            }]),
                    ),
                ),
            );
        }

        // A creature card in the library — the manifest target the Cage blocks.
        let card = create_object(
            &mut state,
            CardId(70103),
            PlayerId(0),
            "Caged Creature".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&card).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
        }

        let mut events = Vec::new();
        let _ = move_object(
            &mut state,
            ZoneMoveRequest::effect(card, Zone::Battlefield, source)
                .face_down(FaceDownProfile::vanilla_2_2()),
            &mut events,
        );

        let obj = state.objects.get(&card).expect("blocked card still exists");
        assert_eq!(
            obj.zone,
            Zone::Library,
            "a CantEnterBattlefieldFrom static must keep the card in its origin zone"
        );
        assert!(
            !obj.face_down,
            "a blocked manifest must roll back the face-down preflight flag, not \
             strand the card face down (CR 614.1d)"
        );
        assert!(
            obj.back_face.is_none(),
            "the face-down profile must not be applied to a card whose entry was \
             rejected — the hidden card must be left unchanged"
        );
    }
}

/// pod-lab loop-3 Q5: verifies the `move_to_zone`/`deliver_replaced_zone_change`
/// incremental-flush carve-out through the FULL production pipeline
/// (`move_object`/`ZoneMoveRequest::effect`), not just `zones::move_to_zone` in
/// isolation — so `entered_battlefield`/`took_plain_zone_transfer`, computed in
/// this file, are genuinely exercised. Every assertion reads
/// `state.layers_dirty` directly (the dirty-lattice/flush-arm seam itself),
/// not just final board state, per this fix's own verification-matrix
/// requirement that a test must fail if the carve-out is reverted or
/// mis-scoped — board state alone is identical either way for a plain
/// creature entry, so it cannot prove which path was taken.
#[cfg(test)]
mod layers_incremental_flush_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ContinuousModification, FilterProp, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::LayersDirty;
    use crate::types::identifiers::CardId;
    use crate::types::statics::StaticMode;

    fn reset_clean(state: &mut GameState) {
        state.layers_dirty = LayersDirty::Clean;
    }

    /// Row 1 (verification matrix): the dominant real-game case — a plain
    /// creature resolving from the Stack — takes the cheap `EnteredObjects`
    /// path, not `Full`. This is the fix's entire perf payoff; if this
    /// assertion regresses to `Full`, the carve-out has been reverted or
    /// over-narrowed.
    #[test]
    fn stack_to_battlefield_plain_entry_marks_entered_not_full() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(80001),
            PlayerId(0),
            "Effect Source".to_string(),
            Zone::Battlefield,
        );
        let spell = create_object(
            &mut state,
            CardId(80002),
            PlayerId(0),
            "Vanilla Creature".to_string(),
            Zone::Stack,
        );
        reset_clean(&mut state);

        let mut events = Vec::new();
        let _ = move_object(
            &mut state,
            ZoneMoveRequest::effect(spell, Zone::Battlefield, source),
            &mut events,
        );

        assert_eq!(state.objects[&spell].zone, Zone::Battlefield);
        match &state.layers_dirty {
            LayersDirty::EnteredObjects(ids) => {
                assert!(
                    ids.contains(&spell),
                    "the entering object must be tracked in EnteredObjects"
                );
            }
            other => panic!(
                "a plain Stack-to-Battlefield entry must take the incremental \
                 EnteredObjects path, got {other:?}"
            ),
        }
    }

    /// Row 2: Hand->Battlefield (land plays, Elvish Piper, Sneak Attack, Show
    /// and Tell) must keep forcing `Full` UNCONDITIONALLY. `layers.rs`'s
    /// zone-reading classifier hardcodes `QuantityRef::HandSize` to `false`,
    /// so a live HandSize-gated static (Carnage Interpreter class) would go
    /// undetected by `static_dependency_before`/`after` alone — this
    /// unconditional exclusion is that class's only protection.
    #[test]
    fn hand_to_battlefield_still_marks_full_via_pipeline() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(80011),
            PlayerId(0),
            "Effect Source".to_string(),
            Zone::Battlefield,
        );
        let land = create_object(
            &mut state,
            CardId(80012),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        reset_clean(&mut state);

        let mut events = Vec::new();
        let _ = move_object(
            &mut state,
            ZoneMoveRequest::effect(land, Zone::Battlefield, source),
            &mut events,
        );

        assert_eq!(state.objects[&land].zone, Zone::Battlefield);
        assert!(
            matches!(state.layers_dirty, LayersDirty::Full),
            "a Hand-origin battlefield entry must still force a full \
             re-evaluation, got {:?}",
            state.layers_dirty
        );
    }

    /// Row 7b (round-3 review blocker): Exile->Battlefield (reanimation,
    /// flicker return, "you may cast this from exile") must ALSO keep forcing
    /// `Full` unconditionally, for the identical reason as Hand.
    /// `QuantityRef::CardsExiledBySource`/`ExiledCardPower`/`TrackedSetSize`/
    /// `FilteredTrackedSetSize`/`TrackedSetAggregate` (Unlicensed Hearse,
    /// Veteran Survivor, Sutured Ghoul class) are ALL hardcoded to `false` in
    /// the same classifier, and their count is live-filtered on
    /// `obj.zone == Zone::Exile` — it changes the instant a linked card
    /// leaves Exile, with no Axis-2 flush-time analog to catch it.
    #[test]
    fn exile_to_battlefield_still_marks_full_via_pipeline() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(80021),
            PlayerId(0),
            "Effect Source".to_string(),
            Zone::Battlefield,
        );
        let exiled = create_object(
            &mut state,
            CardId(80022),
            PlayerId(0),
            "Exiled Creature".to_string(),
            Zone::Exile,
        );
        reset_clean(&mut state);

        let mut events = Vec::new();
        let _ = move_object(
            &mut state,
            ZoneMoveRequest::effect(exiled, Zone::Battlefield, source),
            &mut events,
        );

        assert_eq!(state.objects[&exiled].zone, Zone::Battlefield);
        assert!(
            matches!(state.layers_dirty, LayersDirty::Full),
            "an Exile-origin battlefield entry must still force a full \
             re-evaluation, got {:?}",
            state.layers_dirty
        );
    }

    /// Row 3: a battlefield entry REJECTED by a `CantEnterBattlefieldFrom`
    /// static (Grafdigger's Cage class, CR 614.1d) must still mark `Full` via
    /// `zone_pipeline.rs`'s `entered_battlefield` gate — `move_to_zone` never
    /// reaches its own (now axis-gated) mark block for a rejected entry at
    /// all, so this file's redundant check is the ONLY thing marking
    /// anything for this case, exactly as before the carve-out existed.
    #[test]
    fn rejected_battlefield_entry_still_marks_full() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(80031),
            PlayerId(0),
            "Effect Source".to_string(),
            Zone::Battlefield,
        );
        let cage = create_object(
            &mut state,
            CardId(80032),
            PlayerId(0),
            "Grafdigger's Cage".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&cage).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::CantEnterBattlefieldFrom).affected(
                    TargetFilter::Typed(
                        TypedFilter::default()
                            .with_type(TypeFilter::Creature)
                            .properties(vec![FilterProp::InAnyZone {
                                zones: vec![Zone::Graveyard, Zone::Library],
                            }]),
                    ),
                ),
            );
        }
        let caged = create_object(
            &mut state,
            CardId(80033),
            PlayerId(0),
            "Caged Creature".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&caged).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
        }
        reset_clean(&mut state);

        let mut events = Vec::new();
        let _ = move_object(
            &mut state,
            ZoneMoveRequest::effect(caged, Zone::Battlefield, source),
            &mut events,
        );

        assert_eq!(
            state.objects[&caged].zone,
            Zone::Library,
            "a CantEnterBattlefieldFrom static must keep the card in its origin zone"
        );
        assert!(
            matches!(state.layers_dirty, LayersDirty::Full),
            "a rejected battlefield entry must still force a full \
             re-evaluation via entered_battlefield, got {:?}",
            state.layers_dirty
        );
    }

    /// F5 (CodeRabbit, PR #6777 round): the incremental `EnteredObjects` path
    /// (Row 1) is only safe when nothing on the board reads membership of the
    /// entering object's origin or destination zone. A Graveyard->Battlefield
    /// entry (reanimation) is neither the Hand nor Exile carve-out, so it
    /// would wrongly take the cheap path unless `static_dependency_before`/
    /// `after` itself catches it: a live static (Tarmogoyf class) whose
    /// `affected` filter reads `Zone::Graveyard` must still force `Full` when
    /// a card leaves that zone for the battlefield. Round 4: the watcher
    /// carries a real modification (it sources a live effect) and the fixture
    /// is primed by a real flush, so the before arm is proven through the
    /// INDEXED path — bucket membership asserted, no empty-index fallback.
    #[test]
    fn battlefield_entry_with_static_dependency_marks_full_via_pipeline() {
        let mut state = GameState::new_two_player(42);
        let watcher = create_object(
            &mut state,
            CardId(80041),
            PlayerId(0),
            "Graveyard Watcher".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&watcher).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            // Round-4 fix (maintainer, PR #6777): a real companion modification
            // so the watcher sources a LIVE continuous effect — active effects
            // are built by iterating `def.modifications`
            // (`active_continuous_effects_from_static_definitions`), so an
            // affected-filter-only def would source nothing.
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::Continuous)
                    .affected(TargetFilter::Typed(
                        TypedFilter::default()
                            .with_type(TypeFilter::Creature)
                            .properties(vec![FilterProp::InAnyZone {
                                zones: vec![Zone::Graveyard],
                            }]),
                    ))
                    .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
            );
        }
        let source = create_object(
            &mut state,
            CardId(80042),
            PlayerId(0),
            "Effect Source".to_string(),
            Zone::Battlefield,
        );
        let reanimated = create_object(
            &mut state,
            CardId(80043),
            PlayerId(0),
            "Graveyard Creature".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&reanimated).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
        }
        // Round-4 fix (maintainer, PR #6777): prime with a real flush so the
        // live watcher is INDEXED, then prove the before arm fires through the
        // indexed path (buckets non-empty — no empty-index fallback involved).
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);
        assert_eq!(
            state.static_source_index.battlefield_sources.len(),
            1,
            "fixture premise: the live graveyard-reading watcher is indexed after the priming flush"
        );
        assert!(
            crate::game::layers::static_layer_dependency_for_zone_transition(
                &state,
                reanimated,
                Zone::Graveyard,
                Zone::Battlefield
            ),
            "the indexed watcher must make the pre-transition dependency check true"
        );

        let mut events = Vec::new();
        let _ = move_object(
            &mut state,
            ZoneMoveRequest::effect(reanimated, Zone::Battlefield, source),
            &mut events,
        );

        assert_eq!(state.objects[&reanimated].zone, Zone::Battlefield);
        assert!(
            matches!(state.layers_dirty, LayersDirty::Full),
            "a Graveyard-origin battlefield entry with a live zone-membership-dependent static must force a full re-evaluation via static_dependency_before/after, got {:?}",
            state.layers_dirty
        );
    }

    /// F5 discrimination companion (maintainer, PR #6777 round 2): the sibling
    /// test's watcher static is live on the battlefield BEFORE the transition,
    /// and `move_object` ORs `static_dependency_before || static_dependency_after`
    /// — so that test alone cannot catch the post-entry arm being dropped.
    /// Here the zone-reading static rides ON the entering object itself: while
    /// it sits in the Graveyard it is not a static-effect source (only
    /// battlefield/command objects generate continuous effects), so the
    /// before-check is false, and only the post-entry re-check
    /// (`static_dependency_after`) can see the now-live static and force
    /// `Full`. Removing the after arm turns this mark into the cheap
    /// `EnteredObjects` path and fails this test.
    #[test]
    fn battlefield_entry_whose_own_zone_reading_static_marks_full_post_entry() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(80044),
            PlayerId(0),
            "Effect Source".to_string(),
            Zone::Battlefield,
        );
        let entering_watcher = create_object(
            &mut state,
            CardId(80045),
            PlayerId(0),
            "Entering Graveyard Watcher".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&entering_watcher).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
            // Round-4 fix (maintainer, PR #6777): a real companion modification
            // so the arriving watcher sources a LIVE continuous effect once on
            // the battlefield (active effects iterate `def.modifications`).
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::Continuous)
                    .affected(TargetFilter::Typed(
                        TypedFilter::default()
                            .with_type(TypeFilter::Creature)
                            .properties(vec![FilterProp::InAnyZone {
                                zones: vec![Zone::Graveyard],
                            }]),
                    ))
                    .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
            );
        }

        // Round-3 fix (maintainer, PR #6777): prime with a REAL layer flush so
        // the fixture carries exactly the index state production would — not a
        // hand-reset that leaves `static_source_index` unbuilt. For this board
        // shape the rebuild leaves the index PRECISELY empty:
        // `StaticSourceIndex::rebuild_from_state` keys on generators only
        // (static_source_index.rs), the lone battlefield object carries no
        // static, and the graveyard watcher sits outside both indexed buckets.
        // At the post-entry check the index is then stale-EMPTY, not
        // legitimately empty: the watcher has arrived and IS a generator, but
        // nothing rebuilds the index mid-move, so `use_fallback` in
        // `for_each_static_effect_source` fires and the direct scan sees the
        // newcomer. That is exactly the production shape of "the first
        // generator enters a generator-free board" — reached in any real game
        // — so the `Full` mark pinned here is live behavior, not a recovery
        // path for hand-built state. The populated-bucket shape, where the
        // after arm provably cannot see the newcomer, is pinned by
        // `populated_index_entry_defers_zone_reading_static_to_flush_escalation`.
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);
        assert!(
            matches!(state.layers_dirty, LayersDirty::Clean),
            "priming flush must leave the dirty lattice clean, got {:?}",
            state.layers_dirty
        );
        assert!(
            state.static_source_index.battlefield_sources.is_empty()
                && state.static_source_index.command_sources.is_empty(),
            "fixture premise: a generator-free board rebuilds to a precisely empty index"
        );

        // Precondition for discrimination: with the watcher still in the
        // Graveyard, no battlefield/command object reads zone membership, so
        // the pre-transition check must come up empty and the post-entry arm
        // is the only guard under test.
        assert!(
            !crate::game::layers::static_layer_dependency_for_zone_transition(
                &state,
                entering_watcher,
                Zone::Graveyard,
                Zone::Battlefield
            ),
            "fixture invalid: a pre-transition static dependency would let the before arm mask the after arm"
        );

        let mut events = Vec::new();
        let _ = move_object(
            &mut state,
            ZoneMoveRequest::effect(entering_watcher, Zone::Battlefield, source),
            &mut events,
        );

        assert_eq!(state.objects[&entering_watcher].zone, Zone::Battlefield);
        assert!(
            matches!(state.layers_dirty, LayersDirty::Full),
            "an entering object that itself carries a zone-membership-reading static must force a full re-evaluation via static_dependency_after (the before check is provably false here), got {:?}",
            state.layers_dirty
        );
    }

    /// Round-3 companion (maintainer, PR #6777): the POPULATED-index shape.
    /// With an unrelated generator on the battlefield the indexed buckets are
    /// non-empty, and the mid-mutation dependency checks read a stale-by-design
    /// index (rebuilt only at the top of a flush pass — see the "Authority"
    /// note in static_source_index.rs): the just-entered watcher is not yet a
    /// bucket member, so BOTH the before and after arms are false and
    /// `move_object` proposes the cheap `EnteredObjects` mark. Safety for this
    /// shape is delivered at flush time, not at the mutation site:
    /// `prepare_incremental_flush` escalates to a full pass (arm (1) of
    /// `entered_object_blocks_incremental` fires first on the entering
    /// object's live continuous effect; the recipient-sourced active-effect
    /// check just after the index rebuild is a redundant backstop). This
    /// test pins that handoff end-to-end — cheap mark at the seam, escalation
    /// plus full evaluation at the flush — so neither half of the contract can
    /// silently regress.
    ///
    /// #10400 note: the per-object zone-of-function term in
    /// `static_layer_dependency_for_zone_transition` reads `state.objects`
    /// directly, so it too bypasses the stale-by-design index for the MOVED
    /// SOURCE's own statics. This test's assertions are unchanged because its
    /// fixture's definitions carry empty `active_zones`, which the term
    /// deliberately does not match.
    #[test]
    fn populated_index_entry_defers_zone_reading_static_to_flush_escalation() {
        let mut state = GameState::new_two_player(42);
        let generator = create_object(
            &mut state,
            CardId(80046),
            PlayerId(0),
            "Benign Generator".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&generator).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
            // Plain typed filter: no InZone/InAnyZone prop, so per
            // `target_filter_reads_zone` it reads membership of NEITHER
            // transition zone — it exists only to populate the index bucket.
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::Continuous)
                    .affected(TargetFilter::Typed(
                        TypedFilter::default().with_type(TypeFilter::Creature),
                    ))
                    .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
            );
        }
        let source = create_object(
            &mut state,
            CardId(80047),
            PlayerId(0),
            "Effect Source".to_string(),
            Zone::Battlefield,
        );
        let entering_watcher = create_object(
            &mut state,
            CardId(80048),
            PlayerId(0),
            "Entering Graveyard Watcher".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&entering_watcher).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
            // A real modification (not just a zone-reading affected filter) so
            // the entered object sources a live continuous effect once on the
            // battlefield — the exact condition `entered_object_blocks_incremental`
            // arm (1) escalates on.
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::Continuous)
                    .affected(TargetFilter::Typed(
                        TypedFilter::default()
                            .with_type(TypeFilter::Creature)
                            .properties(vec![FilterProp::InAnyZone {
                                zones: vec![Zone::Graveyard],
                            }]),
                    ))
                    .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
            );
        }

        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);
        assert_eq!(
            state.static_source_index.battlefield_sources.len(),
            1,
            "fixture premise: exactly the benign generator is indexed after the priming flush"
        );
        assert!(
            !crate::game::layers::static_layer_dependency_for_zone_transition(
                &state,
                entering_watcher,
                Zone::Graveyard,
                Zone::Battlefield
            ),
            "fixture invalid: the generator must not read either transition zone"
        );

        let mut events = Vec::new();
        let _ = move_object(
            &mut state,
            ZoneMoveRequest::effect(entering_watcher, Zone::Battlefield, source),
            &mut events,
        );

        assert_eq!(state.objects[&entering_watcher].zone, Zone::Battlefield);
        assert!(
            matches!(state.layers_dirty, LayersDirty::EnteredObjects(_)),
            "with populated (stale-by-design) buckets the mutation-site arms cannot see the entering object's own static; the cheap mark is the designed outcome here, got {:?}",
            state.layers_dirty
        );

        crate::game::perf_counters::reset();
        crate::game::layers::flush_layers(&mut state);
        let counters = crate::game::perf_counters::snapshot();
        assert_eq!(
            counters.layers_escalated, 1,
            "flush must escalate: the entered object sources a live continuous effect"
        );
        assert_eq!(
            counters.layers_full_eval, 1,
            "the escalation must land in a full evaluation"
        );
    }
}

/// CR 712.14a building-block tests for the effect-driven transformed battlefield
/// entry (Esper Origins class). Both drive `execute_zone_move_with_terminal`
/// directly (not the raw `zones::move_to_zone`), so the full
/// `deliver_replaced_zone_change` plain-fallback wiring — including the
/// `move_to_zone_with_entry_flags(..., enter_transformed)` thread — is exercised.
#[cfg(test)]
mod effect_driven_transformed_entry_tests {
    use super::*;
    use crate::game::game_object::BackFaceData;
    use crate::game::layers::{evaluate_layers, flush_layers, mark_layers_full};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ContinuousModification, ControllerRef, FilterProp, StaticCondition, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::game_state::LayersDirty;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;

    /// CR 712.14a (2nd sentence) regression guard: a SINGLE-FACED object
    /// instructed to enter transformed cannot enter the battlefield — it remains
    /// in its origin zone (Exile).
    ///
    /// This guards the PRE-EXISTING belt-and-suspenders early-return inside
    /// `execute_zone_move_with_applied_terminal` (before the `zones.rs` SF1 guard
    /// is reached). It passes both before and after the fix; its role is to pin
    /// the CR 712.14a-2nd-sentence path against the SF1 guard rewrite ever being
    /// regressed to a front-face fallback on the full-pipeline route.
    #[test]
    fn single_faced_object_instructed_enter_transformed_remains() {
        let mut state = GameState::new_two_player(42);
        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Single Faced".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&object_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // back_face intentionally left None (single-faced).
        }

        let mut events = Vec::new();
        let result = execute_zone_move_with_terminal(
            &mut state,
            object_id,
            Zone::Exile,
            Zone::Battlefield,
            object_id,
            None,
            true, // enter_transformed
            EtbTapState::Unspecified,
            false,
            None,
            &[],
            None,
            false,
            None,
            None,
            &mut events,
        );

        assert!(
            matches!(
                result,
                ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained)
            ),
            "CR 712.14a 2nd sentence: a single-faced object instructed to enter \
             transformed must remain"
        );
        assert_eq!(
            state.objects[&object_id].zone,
            Zone::Exile,
            "the single-faced object must stay in Exile"
        );
    }

    /// CR 712.14a CONTROL: a DFC with a PERMANENT front face (Creature) and a
    /// PERMANENT back face (Land) instructed to enter transformed still lands in
    /// Battlefield and ends up transformed. Passes before and after the fix — the
    /// entry-face rewrite must not be front-regressive, and `enter_back_face_up`
    /// must still fire after the guarded entry.
    #[test]
    fn mdfc_permanent_front_transformed_entry_still_lands() {
        let mut state = GameState::new_two_player(42);
        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "MDFC Front".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&object_id).unwrap();
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
            obj.base_card_types = obj.card_types.clone();
            obj.back_face = Some(BackFaceData {
                is_swap_snapshot: false,
                trigger_printed_origins: Vec::new(),
                name: "MDFC Back".to_string(),
                power: None,
                toughness: None,
                loyalty: None,
                printed_loyalty: None,
                defense: None,
                card_types: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Land],
                    subtypes: vec![],
                },
                mana_cost: crate::types::mana::ManaCost::default(),
                keywords: vec![],
                abilities: vec![],
                trigger_definitions: Default::default(),
                replacement_definitions: Default::default(),
                static_definitions: Default::default(),
                color: vec![],
                printed_ref: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                layout_kind: None,
                parse_warnings: vec![],
            });
        }

        let mut events = Vec::new();
        let result = execute_zone_move_with_terminal(
            &mut state,
            object_id,
            Zone::Exile,
            Zone::Battlefield,
            object_id,
            None,
            true, // enter_transformed
            EtbTapState::Unspecified,
            false,
            None,
            &[],
            None,
            false,
            None,
            None,
            &mut events,
        );

        assert!(
            matches!(result, ZoneMoveTerminalResult::Completed(_)),
            "the DFC entered the battlefield and completed the move"
        );
        let obj = state.objects.get(&object_id).unwrap();
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "a permanent-front DFC entering transformed must land on the battlefield"
        );
        assert!(
            obj.transformed,
            "CR 712.14a: the DFC must be transformed (back face) after this entry"
        );
    }

    /// CR 113.6b + CR 400.7 + CR 611.3a: reanimating a source whose static
    /// functions from the graveyard stops that static, so recipients' cached
    /// characteristics must be re-derived through the production pipeline.
    ///
    /// Reach guard, not a revert probe: with the printed Anger parse the
    /// `IsPresent(Mountain you control)` filter carries
    /// `FilterProp::InZone { zone: Zone::Battlefield }`, which
    /// `static_condition_reads_zone_membership` reads, so for a move whose
    /// destination is the battlefield the pre-existing board-wide term already
    /// proposes `Full` at base. What this row guards is the reach end: the
    /// recipient's applied Haste must lapse after the flush. The revert probes
    /// for the new per-object term are the mill (V2/V7/V8), exile (V5/V10) and
    /// library-placement (V11/V11b/V12) rows, whose source zones the condition
    /// does not read.
    #[test]
    fn reanimated_incarnation_marks_full_and_drops_graveyard_grant() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(80071),
            PlayerId(0),
            "Effect Source".to_string(),
            Zone::Battlefield,
        );
        let mountain = create_object(
            &mut state,
            CardId(80072),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&mountain).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Mountain".to_string());
            obj.base_card_types = obj.card_types.clone();
        }
        let bear = create_object(
            &mut state,
            CardId(80073),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
        let anger = create_object(
            &mut state,
            CardId(80074),
            PlayerId(0),
            "Anger".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&anger).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
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
                                    properties: vec![FilterProp::InZone {
                                        zone: Zone::Battlefield,
                                    }],
                                })),
                            },
                        ],
                    })
                    .active_zones(vec![Zone::Graveyard]),
            );
            assert_eq!(
                obj.static_definitions.iter_all().count(),
                1,
                "fixture premise: the milled source carries exactly the Anger static"
            );
        }

        mark_layers_full(&mut state);
        evaluate_layers(&mut state);
        assert!(
            state.objects[&bear].has_keyword(&Keyword::Haste),
            "precondition: Anger in the graveyard with a Mountain grants Haste"
        );
        assert!(!state.layers_dirty.is_dirty());

        let mut events = Vec::new();
        let _ = move_object(
            &mut state,
            ZoneMoveRequest::effect(anger, Zone::Battlefield, source),
            &mut events,
        );

        assert_eq!(state.objects[&anger].zone, Zone::Battlefield);
        assert!(
            matches!(state.layers_dirty, LayersDirty::Full),
            "reanimating a graveyard-functioning static's source must force a full \
             re-evaluation via the per-object zone-of-function term, got {:?}",
            state.layers_dirty
        );
        flush_layers(&mut state);
        assert!(
            !state.objects[&bear].has_keyword(&Keyword::Haste),
            "once Anger leaves the graveyard its grant must lapse for pre-existing recipients"
        );
    }
}

#[cfg(test)]
mod face_down_entry_referent_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::FaceDownProfile;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    /// CR 608.2c: the shared CR 708.3 helper installs characteristics and
    /// NOTHING else. It is reached by every face-down path, including the
    /// face-down CAST in `casting.rs` (where the object is on the stack and has
    /// produced no permanent to name) and that module's two cast SIMULATIONS,
    /// so a publish here would be a write no instruction asked for.
    ///
    /// The referent is published at the delivery instead, from the intent the
    /// REQUEST carried — see `ChainReferentIntent`. What this row nails down is
    /// the negative: no caller of this helper can publish by reaching it.
    ///
    /// It does NOT prove the positive. That is the integration suite's job
    /// (`manifest_dread_that_creature_anaphor`), which drives the synchronous
    /// manifest, the two-card continuation and the accept/decline resume through
    /// the production pipeline.
    #[test]
    fn the_shared_face_down_helper_publishes_no_referent_from_any_zone() {
        for zone in [Zone::Battlefield, Zone::Stack] {
            let mut state = GameState::new_two_player(7);
            let player = PlayerId(0);
            let id = create_object(&mut state, CardId(1), player, "Entrant".to_string(), zone);
            let before = vec![ObjectId(999)];
            state.last_created_token_ids = before.clone();

            apply_face_down_entry_profile(&mut state, id, &FaceDownProfile::vanilla_2_2());

            assert_eq!(
                state.last_created_token_ids, before,
                "the characteristics helper must not touch the referent slot (zone {zone:?})"
            );
        }
    }
}

#[cfg(test)]
mod exiling_player_record_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
    use crate::types::identifiers::CardId;

    /// A card already in exile that P1 exiled.
    fn card_p1_exiled(state: &mut GameState) -> ObjectId {
        let card = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Exiled Card".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&card).unwrap().exiled_by = Some(PlayerId(1));
        card
    }

    /// CR 400.8 + CR 406.6: exiling an object that is already in exile makes a
    /// new object that has just been exiled; the delivery records the player
    /// who performed this exile, replacing the old record.
    #[test]
    fn re_exiling_an_exiled_card_records_the_new_performer() {
        let mut state = GameState::new_two_player(42);
        let card = card_p1_exiled(&mut state);
        let mut events = Vec::new();

        move_object(
            &mut state,
            ZoneMoveRequest::effect(card, Zone::Exile, ObjectId(100)).performed_by(PlayerId(0)),
            &mut events,
        );

        assert_eq!(state.objects[&card].zone, Zone::Exile);
        assert_eq!(state.objects[&card].exiled_by, Some(PlayerId(0)));
    }

    /// CR 400.7 + CR 400.8: a re-exile no player performs leaves the new object
    /// with no exiling player — never the previous object's record.
    #[test]
    fn re_exile_without_performer_clears_the_old_record() {
        let mut state = GameState::new_two_player(42);
        let card = card_p1_exiled(&mut state);
        let mut events = Vec::new();

        move_object(
            &mut state,
            ZoneMoveRequest::effect(card, Zone::Exile, ObjectId(100)),
            &mut events,
        );

        assert_eq!(state.objects[&card].zone, Zone::Exile);
        assert_eq!(state.objects[&card].exiled_by, None);
    }

    /// CR 608.2c + CR 400.8 + CR 406.6: the shared `ChangeZone` resolver
    /// delivers through the pipeline with its controller as the performer, so
    /// an exile-to-exile `ChangeZone` records the controller.
    #[test]
    fn change_zone_exile_to_exile_records_its_controller() {
        let mut state = GameState::new_two_player(42);
        let card = card_p1_exiled(&mut state);
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Exile),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(card)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        crate::game::effects::change_zone::resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&card].zone, Zone::Exile);
        assert_eq!(state.objects[&card].exiled_by, Some(PlayerId(0)));
    }
}

#[cfg(test)]
mod hand_entry_receiver_tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ReplacementDefinition, ReplacementMode, TargetFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::CardId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::resolved_commands::ResolvedRulesCommand;

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    fn dandan() -> GameState {
        GameState::new(FormatConfig::dandan(), 2, 42)
    }

    fn card_in(state: &mut GameState, id: u64, owner: PlayerId, zone: Zone) -> ObjectId {
        create_object(state, CardId(id), owner, format!("Card {id}"), zone)
    }

    fn take(state: &mut GameState, card: ObjectId, taker: PlayerId) {
        let request = ZoneMoveRequest::effect(card, Zone::Hand, ObjectId(100)).performed_by(taker);
        move_object(state, request, &mut Vec::new());
    }

    fn assert_in_hand_of(state: &GameState, card: ObjectId, seat: PlayerId) {
        assert!(
            state.players[seat.0 as usize].hand.contains(&card),
            "{card:?}"
        );
        assert!(state.players[1 - seat.0 as usize].hand.is_empty());
        assert_eq!(state.objects[&card].owner, seat);
        assert_eq!(state.objects[&card].controller, seat);
    }

    fn rebound_owners(state: &GameState) -> Vec<Option<PlayerId>> {
        state
            .resolved_rules_journal
            .entries()
            .iter()
            .filter_map(|entry| match entry.command.as_ref()? {
                ResolvedRulesCommand::ZoneChange(command) => Some(command.rebound_from),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_format_without_shared_zones_does_not_rebind() {
        let mut standard = GameState::new_two_player(42);
        let card = card_in(&mut standard, 1, P0, Zone::Library);
        take(&mut standard, card, P1);
        assert_in_hand_of(&standard, card, P0);

        let mut shared = dandan();
        let card = card_in(&mut shared, 1, P0, Zone::Library);
        take(&mut shared, card, P1);
        assert_in_hand_of(&shared, card, P1);
        assert_eq!(rebound_owners(&shared), vec![Some(P0)]);
    }

    #[test]
    fn a_card_leaving_a_per_seat_zone_keeps_its_owner() {
        let mut state = dandan();
        let creature = card_in(&mut state, 1, P1, Zone::Battlefield);
        state.objects.get_mut(&creature).unwrap().controller = P0;

        take(&mut state, creature, P0);

        assert_in_hand_of(&state, creature, P1);
        assert_eq!(rebound_owners(&state), vec![None]);
    }

    #[test]
    fn a_taker_who_already_owns_the_card_journals_no_rebind() {
        let mut state = dandan();
        let card = card_in(&mut state, 1, P0, Zone::Library);

        take(&mut state, card, P0);

        assert_in_hand_of(&state, card, P0);
        assert_eq!(rebound_owners(&state), vec![None]);
    }

    #[test]
    fn only_hand_from_a_shared_zone_casts_and_sourceless_battlefield_entries_rebind() {
        let mut state = dandan();
        let pile = card_in(&mut state, 1, P0, Zone::Library);
        let exiled = card_in(&mut state, 2, P0, Zone::Exile);
        let effect_source = Some(ObjectId(100));

        assert_eq!(
            entry_receiver(&state, pile, Zone::Hand, Some(P1), None),
            Some(P1)
        );
        assert_eq!(
            entry_receiver(&state, exiled, Zone::Stack, Some(P1), Some(exiled)),
            Some(P1)
        );
        assert_eq!(
            entry_receiver(&state, pile, Zone::Battlefield, Some(P1), None),
            Some(P1)
        );

        for to in [Zone::Library, Zone::Graveyard, Zone::Exile, Zone::Command] {
            assert_eq!(
                entry_receiver(&state, pile, to, Some(P1), None),
                None,
                "{to:?}"
            );
        }
        assert_eq!(
            entry_receiver(&state, exiled, Zone::Hand, Some(P1), None),
            None,
            "a Hand entry out of a per-seat zone"
        );
        assert_eq!(
            entry_receiver(&state, pile, Zone::Battlefield, Some(P1), effect_source),
            None,
            "an effect-placed permanent"
        );
        assert_eq!(entry_receiver(&state, pile, Zone::Stack, None, None), None);
        assert_eq!(
            entry_receiver(&state, pile, Zone::Stack, Some(P0), None),
            None
        );

        let mut standard = GameState::new_two_player(42);
        let theirs = card_in(&mut standard, 1, P0, Zone::Exile);
        assert_eq!(
            entry_receiver(&standard, theirs, Zone::Stack, Some(P1), Some(theirs)),
            None
        );
    }

    #[test]
    fn a_card_redirected_to_command_keeps_its_owner() {
        let mut state = dandan();
        let rebound = card_in(&mut state, 1, P0, Zone::Library);
        let attraction = card_in(&mut state, 2, P0, Zone::Library);
        state
            .objects
            .get_mut(&attraction)
            .unwrap()
            .card_types
            .subtypes
            .push("Attraction".to_string());

        take(&mut state, rebound, P1);
        take(&mut state, attraction, P1);

        assert_eq!(
            state.objects[&rebound].owner, P1,
            "reach: the control rebinds"
        );
        assert_eq!(state.objects[&attraction].zone, Zone::Command);
        assert_eq!(state.objects[&attraction].owner, P0);
        assert_eq!(rebound_owners(&state), vec![Some(P0)]);
    }

    /// Two P0-owned pile cards requested into P1's hand, each under an optional
    /// Hand-to-Exile redirect; returns the state parked on the first member.
    fn paused_batch() -> (GameState, ObjectId, ObjectId) {
        let mut state = dandan();
        let first = card_in(&mut state, 1, P0, Zone::Library);
        let second = card_in(&mut state, 2, P0, Zone::Library);
        let source = card_in(&mut state, 90001, P0, Zone::Battlefield);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Exile,
                            target: TargetFilter::Any,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    ))
                    .destination_zone(Zone::Hand),
            );
        let requests = vec![
            ZoneMoveRequest::effect(first, Zone::Hand, first).performed_by(P1),
            ZoneMoveRequest::effect(second, Zone::Hand, second).performed_by(P1),
        ];
        assert!(matches!(
            move_objects_simultaneously(&mut state, requests, &mut Vec::new()),
            BatchMoveResult::NeedsChoice
        ));
        (state, first, second)
    }

    fn choose(state: &mut GameState, index: usize) {
        apply_as_current(state, GameAction::ChooseReplacement { index })
            .expect("replacement choice accepted");
    }

    #[test]
    fn the_taker_survives_a_serialized_pause_and_a_redirect_clears_the_rebind() {
        let (state, first, second) = paused_batch();
        let mut state: GameState =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();

        choose(&mut state, 0);
        choose(&mut state, 1);

        assert_eq!(state.objects[&first].zone, Zone::Exile);
        assert_eq!(
            state.objects[&first].owner, P0,
            "the redirect left the pile owner alone"
        );
        assert_eq!(
            state.objects[&first].exiled_by,
            Some(P1),
            "a redirected Hand request keeps the taker as the exiling player"
        );
        assert_in_hand_of(&state, second, P1);
    }

    #[test]
    fn declining_the_redirect_delivers_both_members_to_the_taker() {
        let (mut state, first, second) = paused_batch();

        choose(&mut state, 1);
        choose(&mut state, 1);

        assert!(state.players[1].hand.contains(&first));
        assert!(state.players[1].hand.contains(&second));
        assert_eq!(rebound_owners(&state), vec![Some(P0), Some(P0)]);
    }
}

#[cfg(test)]
mod staged_batch_tests {
    use super::*;
    use crate::game::effects::change_zone::{
        fold_physical_order_to_departures, open_owner_library_arrangement,
        submit_owner_library_order, submitted_owner_arrangements, OwnerArrangementInstruction,
        OwnerLibraryOrderProgress,
    };
    use crate::game::triggers::{
        collect_delayed_triggers_into_deferred, collect_triggers_into_deferred,
        filter_consumed_trigger_events, TriggerCollectionRequester,
    };
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, DelayedTriggerCondition, DigRestOrder, Effect, EffectKind,
        LibraryPosition, QuantityExpr, ResolvedAbility, TargetFilter, TriggerDefinition,
    };
    use crate::types::game_state::{
        BatchCompletion, ChangeZoneBatchTail, ChangeZoneCountPolicy, DelayedTrigger,
        DigDeliveryStage, StagedBatchMemberState, StagedDestroyOperation, StagedMemberOperation,
        ZoneMoveCompletion,
    };
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::resolution::ResolutionStateWire;
    use crate::types::triggers::TriggerMode;

    #[test]
    fn staged_change_zone_keeps_delayed_leaves_trigger() {
        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let source = effect_source(&mut state);
        let members: Vec<_> = [1, 2]
            .into_iter()
            .map(|card| {
                create_object(
                    &mut state,
                    CardId(card),
                    controller,
                    format!("Departing member {card}"),
                    Zone::Battlefield,
                )
            })
            .collect();
        let gain = Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        };
        let observer = TriggerDefinition::new(TriggerMode::LeavesBattlefield)
            .valid_card(TargetFilter::Any)
            .execute(AbilityDefinition::new(AbilityKind::Spell, gain.clone()));
        let object = state.objects.get_mut(&source).unwrap();
        std::sync::Arc::make_mut(&mut object.base_trigger_definitions).push(observer);
        object.materialize_base_trigger_definitions();
        state.delayed_triggers.push(DelayedTrigger::new(
            DelayedTriggerCondition::WhenLeavesPlay {
                object_id: members[0],
            },
            Box::new(ResolvedAbility::new(gain, vec![], source, controller)),
            controller,
            source,
            true,
        ));

        let group = crate::game::triggers::allocate_logical_zone_change_group(&mut state, &members);
        let tail = ChangeZoneBatchTail {
            group_id: group.logical_group_id,
            effect_kind: EffectKind::ChangeZone,
            source_id: source,
            controller,
            destination: Zone::Exile,
            count: ChangeZoneCountPolicy::MassAllMembers,
            pending_return_result_producer: None,
            own_exile_links: crate::types::game_state::OwnExileLinkSettlement::Retain,
        };
        let requests = members
            .iter()
            .map(|&id| ZoneMoveRequest::effect(id, Zone::Exile, source))
            .collect();
        let mut events = Vec::new();
        assert!(matches!(
            deliver_staged_change_zone_batch(&mut state, requests, group, tail, &mut events),
            StagedChangeZoneOutcome::Settled { .. }
        ));
        assert!(members
            .iter()
            .all(|id| state.objects[id].zone == Zone::Exile));
        // CR 603.2c: the ordinary observer fires once for each departure,
        // and the subsequent priority scan must not collect either one again.
        assert_eq!(state.deferred_triggers.len(), 2);
        let ordinary_events = filter_consumed_trigger_events(
            &events,
            TriggerCollectionRequester::Ordinary,
            &state.consumed_before_priority_trigger_events,
        );
        collect_triggers_into_deferred(&mut state, &ordinary_events);
        assert_eq!(state.deferred_triggers.len(), 2, "no ordinary recollection");
        // CR 603.7b: the one-shot delayed trigger still sees its next event.
        let delayed_events = filter_consumed_trigger_events(
            &events,
            TriggerCollectionRequester::Delayed,
            &state.consumed_before_priority_trigger_events,
        );
        collect_delayed_triggers_into_deferred(&mut state, &delayed_events);
        assert_eq!(state.deferred_triggers.len(), 3);
        assert!(state.delayed_triggers.is_empty());
        collect_delayed_triggers_into_deferred(&mut state, &delayed_events);
        assert_eq!(
            state.deferred_triggers.len(),
            3,
            "the delayed trigger fires once"
        );
    }

    fn effect_source(state: &mut GameState) -> ObjectId {
        create_object(
            state,
            CardId(90_001),
            PlayerId(0),
            "Staged source".to_string(),
            Zone::Battlefield,
        )
    }

    fn graveyard_card(state: &mut GameState, card: u64) -> ObjectId {
        create_object(
            state,
            CardId(card),
            PlayerId(0),
            format!("Staged card {card}"),
            Zone::Graveyard,
        )
    }

    fn library_card(state: &mut GameState, card: u64) -> ObjectId {
        let id = create_object(
            state,
            CardId(card),
            PlayerId(0),
            format!("Library card {card}"),
            Zone::Library,
        );
        if !state.players[0].library.contains(&id) {
            state.players[0].library.push_back(id);
        }
        id
    }

    fn staged_request(
        state: &GameState,
        id: ObjectId,
        destination: Zone,
        placement: Option<LibraryPosition>,
    ) -> PendingBatchZoneMoveRequest {
        let source = state
            .objects
            .values()
            .find(|object| object.name == "Staged source")
            .expect("fixture source")
            .id;
        let mut request = ZoneMoveRequest::effect(id, destination, source);
        if let Some(placement) = placement {
            request = request.at_library_position(placement);
        }
        request.into_pending()
    }

    fn approved_library_event(
        state: &GameState,
        id: ObjectId,
        position: LibraryPosition,
    ) -> ProposedEvent {
        let from = state.objects[&id].zone;
        let source = state
            .objects
            .values()
            .find(|object| object.name == "Staged source")
            .expect("fixture source")
            .id;
        let mut event = ProposedEvent::zone_change(id, from, Zone::Library, Some(source));
        if let ProposedEvent::ZoneChange {
            library_placement, ..
        } = &mut event
        {
            *library_placement = Some(position);
        }
        event
    }

    fn staged_member(
        state: &GameState,
        id: ObjectId,
        approved: Option<ProposedEvent>,
        member_state: StagedBatchMemberState,
        request_destination: Zone,
        request_placement: Option<LibraryPosition>,
    ) -> StagedBatchMember {
        let object = &state.objects[&id];
        StagedBatchMember {
            announcement_index: 0,
            request: staged_request(state, id, request_destination, request_placement),
            identity: ObjectIncarnationRef::from_object(object),
            owner: object.owner,
            state: member_state,
            approved_event: approved,
            pending_outcome: None,
            terminal: None,
            operation: StagedMemberOperation::ZoneMove,
            approved_post_replacement: None,
        }
    }

    fn take_owner(
        state: &mut GameState,
        group_id: LogicalZoneChangeGroupId,
    ) -> PendingBatchDeliveries {
        let owner = state
            .resolution_stack
            .take_active_batch_delivery()
            .expect("staged frame")
            .expect("staged owner");
        assert_eq!(owner.logical_zone_change_group.logical_group_id, group_id);
        owner
    }

    fn put_owner(state: &mut GameState, owner: PendingBatchDeliveries) {
        state
            .resolution_stack
            .park_batch_owner(owner)
            .expect("staged owner re-parks");
    }

    fn push_approved_staged(
        state: &mut GameState,
        members: Vec<StagedBatchMember>,
        stage: BatchDeliveryStage,
    ) -> LogicalZoneChangeGroupId {
        let announced: Vec<ObjectId> = members
            .iter()
            .map(|member| member.identity.object_id)
            .collect();
        let group = crate::game::triggers::allocate_logical_zone_change_group(state, &announced);
        let group_id = group.logical_group_id;
        let attempted = announced.clone();
        let mut members = members;
        for (index, member) in members.iter_mut().enumerate() {
            member.announcement_index = index as u32;
        }
        let requests: Vec<PendingBatchZoneMoveRequest> = members
            .iter()
            .filter(|member| member.state == StagedBatchMemberState::Unconsulted)
            .map(|member| member.request.clone())
            .collect();
        let remaining: Vec<ObjectId> = requests.iter().map(|request| request.object_id).collect();
        let cursor = if stage == BatchDeliveryStage::Approving {
            0
        } else {
            members.len()
        };
        let staged = StagedBatchDelivery {
            stage,
            cursor,
            members,
            arrangements: Vec::new(),
            arrangement_cursor: 0,
            population_devour_snapshot: None,
        };
        state.push_batch_delivery(PendingBatchDeliveries {
            component_delivery_parent: None,
            logical_zone_change_group: group,
            paused_current: None,
            remaining,
            destination: Zone::Library,
            source_id: None,
            enter_tapped: EtbTapState::Unspecified,
            exile_tracking: ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: None,
            replacement_applied: HashSet::new(),
            requests,
            attempted,
            zone_change_record_start: state.zone_changes_this_turn.len(),
            deferred_events: Vec::new(),
            staged: Some(staged),
        });
        group_id
    }

    /// CR 800.4a: surviving batch deliveries settle from their own
    /// event slice, never the unrelated departure events already in the caller.
    #[test]
    fn staged_departure_reconciliation_settles_survivor_events_without_departure_prefix() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let survivor = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Survivor".into(),
            Zone::Battlefield,
        );
        let departed = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Departed".into(),
            Zone::Battlefield,
        );
        let group_id =
            replacement_origin_group(&mut state, &[survivor, departed], LibraryPosition::Bottom);
        let identity = ObjectIncarnationRef::from_object(&state.objects[&departed]);
        let owner = state.active_batch_delivery_mut().unwrap();
        owner
            .logical_zone_change_group
            .record_abandoned_by_player_left(identity)
            .unwrap();
        prune_staged_member_for_player_left(owner, identity);
        let staged = owner.staged.as_mut().unwrap();
        staged.cursor = staged.members.len();
        assert_eq!(staged.members[1].state, StagedBatchMemberState::Abandoned);
        let mut events = Vec::new();
        move_object(
            &mut state,
            ZoneMoveRequest::player_left_game(departed, Zone::Exile),
            &mut events,
        );
        let departure_prefix = events.clone();
        let result = reconcile_staged_batch_after_departure(&mut state, &mut events).unwrap();
        assert!(matches!(
            result,
            crate::game::engine_resolution_choices::DepartureSettlement::Settled
        ));
        assert_eq!(&events[..departure_prefix.len()], departure_prefix);
        let moves: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged { object_id, to, .. } => Some((*object_id, *to)),
                _ => None,
            })
            .collect();
        assert_eq!(
            moves,
            vec![(departed, Zone::Exile), (survivor, Zone::Library)]
        );
        assert_eq!(
            state
                .library_of(PlayerId(0))
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![survivor]
        );
        assert!(
            state.resolution_stack.is_empty(),
            "group {group_id:?} must collect and retire"
        );
    }

    /// CR 401.4 + CR 730.3a: preserving ordinary numbered insertion must not
    /// discard a merged population's owner-submitted physical rank authority.
    #[test]
    fn staged_numbered_owner_arrangement_preserves_component_interleaving() {
        let mut scenario = crate::game::scenario::GameScenario::new();
        let player = PlayerId(0);
        scenario.with_library_top(player, &["L0", "L1", "L2"]);
        let host = scenario.add_creature(player, "Host", 2, 2).id();
        let rider = scenario.add_creature_to_hand(player, "Rider", 3, 3).id();
        let ordinary = scenario.add_creature(player, "Ordinary", 2, 2).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let original: Vec<_> = state.library_of(player).iter().copied().collect();
        let source = effect_source(state);
        crate::game::merge::merge_object_onto(
            state,
            rider,
            host,
            crate::game::merge::MergeSide::Bottom,
            &mut Vec::new(),
        );
        let position = LibraryPosition::NthFromTop { n: 3 };
        open_owner_library_arrangement(
            state,
            vec![(player, vec![host, ordinary])],
            None,
            source,
            position.clone(),
            false,
            None,
        );
        let instruction = OwnerArrangementInstruction {
            source_id: source,
            library_position: &position,
            track_exiled_by_source: false,
            duration: None,
        };
        let OwnerLibraryOrderProgress::Complete(batches) =
            submit_owner_library_order(state, player, None, &instruction, &[rider, ordinary, host])
        else {
            panic!("one owner's submitted order completes the arrangement");
        };
        let departures = fold_physical_order_to_departures(&batches);
        let seeds = submitted_owner_arrangements(batches, &position);
        let requests = departures
            .into_iter()
            .map(|id| {
                ZoneMoveRequest::effect(id, Zone::Library, source)
                    .at_library_position(position.clone())
            })
            .collect();
        state.waiting_for = WaitingFor::Priority { player };
        assert!(matches!(
            move_owner_arranged_batch_then(state, requests, seeds, None, &mut Vec::new()),
            BatchMoveResult::Done
        ));
        assert_eq!(
            state.library_of(player).iter().copied().collect::<Vec<_>>(),
            vec![original[0], original[1], rider, ordinary, host, original[2]]
        );
        assert!(state.resolution_stack.is_empty());
    }

    fn seeded_component_fixture(
        stage: BatchDeliveryStage,
    ) -> (GameState, ObjectId, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = effect_source(&mut state);
        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Host".into(),
            Zone::Battlefield,
        );
        let rider = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Rider".into(),
            Zone::Battlefield,
        );
        let ordinary = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Ordinary".into(),
            Zone::Battlefield,
        );
        let reqs: Vec<_> = [host, ordinary]
            .into_iter()
            .map(|id| {
                ZoneMoveRequest::effect(id, Zone::Library, source)
                    .at_library_position(LibraryPosition::Bottom)
            })
            .collect();
        let group = crate::game::triggers::allocate_logical_zone_change_group(
            &mut state,
            &[host, ordinary],
        );
        let departure = ObjectIncarnationRef::from_object(&state.objects[&host]);
        let entries = [rider, ordinary, host]
            .into_iter()
            .map(|id| {
                let mut entry = MassLibraryOrderMember::card(
                    ObjectIncarnationRef::from_object(&state.objects[&id]),
                    Zone::Battlefield,
                );
                if id != ordinary {
                    entry.departing_permanent = Some(departure);
                }
                entry
            })
            .collect();
        let seed = StagedLibraryArrangement {
            position: LibraryPosition::Bottom,
            batch: MassLibraryOrderBatch {
                owner: PlayerId(0),
                arranger: None,
                members: entries,
            },
            status: StagedArrangementStatus::Submitted,
        };
        let mut staged = new_staged_batch(&state, &reqs, vec![seed]);
        staged.stage = stage;
        if stage != BatchDeliveryStage::Approving {
            for member in &mut staged.members {
                member.state = StagedBatchMemberState::Approved;
                member.approved_event = Some(approved_library_event(
                    &state,
                    member.identity.object_id,
                    LibraryPosition::Bottom,
                ));
            }
        }
        let mut owner = new_staged_owner(&state, &reqs, group, staged, None);
        sync_staged_requests(&mut owner);
        put_owner(&mut state, owner);
        (state, host, rider, ordinary)
    }

    fn inherited_component_fixture() -> (GameState, ObjectId, ObjectId, ObjectId) {
        let (mut state, host, rider, ordinary) =
            seeded_component_fixture(BatchDeliveryStage::Delivering);
        let mut parent = state
            .resolution_stack
            .take_active_batch_delivery()
            .unwrap()
            .unwrap();
        let member = &parent.staged.as_ref().unwrap().members[0];
        let event = member.approved_event.clone().unwrap();
        parent.paused_current = Some(PendingZoneChangeDelivery::new(
            member.identity,
            event.clone(),
        ));
        let link = PendingComponentDeliveryParent {
            parent_group_id: parent.logical_zone_change_group.logical_group_id,
            member: member.identity,
            expected_event: event.clone(),
            approved_event: event,
            inherited_arrangements: parent.staged.as_ref().unwrap().arrangements.clone(),
        };
        let reqs: Vec<_> = [host, rider]
            .into_iter()
            .map(|id| {
                ZoneMoveRequest::merged_component(id, Zone::Library)
                    .at_library_position(LibraryPosition::Bottom)
            })
            .collect();
        let group =
            crate::game::triggers::allocate_logical_zone_change_group(&mut state, &[host, rider]);
        let mut staged = new_staged_batch(&state, &reqs, link.inherited_arrangements.clone());
        staged.stage = BatchDeliveryStage::Delivering;
        for member in &mut staged.members {
            member.state = StagedBatchMemberState::Approved;
            member.approved_event = Some(approved_library_event(
                &state,
                member.identity.object_id,
                LibraryPosition::Bottom,
            ));
        }
        let mut child = new_staged_owner(&state, &reqs, group, staged, Some(link));
        sync_staged_requests(&mut child);
        put_owner(&mut state, parent);
        put_owner(&mut state, child);
        (state, host, rider, ordinary)
    }

    #[test]
    fn staged_validator_accepts_seeded_component_entries() {
        let (state, _, _, _) = seeded_component_fixture(BatchDeliveryStage::Arranging);
        state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .unwrap();
    }

    #[test]
    fn staged_validator_accepts_seeded_component_entries_while_approving() {
        let (state, host, _, _) = seeded_component_fixture(BatchDeliveryStage::Approving);
        let staged = state
            .active_batch_delivery()
            .unwrap()
            .staged
            .as_ref()
            .unwrap();
        let member = staged
            .members
            .iter()
            .find(|member| member.identity.object_id == host)
            .unwrap();
        assert_eq!(member.state, StagedBatchMemberState::Unconsulted);
        assert_eq!(member.approved_event, None);
        state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .unwrap();
    }

    #[test]
    fn staged_validator_rejects_seeded_component_entry_for_nonmember_departure_while_approving() {
        let (mut state, _, _, _) = seeded_component_fixture(BatchDeliveryStage::Approving);
        state
            .active_batch_delivery_mut()
            .unwrap()
            .staged
            .as_mut()
            .unwrap()
            .arrangements[0]
            .batch
            .members[0]
            .departing_permanent = Some(ObjectIncarnationRef::of(ObjectId(999), 0));
        assert!(state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .is_err());
    }

    #[test]
    fn staged_validator_rejects_uncovered_component_entry_once_arranging() {
        let (mut state, host, _, _) = seeded_component_fixture(BatchDeliveryStage::Arranging);
        let redirected = approved_library_event(&state, host, LibraryPosition::Top);
        state
            .active_batch_delivery_mut()
            .unwrap()
            .staged
            .as_mut()
            .unwrap()
            .members[0]
            .approved_event = Some(redirected);
        assert!(state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .is_err());
    }

    #[test]
    fn staged_validator_accepts_redirected_component_seed_until_outer_begin() {
        let (mut state, host, _, ordinary) =
            seeded_component_fixture(BatchDeliveryStage::Approving);
        let mut owner = state
            .resolution_stack
            .take_active_batch_delivery()
            .unwrap()
            .unwrap();
        let staged = owner.staged.as_mut().unwrap();
        staged.members[0].state = StagedBatchMemberState::Approved;
        staged.members[0].approved_event = Some(ProposedEvent::zone_change(
            host,
            Zone::Battlefield,
            Zone::Command,
            None,
        ));
        staged.cursor = 1;
        sync_staged_requests(&mut owner);
        owner.validate_staged().unwrap();
        let staged = owner.staged.as_mut().unwrap();
        staged.members[1].state = StagedBatchMemberState::Approved;
        staged.members[1].approved_event = Some(approved_library_event(
            &state,
            ordinary,
            LibraryPosition::Bottom,
        ));
        staged.cursor = 2;
        sync_staged_requests(&mut owner);
        begin_staged_arrangement(&state, &mut owner);
        owner.validate_staged().unwrap();
        assert_eq!(
            owner.staged.as_ref().unwrap().arrangements[0]
                .batch
                .members
                .iter()
                .map(|entry| entry.identity.object_id)
                .collect::<Vec<_>>(),
            vec![ordinary]
        );
    }

    #[test]
    fn staged_validator_accepts_inherited_component_arrangement() {
        let (state, _, _, _) = inherited_component_fixture();
        state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .unwrap();
        let wire = serde_json::to_string(&state).unwrap();
        let restored: GameState = serde_json::from_str(&wire).unwrap();
        assert_eq!(
            restored.active_batch_delivery().unwrap(),
            state.active_batch_delivery().unwrap()
        );
    }

    #[test]
    fn staged_validator_rejects_drifted_inherited_arrangement() {
        let (mut state, _, _, _) = inherited_component_fixture();
        // Change both child copies: only the parent's independent authority can catch this.
        let child = state.active_batch_delivery_mut().unwrap();
        child.staged.as_mut().unwrap().arrangements[0]
            .batch
            .members
            .swap(0, 1);
        child
            .component_delivery_parent
            .as_mut()
            .unwrap()
            .inherited_arrangements[0]
            .batch
            .members
            .swap(0, 1);
        assert!(state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .is_err());
    }

    #[test]
    fn staged_validator_rejects_foreign_entry_outside_inheritance() {
        let (mut state, _, _, ordinary) = seeded_component_fixture(BatchDeliveryStage::Arranging);
        let mut foreign = MassLibraryOrderMember::card(
            ObjectIncarnationRef::of(ObjectId(999), 0),
            Zone::Battlefield,
        );
        foreign.departing_permanent =
            Some(ObjectIncarnationRef::from_object(&state.objects[&ordinary]));
        state
            .active_batch_delivery_mut()
            .unwrap()
            .staged
            .as_mut()
            .unwrap()
            .arrangements[0]
            .batch
            .members
            .push(foreign);
        // Valid component coverage first, then remove that authority.
        state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .unwrap();
        state
            .active_batch_delivery_mut()
            .unwrap()
            .staged
            .as_mut()
            .unwrap()
            .arrangements[0]
            .batch
            .members
            .last_mut()
            .unwrap()
            .departing_permanent = None;
        assert!(state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .is_err());
    }

    #[test]
    fn component_detach_strips_inherited_foreign_entries() {
        let (mut state, host, rider, _) = inherited_component_fixture();
        let mut child = state
            .resolution_stack
            .take_active_batch_delivery()
            .unwrap()
            .unwrap();
        child.detach_component_parent();
        assert_eq!(child.component_delivery_parent, None);
        let group = &child.staged.as_ref().unwrap().arrangements[0];
        assert_eq!(group.status, StagedArrangementStatus::Submitted);
        assert_eq!(
            group
                .batch
                .members
                .iter()
                .map(|entry| entry.identity.object_id)
                .collect::<Vec<_>>(),
            vec![rider, host]
        );
        child.validate_staged().unwrap();
    }

    #[test]
    fn prune_inherited_entries_keeps_link_equality() {
        let (mut state, _, _, ordinary) = inherited_component_fixture();
        let mut child = state
            .resolution_stack
            .take_active_batch_delivery()
            .unwrap()
            .unwrap();
        let identity = ObjectIncarnationRef::from_object(&state.objects[&ordinary]);
        let parent = state.active_batch_delivery_mut().unwrap();
        prune_staged_member_for_player_left(parent, identity);
        child.prune_inherited_entries(&[identity]);
        assert_eq!(
            child
                .component_delivery_parent
                .as_ref()
                .unwrap()
                .inherited_arrangements,
            parent.staged.as_ref().unwrap().arrangements
        );
        assert_eq!(
            child.staged.as_ref().unwrap().arrangements,
            parent.staged.as_ref().unwrap().arrangements
        );
        put_owner(&mut state, child);
        state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .unwrap();
    }

    #[test]
    fn staged_approval_retains_every_member_before_any_move() {
        let mut state = GameState::new_two_player(42);
        let source = effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let reqs = vec![
            ZoneMoveRequest::effect(a, Zone::Library, source)
                .at_library_position(LibraryPosition::Bottom),
            ZoneMoveRequest::effect(b, Zone::Library, source)
                .at_library_position(LibraryPosition::Bottom),
        ];
        let group = crate::game::triggers::allocate_logical_zone_change_group(&mut state, &[a, b]);
        let group_id = group.logical_group_id;
        let staged = new_staged_batch(&state, &reqs, Vec::new());
        let owner = new_staged_owner(&state, &reqs, group, staged, None);
        state.resolution_stack.park_batch_owner(owner).unwrap();

        let mut events = Vec::new();
        let mut owner = take_owner(&mut state, group_id);
        assert_eq!(
            consult_next_staged_member(&mut state, &mut owner, &mut events),
            StagedConsult::Continue
        );
        put_owner(&mut state, owner);
        assert!(events.is_empty(), "approval must not deliver");
        assert_eq!(state.objects[&a].zone, Zone::Graveyard);
        assert_eq!(
            state
                .active_batch_delivery()
                .unwrap()
                .staged
                .as_ref()
                .unwrap()
                .members[0]
                .state,
            StagedBatchMemberState::Approved
        );

        let mut owner = take_owner(&mut state, group_id);
        assert_eq!(
            consult_next_staged_member(&mut state, &mut owner, &mut events),
            StagedConsult::Continue
        );
        put_owner(&mut state, owner);
        assert!(events.is_empty(), "approval must not deliver");
        assert_eq!(state.objects[&b].zone, Zone::Graveyard);

        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        put_owner(&mut state, owner);
        assert_eq!(
            state
                .active_batch_delivery()
                .unwrap()
                .staged
                .as_ref()
                .unwrap()
                .stage,
            BatchDeliveryStage::Delivering,
            "a request-origin group needs no prompt"
        );
        assert!(events.is_empty());

        let mut owner = take_owner(&mut state, group_id);
        assert_eq!(
            deliver_staged_members(&mut state, &mut owner, &mut events),
            StagedStop::Delivered
        );
        assert_eq!(state.objects[&a].zone, Zone::Library);
        assert_eq!(state.objects[&b].zone, Zone::Library);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::ZoneChanged { .. }))
                .count(),
            2,
            "delivery emits every move"
        );
    }

    fn replacement_origin_group(
        state: &mut GameState,
        cards: &[ObjectId],
        position: LibraryPosition,
    ) -> LogicalZoneChangeGroupId {
        let members = cards
            .iter()
            .map(|id| {
                staged_member(
                    state,
                    *id,
                    Some(approved_library_event(state, *id, position.clone())),
                    StagedBatchMemberState::Approved,
                    Zone::Graveyard,
                    None,
                )
            })
            .collect();
        push_approved_staged(state, members, BatchDeliveryStage::Approving)
    }

    fn arrangement_prompt(state: &GameState) -> (Vec<ObjectId>, usize) {
        match &state.waiting_for {
            WaitingFor::DigRestSplitChoice {
                cards, top_count, ..
            } => (cards.clone(), *top_count),
            other => panic!("expected a batch arrangement prompt, got {other:?}"),
        }
    }

    /// CR 401.7: a request-origin numbered block uses reverse announcement
    /// order without asking for another permutation.
    #[test]
    fn derived_nth_from_top_pair_matches_legacy_reverse_insertion() {
        for count in [0, 1, 3] {
            let mut state = GameState::new_two_player(42);
            effect_source(&mut state);
            let sentinels: Vec<_> = (0..count)
                .map(|index| library_card(&mut state, index + 10))
                .collect();
            let a = graveyard_card(&mut state, 1);
            let b = graveyard_card(&mut state, 2);
            let position = LibraryPosition::NthFromTop { n: 2 };
            let members = [a, b]
                .into_iter()
                .map(|id| {
                    staged_member(
                        &state,
                        id,
                        Some(approved_library_event(&state, id, position.clone())),
                        StagedBatchMemberState::Approved,
                        Zone::Library,
                        Some(position.clone()),
                    )
                })
                .collect();
            let group_id = push_approved_staged(&mut state, members, BatchDeliveryStage::Approving);
            let mut owner = take_owner(&mut state, group_id);
            begin_staged_arrangement(&state, &mut owner);
            let staged = owner.staged.as_ref().unwrap();
            assert_eq!(staged.stage, BatchDeliveryStage::Delivering);
            assert_eq!(staged.arrangements.len(), 1);
            assert_eq!(
                staged.arrangements[0].status,
                StagedArrangementStatus::Derived
            );
            assert_eq!(
                staged.arrangements[0]
                    .batch
                    .members
                    .iter()
                    .map(|member| member.identity.object_id)
                    .collect::<Vec<_>>(),
                [b, a]
            );
            owner.validate_staged().expect("numbered group validates");
            let mut missing_group = owner.clone();
            missing_group.staged.as_mut().unwrap().arrangements.clear();
            assert!(
                missing_group.validate_staged().is_err(),
                "a numbered approval must have its arrangement authority"
            );
            assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_none());
            assert_eq!(
                deliver_staged_members(&mut state, &mut owner, &mut Vec::new()),
                StagedStop::Delivered
            );
            let mut expected = sentinels;
            let base = expected.len().min(1);
            expected.splice(base..base, [b, a]);
            assert_eq!(
                state
                    .library_of(PlayerId(0))
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[test]
    fn nth_from_top_group_gated_off_for_library_origin_and_shared_library() {
        let nth = LibraryPosition::NthFromTop { n: 2 };
        for (origin, position, sibling_position, sibling_owner, expected_positions) in [
            (Zone::Library, nth.clone(), nth.clone(), PlayerId(0), vec![]),
            (
                Zone::Graveyard,
                nth.clone(),
                LibraryPosition::Top,
                PlayerId(0),
                vec![LibraryPosition::Top],
            ),
            (
                Zone::Graveyard,
                nth.clone(),
                LibraryPosition::Bottom,
                PlayerId(0),
                vec![LibraryPosition::Bottom],
            ),
            (
                Zone::Graveyard,
                nth.clone(),
                LibraryPosition::NthFromTop { n: 3 },
                PlayerId(0),
                vec![],
            ),
            (
                Zone::Graveyard,
                nth.clone(),
                LibraryPosition::Top,
                PlayerId(1),
                vec![nth, LibraryPosition::Top],
            ),
            // Top and Bottom remain independent, even for library origins.
            (
                Zone::Library,
                LibraryPosition::Top,
                LibraryPosition::Bottom,
                PlayerId(0),
                vec![LibraryPosition::Top, LibraryPosition::Bottom],
            ),
        ] {
            let mut state = GameState::new_two_player(42);
            effect_source(&mut state);
            let a = if origin == Zone::Library {
                library_card(&mut state, 1)
            } else {
                graveyard_card(&mut state, 1)
            };
            let b = create_object(
                &mut state,
                CardId(2),
                sibling_owner,
                "Sibling".into(),
                origin,
            );
            let members = [(a, position), (b, sibling_position)]
                .into_iter()
                .map(|(id, position)| {
                    staged_member(
                        &state,
                        id,
                        Some(approved_library_event(&state, id, position.clone())),
                        StagedBatchMemberState::Approved,
                        Zone::Library,
                        Some(position),
                    )
                })
                .collect();
            let group_id = push_approved_staged(&mut state, members, BatchDeliveryStage::Approving);
            let mut owner = take_owner(&mut state, group_id);
            begin_staged_arrangement(&state, &mut owner);
            assert_eq!(
                owner
                    .staged
                    .as_ref()
                    .unwrap()
                    .arrangements
                    .iter()
                    .map(|group| group.position.clone())
                    .collect::<Vec<_>>(),
                expected_positions
            );
            owner
                .validate_staged()
                .expect("producer and validator share the gate");
        }
    }

    /// CR 401.4: a Top group can reorder library-origin members around
    /// cross-zone arrivals that are already present, as in Doomsday.
    #[test]
    fn staged_arrangement_slot_top_group_tolerates_interleaved_library_origin_members() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let library_a = library_card(&mut state, 1);
        let library_c = library_card(&mut state, 2);
        let library_e = library_card(&mut state, 3);
        let graveyard_a = graveyard_card(&mut state, 4);
        let graveyard_b = graveyard_card(&mut state, 5);
        // Reverse announcement derives the measured final-order ranks:
        // [Library C, Graveyard A, Library E, Graveyard B, Library A].
        let members = [library_a, graveyard_b, library_e, graveyard_a, library_c]
            .into_iter()
            .map(|id| {
                staged_member(
                    &state,
                    id,
                    Some(approved_library_event(&state, id, LibraryPosition::Top)),
                    StagedBatchMemberState::Approved,
                    Zone::Library,
                    Some(LibraryPosition::Top),
                )
            })
            .collect();
        let group_id = push_approved_staged(&mut state, members, BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        // The two graveyard cards arrived in an earlier batch. The current
        // library reordering has not delivered any of its library-origin cards.
        zones::move_to_library_at_index(&mut state, graveyard_a, Some(0), &mut Vec::new());
        zones::move_to_library_at_index(&mut state, graveyard_b, Some(2), &mut Vec::new());
        zones::reorder_within_library(
            &mut state,
            PlayerId(0),
            &[graveyard_a, library_e, graveyard_b, library_a, library_c],
            Some(0),
        );
        assert_eq!(
            state
                .library_of(PlayerId(0))
                .iter()
                .position(|id| *id == graveyard_a),
            Some(0)
        );
        assert_eq!(
            state
                .library_of(PlayerId(0))
                .iter()
                .position(|id| *id == graveyard_b),
            Some(2)
        );
        let current = &owner.staged.as_ref().unwrap().members[4];
        let slot = staged_arrangement_slot(&state, &owner, current).expect("Top reorder slot");
        assert_eq!(slot, 0, "Library C goes before every present higher rank");
        zones::move_to_library_at_index(&mut state, library_c, Some(slot), &mut Vec::new());
        assert_eq!(
            state
                .library_of(PlayerId(0))
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [library_c, graveyard_a, library_e, graveyard_b, library_a]
        );
    }

    /// CR 400.7 + CR 401.4: present cross-zone arrivals are recognized by
    /// incarnation, even before their own staged terminal has been recorded.
    #[test]
    fn nth_from_top_group_counts_present_positions_not_adjacent_ranks() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let l0 = library_card(&mut state, 10);
        let l1 = library_card(&mut state, 11);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let c = graveyard_card(&mut state, 3);
        let position = LibraryPosition::NthFromTop { n: 2 };
        let members = [a, b, c]
            .into_iter()
            .map(|id| {
                staged_member(
                    &state,
                    id,
                    Some(approved_library_event(&state, id, position.clone())),
                    StagedBatchMemberState::Approved,
                    Zone::Library,
                    Some(position.clone()),
                )
            })
            .collect();
        let group_id = push_approved_staged(&mut state, members, BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        // Derived final order is [C, B, A]. Present ranks {0, 2} occupy
        // adjacent library positions, not adjacent final-order ranks.
        zones::move_to_library_at_index(&mut state, c, Some(1), &mut Vec::new());
        zones::move_to_library_at_index(&mut state, a, Some(2), &mut Vec::new());
        let member = &owner.staged.as_ref().unwrap().members[1];
        let slot = staged_arrangement_slot(&state, &owner, member).expect("numbered group slot");
        assert_eq!(slot, 2);
        zones::move_to_library_at_index(&mut state, b, Some(slot), &mut Vec::new());
        assert_eq!(
            state
                .library_of(PlayerId(0))
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [l0, c, b, a, l1]
        );
    }

    #[test]
    fn staged_arrangement_submits_the_owner_permutation() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let p = library_card(&mut state, 10);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let group_id = replacement_origin_group(&mut state, &[a, b], LibraryPosition::Top);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        assert_eq!(
            owner.staged.as_ref().unwrap().stage,
            BatchDeliveryStage::Arranging
        );
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
        put_owner(&mut state, owner);
        let (offered, top_count) = arrangement_prompt(&state);
        assert_eq!(offered, [a, b]);
        assert_eq!(top_count, 2);
        let wire = serde_json::to_value(&state).expect("arrangement pause serializes");
        state = serde_json::from_value(wire).expect("arrangement pause decodes");
        state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .expect("restored arrangement is valid before submitting");

        let mut events = Vec::new();
        submit_staged_batch_arrangement(
            &mut state,
            group_id,
            PlayerId(0),
            &offered,
            top_count,
            DigRestSplitScope::OrderOnly,
            &[b, a],
            &mut events,
        )
        .expect("a full permutation is legal");
        assert!(state.active_batch_delivery().is_none(), "the batch settles");
        let library: Vec<ObjectId> = state.players[0].library.iter().copied().collect();
        assert_eq!(
            library,
            [b, a, p],
            "the submitted order is the final top order"
        );
        assert!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::ZoneChanged { .. }))
                .count()
                == 2
        );
    }

    /// CR 101.4 + CR 401.4: the stored arrangement groups are APNAP-ordered, so the
    /// monotone cursor IS the publication order even when the populations were
    /// announced by a later seat first. Both nonidentity submissions then settle.
    #[test]
    fn arrangement_groups_store_and_publish_in_apnap_order() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        // Announced by P1 FIRST, then P0.
        let p1_a = create_object(
            &mut state,
            CardId(31),
            PlayerId(1),
            "P1 graveyard A".to_string(),
            Zone::Graveyard,
        );
        let p1_b = create_object(
            &mut state,
            CardId(32),
            PlayerId(1),
            "P1 graveyard B".to_string(),
            Zone::Graveyard,
        );
        let p0_a = graveyard_card(&mut state, 33);
        let p0_b = graveyard_card(&mut state, 34);
        let members = [p1_a, p1_b, p0_a, p0_b]
            .into_iter()
            .map(|id| {
                staged_member(
                    &state,
                    id,
                    Some(approved_library_event(&state, id, LibraryPosition::Bottom)),
                    StagedBatchMemberState::Approved,
                    Zone::Library,
                    None,
                )
            })
            .collect();
        let group_id = push_approved_staged(&mut state, members, BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        {
            let staged = owner.staged.as_ref().expect("staged payload");
            assert_eq!(staged.stage, BatchDeliveryStage::Arranging);
            let owners: Vec<PlayerId> = staged
                .arrangements
                .iter()
                .map(|arrangement| arrangement.batch.owner)
                .collect();
            assert_eq!(
                owners,
                vec![PlayerId(0), PlayerId(1)],
                "the active player is stored first even though P1 announced first"
            );
            let p1_order: Vec<ObjectId> = staged.arrangements[1]
                .batch
                .members
                .iter()
                .map(|member| member.identity.object_id)
                .collect();
            assert_eq!(
                p1_order,
                vec![p1_a, p1_b],
                "within-group announcement order is preserved"
            );
        }

        // The first publication is the AP's group, at the monotone cursor.
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
        assert!(
            matches!(&state.waiting_for, WaitingFor::DigRestSplitChoice { player, .. } if *player == PlayerId(0)),
            "the AP's arrangement is offered first, got {:?}",
            state.waiting_for
        );
        assert_eq!(owner.staged.as_ref().unwrap().arrangement_cursor, 0);
        put_owner(&mut state, owner);

        let (offered, top_count) = arrangement_prompt(&state);
        assert_eq!(offered, [p0_a, p0_b]);
        let mut events = Vec::new();
        submit_staged_batch_arrangement(
            &mut state,
            group_id,
            PlayerId(0),
            &offered,
            top_count,
            DigRestSplitScope::OrderOnly,
            &[p0_b, p0_a],
            &mut events,
        )
        .expect("the AP's nonidentity order is legal");

        // The next publication must be the NAP's still-awaiting group — a
        // submission never skips an earlier owner, and the cursor advanced by one.
        assert!(
            matches!(&state.waiting_for, WaitingFor::DigRestSplitChoice { player, .. } if *player == PlayerId(1)),
            "the NAP's arrangement follows, got {:?}",
            state.waiting_for
        );
        let (offered, top_count) = arrangement_prompt(&state);
        assert_eq!(offered, [p1_a, p1_b]);
        submit_staged_batch_arrangement(
            &mut state,
            group_id,
            PlayerId(1),
            &offered,
            top_count,
            DigRestSplitScope::OrderOnly,
            &[p1_b, p1_a],
            &mut events,
        )
        .expect("the NAP's nonidentity order is legal");

        assert!(
            state.active_batch_delivery().is_none(),
            "every group submitted, so the batch settles"
        );
        let p0_library: Vec<ObjectId> = state.library_of(PlayerId(0)).iter().copied().collect();
        let p1_library: Vec<ObjectId> = state.library_of(PlayerId(1)).iter().copied().collect();
        for (library, bottom, above) in [(&p0_library, p0_a, p0_b), (&p1_library, p1_a, p1_b)] {
            let bottom = library
                .iter()
                .position(|id| *id == bottom)
                .expect("card placed");
            let above = library
                .iter()
                .position(|id| *id == above)
                .expect("card placed");
            assert!(
                bottom > above,
                "each owner's submitted Bottom order is the delivered order"
            );
        }
    }

    #[test]
    fn staged_arrangement_rejects_every_invalid_submission_before_mutating() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let foreign = graveyard_card(&mut state, 3);
        let group_id = replacement_origin_group(&mut state, &[a, b], LibraryPosition::Bottom);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
        put_owner(&mut state, owner);
        let (offered, top_count) = arrangement_prompt(&state);
        let mut events = Vec::new();
        for (arrangement, scope, owner) in [
            (vec![a], DigRestSplitScope::OrderOnly, PlayerId(0)),
            (vec![a, a], DigRestSplitScope::OrderOnly, PlayerId(0)),
            (vec![a, foreign], DigRestSplitScope::OrderOnly, PlayerId(0)),
            (
                vec![a, b],
                DigRestSplitScope::PartitionAndOrder,
                PlayerId(0),
            ),
            (vec![a, b], DigRestSplitScope::OrderOnly, PlayerId(1)),
        ] {
            assert!(
                submit_staged_batch_arrangement(
                    &mut state,
                    group_id,
                    owner,
                    &offered,
                    top_count,
                    scope,
                    &arrangement,
                    &mut events,
                )
                .is_err(),
                "invalid submission {arrangement:?}/{scope:?}/{owner:?}"
            );
        }
        assert!(
            state.active_batch_delivery().is_some(),
            "no mutation on rejection"
        );
        assert_eq!(
            state
                .active_batch_delivery()
                .unwrap()
                .staged
                .as_ref()
                .unwrap()
                .stage,
            BatchDeliveryStage::Arranging
        );
        assert_eq!(state.objects[&a].zone, Zone::Graveyard);
        assert!(events.is_empty());
    }

    #[test]
    fn singleton_and_empty_groups_never_prompt() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let group_id = replacement_origin_group(&mut state, &[a], LibraryPosition::Top);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        put_owner(&mut state, owner);
        let staged = state
            .active_batch_delivery()
            .unwrap()
            .staged
            .as_ref()
            .unwrap();
        assert_eq!(staged.arrangements.len(), 1);
        assert_eq!(
            staged.arrangements[0].status,
            StagedArrangementStatus::Derived
        );
        assert_eq!(staged.stage, BatchDeliveryStage::Delivering);
        let mut owner = take_owner(&mut state, group_id);
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_none());
        put_owner(&mut state, owner);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn finalize_moves_a_retained_outcome_only_without_a_live_child() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let member = staged_member(
            &state,
            a,
            Some(approved_library_event(&state, a, LibraryPosition::Bottom)),
            StagedBatchMemberState::Approved,
            Zone::Graveyard,
            None,
        );
        let mut sibling = staged_member(
            &state,
            b,
            Some(approved_library_event(&state, b, LibraryPosition::Bottom)),
            StagedBatchMemberState::Approved,
            Zone::Graveyard,
            None,
        );
        sibling.announcement_index = 1;
        let group = crate::game::triggers::allocate_logical_zone_change_group(&mut state, &[a, b]);
        let group_id = group.logical_group_id;
        let mut members = vec![member, sibling];
        members[0].state = StagedBatchMemberState::InnerConsulting;
        members[0].approved_event = None;
        members[0].pending_outcome = Some(StagedMemberOutcome::Prevented);
        let staged = StagedBatchDelivery {
            stage: BatchDeliveryStage::Approving,
            cursor: 0,
            members,
            arrangements: Vec::new(),
            arrangement_cursor: 0,
            population_devour_snapshot: None,
        };
        state.push_batch_delivery(PendingBatchDeliveries {
            component_delivery_parent: None,
            logical_zone_change_group: group,
            paused_current: None,
            remaining: Vec::new(),
            destination: Zone::Library,
            source_id: None,
            enter_tapped: EtbTapState::Unspecified,
            exile_tracking: ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: None,
            replacement_applied: HashSet::new(),
            requests: Vec::new(),
            attempted: vec![a, b],
            zone_change_record_start: state.zone_changes_this_turn.len(),
            deferred_events: Vec::new(),
            staged: Some(staged),
        });
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(finalize_staged_pending_outcome(&mut state, group_id));
        let staged = state
            .active_batch_delivery()
            .unwrap()
            .staged
            .as_ref()
            .unwrap();
        assert_eq!(staged.members[0].state, StagedBatchMemberState::Terminal);
        assert_eq!(
            staged.members[0].terminal,
            Some(ZoneMoveCompletion::Prevented)
        );
        assert_eq!(staged.cursor, 1);
        assert!(
            state
                .active_batch_delivery()
                .unwrap()
                .logical_zone_change_group
                .terminal_outcomes
                .is_empty(),
            "the terminal is recorded only at settlement"
        );
    }

    fn member_stage_fixture(stage: BatchDeliveryStage) -> GameState {
        let mut state = GameState::new_two_player(42);
        let source = effect_source(&mut state);
        let members = (1..=3)
            .map(|card| {
                let id = create_object(
                    &mut state,
                    CardId(card),
                    PlayerId(0),
                    format!("Staged permanent {card}"),
                    Zone::Battlefield,
                );
                let mut member = staged_member(
                    &state,
                    id,
                    Some(ProposedEvent::zone_change(
                        id,
                        Zone::Battlefield,
                        Zone::Graveyard,
                        Some(source),
                    )),
                    StagedBatchMemberState::Approved,
                    Zone::Graveyard,
                    None,
                );
                member.operation = StagedMemberOperation::Destroy(StagedDestroyOperation {
                    source,
                    cant_regenerate: false,
                    outer: StagedDestroyOuterApproval::Approved,
                });
                member
            })
            .collect();
        push_approved_staged(&mut state, members, stage);
        let owner = state.active_batch_delivery_mut().unwrap();
        let staged = owner.staged.as_mut().unwrap();
        staged.cursor = if stage == BatchDeliveryStage::Approving {
            for member in staged.members.iter_mut().skip(1) {
                set_fixture_member_state(member, StagedBatchMemberState::Unconsulted);
            }
            1
        } else {
            0
        };
        sync_staged_requests(owner);
        owner
            .validate_staged()
            .expect("the member-stage baseline is valid before mutation");
        state
    }

    fn set_fixture_member_state(member: &mut StagedBatchMember, state: StagedBatchMemberState) {
        member.state = state;
        member.approved_event = None;
        member.pending_outcome = None;
        member.terminal = None;
        if let StagedMemberOperation::Destroy(operation) = &mut member.operation {
            operation.outer = match state {
                StagedBatchMemberState::Unconsulted | StagedBatchMemberState::OuterConsulting => {
                    StagedDestroyOuterApproval::Unapproved
                }
                StagedBatchMemberState::InnerConsulting
                | StagedBatchMemberState::Approved
                | StagedBatchMemberState::PreventedWithChild
                | StagedBatchMemberState::Terminal
                | StagedBatchMemberState::Abandoned => StagedDestroyOuterApproval::Approved,
            };
        }
        match state {
            StagedBatchMemberState::Unconsulted
            | StagedBatchMemberState::OuterConsulting
            | StagedBatchMemberState::InnerConsulting
            | StagedBatchMemberState::Abandoned => {}
            StagedBatchMemberState::Approved => {
                member.approved_event = Some(ProposedEvent::zone_change(
                    member.identity.object_id,
                    Zone::Battlefield,
                    Zone::Graveyard,
                    None,
                ));
            }
            StagedBatchMemberState::PreventedWithChild => {
                member.pending_outcome = Some(StagedMemberOutcome::Prevented);
            }
            StagedBatchMemberState::Terminal => {
                member.terminal = Some(ZoneMoveCompletion::Prevented);
            }
        }
    }

    #[test]
    fn staged_transport_rejects_unfinished_consultations_outside_approval_cursor() {
        for stage in [
            BatchDeliveryStage::Approving,
            BatchDeliveryStage::Arranging,
            BatchDeliveryStage::Delivering,
        ] {
            for forbidden in [
                StagedBatchMemberState::OuterConsulting,
                StagedBatchMemberState::InnerConsulting,
                StagedBatchMemberState::PreventedWithChild,
                StagedBatchMemberState::Unconsulted,
            ] {
                let mut state = member_stage_fixture(stage);
                let mut wire =
                    serde_json::to_value(ResolutionStateWire::from_game_state(state.clone()))
                        .expect("valid baseline serializes");
                let owner = state.active_batch_delivery_mut().unwrap();
                owner
                    .validate_staged()
                    .expect("control: completed non-positioned approvals are valid");
                let index = if stage == BatchDeliveryStage::Approving {
                    0
                } else {
                    1
                };
                set_fixture_member_state(
                    &mut owner.staged.as_mut().unwrap().members[index],
                    forbidden,
                );
                // Keep each member and its legacy projection coherent: only
                // the stage/cursor relation is malformed.
                sync_staged_requests(owner);
                let frames = wire["resolution_frames"]["frames"].as_array_mut().unwrap();
                let batch = frames
                    .iter_mut()
                    .find(|frame| frame["type"] == "BatchDelivery")
                    .unwrap();
                batch["data"] = serde_json::to_value(owner).unwrap();
                let error = match serde_json::from_value::<ResolutionStateWire>(wire) {
                    Ok(_) => panic!("{stage:?} accepted unfinished {forbidden:?}"),
                    Err(error) => error.to_string(),
                };
                let expected = match stage {
                    BatchDeliveryStage::Approving => {
                        "an unfinished staged consultation precedes the approval cursor"
                    }
                    BatchDeliveryStage::Arranging => {
                        "a staged member entered arrangement without a completed consultation"
                    }
                    BatchDeliveryStage::Delivering => {
                        "a staged member entered delivery without a final approval"
                    }
                };
                assert!(error.contains(expected), "{stage:?}/{forbidden:?}: {error}");
            }
        }
    }

    #[test]
    fn staged_transport_accepts_completed_members_and_terminal_or_abandoned_prefixes() {
        for stage in [
            BatchDeliveryStage::Approving,
            BatchDeliveryStage::Arranging,
            BatchDeliveryStage::Delivering,
        ] {
            for completed in [
                StagedBatchMemberState::Approved,
                StagedBatchMemberState::Terminal,
                StagedBatchMemberState::Abandoned,
            ] {
                let mut state = member_stage_fixture(stage);
                let owner = state.active_batch_delivery_mut().unwrap();
                let staged = owner.staged.as_mut().unwrap();
                let index = if stage == BatchDeliveryStage::Approving {
                    0
                } else if completed == StagedBatchMemberState::Approved {
                    1
                } else {
                    staged.cursor = 1;
                    0
                };
                set_fixture_member_state(&mut staged.members[index], completed);
                sync_staged_requests(owner);
                owner
                    .validate_staged()
                    .expect("completed state is legal at this stage and cursor");
                let wire = serde_json::to_value(&state).unwrap();
                let restored: GameState =
                    serde_json::from_value(wire).expect("completed member decodes");
                assert_eq!(
                    restored.active_batch_delivery().unwrap().staged,
                    state.active_batch_delivery().unwrap().staged,
                    "{stage:?}/{completed:?}"
                );
            }
        }
    }

    #[test]
    fn staged_transport_checks_every_state_at_approval_cursor() {
        for (current, allowed) in [
            (StagedBatchMemberState::Unconsulted, true),
            (StagedBatchMemberState::OuterConsulting, true),
            (StagedBatchMemberState::InnerConsulting, true),
            (StagedBatchMemberState::Approved, false),
            (StagedBatchMemberState::PreventedWithChild, true),
            (StagedBatchMemberState::Terminal, false),
            (StagedBatchMemberState::Abandoned, true),
        ] {
            let mut state = member_stage_fixture(BatchDeliveryStage::Approving);
            let mut wire =
                serde_json::to_value(ResolutionStateWire::from_game_state(state.clone()))
                    .expect("valid baseline serializes");
            let owner = state.active_batch_delivery_mut().unwrap();
            set_fixture_member_state(&mut owner.staged.as_mut().unwrap().members[1], current);
            sync_staged_requests(owner);
            let frames = wire["resolution_frames"]["frames"].as_array_mut().unwrap();
            let batch = frames
                .iter_mut()
                .find(|frame| frame["type"] == "BatchDelivery")
                .unwrap();
            batch["data"] = serde_json::to_value(&*owner).unwrap();
            let decoded = serde_json::from_value::<ResolutionStateWire>(wire);
            if allowed {
                owner
                    .validate_staged()
                    .expect("an unfinished consultation or abandonment is valid at the cursor");
                let restored = decoded
                    .expect("cursor consultation decodes")
                    .into_game_state();
                assert_eq!(
                    restored
                        .active_batch_delivery()
                        .unwrap()
                        .staged
                        .as_ref()
                        .unwrap()
                        .members[1]
                        .state,
                    current
                );
            } else {
                let expected = "a consulted staged member follows the approval cursor";
                assert_eq!(owner.validate_staged().unwrap_err(), expected);
                let error = match decoded {
                    Ok(_) => panic!("approval cursor accepted {current:?}"),
                    Err(error) => error.to_string(),
                };
                assert!(error.contains(expected), "{current:?}: {error}");
            }
        }
    }

    #[test]
    fn staged_transport_checks_every_state_after_approval_cursor() {
        for (state_after, allowed) in [
            (StagedBatchMemberState::Unconsulted, true),
            (StagedBatchMemberState::OuterConsulting, false),
            (StagedBatchMemberState::InnerConsulting, false),
            (StagedBatchMemberState::Approved, false),
            (StagedBatchMemberState::PreventedWithChild, false),
            (StagedBatchMemberState::Terminal, false),
            (StagedBatchMemberState::Abandoned, true),
        ] {
            let mut state = member_stage_fixture(BatchDeliveryStage::Approving);
            let mut wire =
                serde_json::to_value(ResolutionStateWire::from_game_state(state.clone()))
                    .expect("valid baseline serializes");
            let owner = state.active_batch_delivery_mut().unwrap();
            owner
                .validate_staged()
                .expect("control: unconsulted suffix is valid");
            set_fixture_member_state(&mut owner.staged.as_mut().unwrap().members[2], state_after);
            sync_staged_requests(owner);
            let frames = wire["resolution_frames"]["frames"].as_array_mut().unwrap();
            let batch = frames
                .iter_mut()
                .find(|frame| frame["type"] == "BatchDelivery")
                .unwrap();
            batch["data"] = serde_json::to_value(owner).unwrap();
            let decoded = serde_json::from_value::<ResolutionStateWire>(wire);
            if allowed {
                let restored = decoded
                    .expect("unconsulted or abandoned suffix decodes")
                    .into_game_state();
                assert_eq!(
                    restored
                        .active_batch_delivery()
                        .unwrap()
                        .staged
                        .as_ref()
                        .unwrap()
                        .members[2]
                        .state,
                    state_after
                );
            } else {
                let error = match decoded {
                    Ok(_) => panic!("approval suffix accepted {state_after:?}"),
                    Err(error) => error.to_string(),
                };
                assert!(
                    error.contains("a consulted staged member follows the approval cursor"),
                    "{state_after:?}: {error}"
                );
            }
        }
    }

    #[test]
    fn staged_transport_requires_destroy_outer_approval_for_inner_consultation() {
        for outer in [
            StagedDestroyOuterApproval::Approved,
            StagedDestroyOuterApproval::Unapproved,
        ] {
            let mut state = member_stage_fixture(BatchDeliveryStage::Approving);
            let mut wire =
                serde_json::to_value(ResolutionStateWire::from_game_state(state.clone()))
                    .expect("valid baseline serializes");
            let owner = state.active_batch_delivery_mut().unwrap();
            let member = &mut owner.staged.as_mut().unwrap().members[1];
            set_fixture_member_state(member, StagedBatchMemberState::InnerConsulting);
            let StagedMemberOperation::Destroy(operation) = &mut member.operation else {
                panic!("fixture member is a Destroy");
            };
            operation.outer = outer;
            sync_staged_requests(owner);
            let frames = wire["resolution_frames"]["frames"].as_array_mut().unwrap();
            let batch = frames
                .iter_mut()
                .find(|frame| frame["type"] == "BatchDelivery")
                .unwrap();
            batch["data"] = serde_json::to_value(&*owner).unwrap();
            let decoded = serde_json::from_value::<ResolutionStateWire>(wire);
            match outer {
                StagedDestroyOuterApproval::Approved => {
                    owner
                        .validate_staged()
                        .expect("outer approval permits the inner consult");
                    let restored = decoded
                        .expect("approved inner consult decodes")
                        .into_game_state();
                    assert_eq!(
                        restored.active_batch_delivery().unwrap().staged,
                        owner.staged
                    );
                }
                StagedDestroyOuterApproval::Unapproved => {
                    let expected = "an inner-consulting destroy member has no outer approval";
                    assert_eq!(owner.validate_staged().unwrap_err(), expected);
                    let error = match decoded {
                        Ok(_) => panic!("inner Destroy consult accepted an unapproved outer event"),
                        Err(error) => error.to_string(),
                    };
                    assert!(error.contains(expected), "{error}");
                }
            }
        }
    }

    #[test]
    fn staged_transport_rejects_undelivered_member_before_arranging_or_delivering_cursor() {
        for stage in [
            BatchDeliveryStage::Arranging,
            BatchDeliveryStage::Delivering,
        ] {
            let mut state = GameState::new_two_player(42);
            effect_source(&mut state);
            let a = graveyard_card(&mut state, 1);
            let b = graveyard_card(&mut state, 2);
            let group = replacement_origin_group(&mut state, &[a, b], LibraryPosition::Top);
            let mut owner = take_owner(&mut state, group);
            begin_staged_arrangement(&state, &mut owner);
            if stage == BatchDeliveryStage::Arranging {
                assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
            } else {
                let staged = owner.staged.as_mut().unwrap();
                staged.stage = stage;
                for arrangement in &mut staged.arrangements {
                    arrangement.status = StagedArrangementStatus::Submitted;
                }
            }
            owner
                .validate_staged()
                .expect("cursor at the first undelivered member is valid");
            put_owner(&mut state, owner);
            let mut wire = serde_json::to_value(ResolutionStateWire::from_game_state(state))
                .expect("valid cursor serializes");
            serde_json::from_value::<ResolutionStateWire>(wire.clone())
                .expect("control: the first undelivered member decodes");
            let frames = wire["resolution_frames"]["frames"]
                .as_array_mut()
                .expect("typed frames");
            let batch = frames
                .iter_mut()
                .find(|frame| frame["type"] == "BatchDelivery")
                .expect("the wire contains its staged batch");
            batch["data"]["staged"]["cursor"] = serde_json::json!(2);
            let error = match serde_json::from_value::<ResolutionStateWire>(wire) {
                Ok(_) => panic!("{stage:?}: an undelivered member was skipped at decode"),
                Err(error) => error,
            };
            assert!(
                error
                    .to_string()
                    .contains("an undelivered staged member precedes the delivery cursor"),
                "{stage:?}: {error}"
            );
        }
    }

    #[test]
    fn staged_arranging_concession_abandoned_prefix_round_trips_and_resumes() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let sentinel = library_card(&mut state, 10);
        let departed = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Departed".into(),
            Zone::Graveyard,
        );
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let group = replacement_origin_group(&mut state, &[departed, a, b], LibraryPosition::Top);
        let mut owner = take_owner(&mut state, group);
        begin_staged_arrangement(&state, &mut owner);
        let identity = owner.staged.as_ref().unwrap().members[0].identity;
        owner
            .logical_zone_change_group
            .record_abandoned_by_player_left(identity)
            .unwrap();
        prune_staged_member_for_player_left(&mut owner, identity);
        let staged = owner.staged.as_ref().unwrap();
        assert_eq!(staged.stage, BatchDeliveryStage::Arranging);
        assert_eq!(staged.cursor, 1);
        assert_eq!(staged.members[0].state, StagedBatchMemberState::Abandoned);
        owner
            .validate_staged()
            .expect("concession may advance over an abandoned prefix");
        let mut events = Vec::new();
        move_object(
            &mut state,
            ZoneMoveRequest::player_left_game(departed, Zone::Exile),
            &mut events,
        );
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
        put_owner(&mut state, owner);
        let wire = serde_json::to_value(&state).expect("abandoned prefix serializes");
        let mut restored: GameState =
            serde_json::from_value(wire).expect("abandoned prefix decodes");
        restored
            .resolution_stack
            .validate_at_rest(&restored.waiting_for)
            .expect("concession prefix remains valid after restore");
        let (offered, top_count) = arrangement_prompt(&restored);
        assert_eq!(offered, [a, b]);
        submit_staged_batch_arrangement(
            &mut restored,
            group,
            PlayerId(0),
            &offered,
            top_count,
            DigRestSplitScope::OrderOnly,
            &[b, a],
            &mut events,
        )
        .expect("surviving members resume after the abandoned prefix");
        assert_eq!(restored.objects[&departed].zone, Zone::Exile);
        assert_eq!(
            restored.players[0]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [b, a, sentinel]
        );
        assert!(restored.active_batch_delivery().is_none());
        assert!(restored.stack.is_empty());
        assert!(restored.resolution_stack.is_empty());
    }

    #[test]
    fn staged_transport_round_trips_every_stage() {
        for operation in [
            StagedMemberOperation::ZoneMove,
            StagedMemberOperation::Destroy(StagedDestroyOperation {
                source: ObjectId(999),
                cant_regenerate: false,
                outer: StagedDestroyOuterApproval::Unapproved,
            }),
        ] {
            for stage in [
                BatchDeliveryStage::Approving,
                BatchDeliveryStage::Arranging,
                BatchDeliveryStage::Delivering,
            ] {
                let mut state = GameState::new_two_player(42);
                effect_source(&mut state);
                let a = graveyard_card(&mut state, 1);
                let member = staged_member(
                    &state,
                    a,
                    Some(approved_library_event(&state, a, LibraryPosition::Bottom)),
                    StagedBatchMemberState::Approved,
                    Zone::Graveyard,
                    None,
                );
                let mut member = member;
                member.operation = match (operation, stage) {
                    (
                        StagedMemberOperation::Destroy(operation),
                        BatchDeliveryStage::Arranging | BatchDeliveryStage::Delivering,
                    ) => StagedMemberOperation::Destroy(StagedDestroyOperation {
                        outer: StagedDestroyOuterApproval::Approved,
                        ..operation
                    }),
                    (operation, _) => operation,
                };
                match stage {
                    BatchDeliveryStage::Approving => {
                        member.state = StagedBatchMemberState::Unconsulted;
                        member.approved_event = None;
                    }
                    // A Delivering payload whose cursor is at the end has already
                    // delivered every member, so each carries its terminal. An
                    // `Approved` member behind the delivery cursor is the malformed
                    // shape the staged validator rejects.
                    BatchDeliveryStage::Delivering => {
                        member.state = StagedBatchMemberState::Terminal;
                        member.terminal = Some(ZoneMoveCompletion::Moved);
                    }
                    BatchDeliveryStage::Arranging => {}
                }
                let group_id = push_approved_staged(&mut state, vec![member], stage);
                if stage != BatchDeliveryStage::Approving {
                    let mut owner = take_owner(&mut state, group_id);
                    begin_staged_arrangement(&state, &mut owner);
                    let staged = owner.staged.as_mut().unwrap();
                    staged.stage = stage;
                    staged.cursor = match stage {
                        BatchDeliveryStage::Delivering => staged.members.len(),
                        BatchDeliveryStage::Approving | BatchDeliveryStage::Arranging => 0,
                    };
                    put_owner(&mut state, owner);
                }
                let before = state.active_batch_delivery().unwrap().staged.clone();
                let wire = serde_json::to_value(&state).expect("state serializes");
                let restored: GameState = serde_json::from_value(wire).expect("state decodes");
                let frame = restored
                    .active_batch_delivery()
                    .expect("the staged owner survives the round trip");
                assert_eq!(
                    frame.staged, before,
                    "stage, cursor, members and approvals survive {stage:?}"
                );
                assert_eq!(frame.logical_zone_change_group.logical_group_id, group_id);
            }
        }
    }

    #[test]
    fn staged_validator_rejects_duplicate_owner_position_groups() {
        for position in [
            LibraryPosition::Top,
            LibraryPosition::Bottom,
            LibraryPosition::NthFromTop { n: 2 },
        ] {
            let mut state = GameState::new_two_player(42);
            effect_source(&mut state);
            let a = graveyard_card(&mut state, 1);
            let b = graveyard_card(&mut state, 2);
            let group_id = replacement_origin_group(&mut state, &[a, b], position);
            let mut owner = take_owner(&mut state, group_id);
            begin_staged_arrangement(&state, &mut owner);
            owner.validate_staged().expect("the derived group is valid");
            let staged = owner.staged.as_mut().unwrap();
            staged.arrangements.push(staged.arrangements[0].clone());
            assert!(
                owner.validate_staged().is_err(),
                "duplicate keys are ambiguous"
            );
        }
    }

    #[test]
    fn staged_validator_rejects_duplicate_submitted_seed_keys_with_disjoint_members() {
        for position in [
            LibraryPosition::Top,
            LibraryPosition::Bottom,
            LibraryPosition::NthFromTop { n: 2 },
        ] {
            let mut state = member_stage_fixture(BatchDeliveryStage::Approving);
            let owner = state.active_batch_delivery_mut().unwrap();
            let staged = owner.staged.as_mut().unwrap();
            staged.arrangements.push(StagedLibraryArrangement {
                position,
                batch: MassLibraryOrderBatch {
                    owner: PlayerId(0),
                    arranger: None,
                    members: vec![MassLibraryOrderMember::card(
                        staged.members[0].identity,
                        Zone::Graveyard,
                    )],
                },
                status: StagedArrangementStatus::Submitted,
            });
            owner
                .validate_staged()
                .expect("control: one submitted seed at the key is valid");
            let staged = owner.staged.as_mut().unwrap();
            let mut duplicate = staged.arrangements[0].clone();
            duplicate.batch.members = vec![MassLibraryOrderMember::card(
                staged.members[1].identity,
                Zone::Graveyard,
            )];
            staged.arrangements.push(duplicate);
            assert_eq!(
                owner.validate_staged().unwrap_err(),
                "staged arrangements repeat an owner/position group",
                "disjoint Submitted seeds must not bypass key uniqueness"
            );
        }
    }

    #[test]
    fn staged_validator_requires_group_for_every_positioned_approval() {
        for stage in [
            BatchDeliveryStage::Arranging,
            BatchDeliveryStage::Delivering,
        ] {
            let mut state = GameState::new_two_player(42);
            effect_source(&mut state);
            let a = graveyard_card(&mut state, 1);
            let b = graveyard_card(&mut state, 2);
            let members = [(a, LibraryPosition::Top), (b, LibraryPosition::Bottom)]
                .into_iter()
                .map(|(id, position)| {
                    staged_member(
                        &state,
                        id,
                        Some(approved_library_event(&state, id, position)),
                        StagedBatchMemberState::Approved,
                        Zone::Graveyard,
                        None,
                    )
                })
                .collect();
            let group_id = push_approved_staged(&mut state, members, BatchDeliveryStage::Approving);
            let mut owner = take_owner(&mut state, group_id);
            begin_staged_arrangement(&state, &mut owner);
            owner.staged.as_mut().unwrap().stage = stage;
            owner
                .validate_staged()
                .expect("Top and Bottom are independent valid keys");
            let missing = owner.staged.as_mut().unwrap().arrangements.remove(0);
            assert!(owner.validate_staged().is_err(), "every key needs a group");
            owner
                .staged
                .as_mut()
                .unwrap()
                .arrangements
                .insert(0, missing);
            owner.staged.as_mut().unwrap().arrangements[0]
                .batch
                .members
                .clear();
            assert!(
                owner.validate_staged().is_err(),
                "a group must cover its approval"
            );
            owner.staged.as_mut().unwrap().arrangements.clear();
            assert!(
                owner.validate_staged().is_err(),
                "an empty set omits every key"
            );
        }
    }

    #[test]
    fn staged_validator_rejects_identity_at_another_approved_key() {
        for stage in [
            BatchDeliveryStage::Arranging,
            BatchDeliveryStage::Delivering,
        ] {
            for both_top in [false, true] {
                let mut state = GameState::new_two_player(42);
                effect_source(&mut state);
                let a = graveyard_card(&mut state, 1);
                let b = graveyard_card(&mut state, 2);
                let members = [
                    (a, LibraryPosition::Top),
                    (
                        b,
                        if both_top {
                            LibraryPosition::Top
                        } else {
                            LibraryPosition::Bottom
                        },
                    ),
                ]
                .into_iter()
                .map(|(id, position)| {
                    staged_member(
                        &state,
                        id,
                        Some(approved_library_event(&state, id, position)),
                        StagedBatchMemberState::Approved,
                        Zone::Graveyard,
                        None,
                    )
                })
                .collect();
                let group =
                    push_approved_staged(&mut state, members, BatchDeliveryStage::Approving);
                let mut owner = take_owner(&mut state, group);
                begin_staged_arrangement(&state, &mut owner);
                let staged = owner.staged.as_mut().unwrap();
                staged.stage = stage;
                staged.cursor = 0;
                for arrangement in &mut staged.arrangements {
                    arrangement.status = StagedArrangementStatus::Submitted;
                }
                owner.validate_staged().expect("valid approved keys");
                let staged = owner.staged.as_mut().unwrap();
                if both_top {
                    let mut wrong = staged.arrangements[0].clone();
                    wrong.position = LibraryPosition::Bottom;
                    wrong
                        .batch
                        .members
                        .retain(|entry| entry.identity.object_id == a);
                    staged.arrangements.insert(0, wrong);
                } else {
                    let wrong = staged.arrangements[1].batch.members[0].clone();
                    staged.arrangements[0].batch.members.push(wrong);
                }
                assert!(
                    owner.validate_staged().is_err(),
                    "{stage:?}, both_top={both_top}"
                );
            }
        }
    }

    #[test]
    fn staged_validator_rejects_arrangement_outside_numbered_group_domain() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = library_card(&mut state, 1);
        let group =
            replacement_origin_group(&mut state, &[a], LibraryPosition::NthFromTop { n: 2 });
        let mut owner = take_owner(&mut state, group);
        let staged = owner.staged.as_mut().unwrap();
        staged.stage = BatchDeliveryStage::Delivering;
        staged.cursor = 0;
        owner
            .validate_staged()
            .expect("ungrouped library-origin numbered placement is valid");
        let staged = owner.staged.as_mut().unwrap();
        staged.arrangements.push(StagedLibraryArrangement {
            position: LibraryPosition::NthFromTop { n: 2 },
            batch: MassLibraryOrderBatch {
                owner: PlayerId(0),
                arranger: None,
                members: vec![MassLibraryOrderMember::card(
                    staged.members[0].identity,
                    Zone::Library,
                )],
            },
            status: StagedArrangementStatus::Submitted,
        });
        assert_eq!(
            owner
                .validate_staged()
                .expect_err("library-origin numbered placement is ungrouped"),
            "a staged arrangement is outside the approved grouping domain"
        );
    }

    #[test]
    fn staged_arrangement_slot_uses_the_approved_key() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let sentinel = library_card(&mut state, 10);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let members = [(a, LibraryPosition::Top), (b, LibraryPosition::Bottom)]
            .into_iter()
            .map(|(id, position)| {
                staged_member(
                    &state,
                    id,
                    Some(approved_library_event(&state, id, position)),
                    StagedBatchMemberState::Approved,
                    Zone::Graveyard,
                    None,
                )
            })
            .collect();
        let group = push_approved_staged(&mut state, members, BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group);
        begin_staged_arrangement(&state, &mut owner);
        let staged = owner.staged.as_mut().unwrap();
        // A malformed duplicate must not redirect B from its approved Bottom.
        let b_entry = staged.arrangements[1].batch.members[0].clone();
        staged.arrangements[0].batch.members.push(b_entry);
        for member in &owner.staged.as_ref().unwrap().members {
            let slot = staged_arrangement_slot(&state, &owner, member).unwrap();
            zones::move_to_library_at_index(
                &mut state,
                member.identity.object_id,
                Some(slot),
                &mut Vec::new(),
            );
        }
        assert_eq!(
            state
                .library_of(PlayerId(0))
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [a, sentinel, b]
        );
    }

    fn parked_population_for_decode(destroy: bool) -> GameState {
        let mut state = GameState::new_two_player(42);
        let source = effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let mut member = staged_member(
            &state,
            a,
            None,
            StagedBatchMemberState::Unconsulted,
            Zone::Graveyard,
            None,
        );
        if destroy {
            member.operation = StagedMemberOperation::Destroy(StagedDestroyOperation {
                source,
                cant_regenerate: false,
                outer: StagedDestroyOuterApproval::Unapproved,
            });
        }
        let group = push_approved_staged(&mut state, vec![member], BatchDeliveryStage::Approving);
        let completion = if destroy {
            BatchCompletion::DestroyPopulationSettled {
                ability: Box::new(ResolvedAbility::new(
                    Effect::NoOp,
                    vec![],
                    source,
                    PlayerId(0),
                )),
                settled_destroyed: None,
            }
        } else {
            BatchCompletion::ChangeZonePopulationSettled {
                tail: Box::new(ChangeZoneBatchTail {
                    group_id: group,
                    effect_kind: EffectKind::ChangeZone,
                    source_id: source,
                    controller: PlayerId(0),
                    destination: Zone::Graveyard,
                    count: ChangeZoneCountPolicy::MassAllMembers,
                    pending_return_result_producer: None,
                    own_exile_links: crate::types::game_state::OwnExileLinkSettlement::Retain,
                }),
                settled: None,
            }
        };
        state.active_batch_delivery_mut().unwrap().completion = Some(completion);
        state
    }

    #[test]
    fn staged_population_decode_rejects_premature_settlement() {
        for destroy in [true, false] {
            let state = parked_population_for_decode(destroy);
            let mut wire = serde_json::to_value(&state).unwrap();
            serde_json::from_value::<GameState>(wire.clone()).expect("valid parked population");
            // Mutate the valid wire, not the live owner: malformed saves must fail decoding.
            fn settle_population(value: &mut serde_json::Value) {
                match value {
                    serde_json::Value::Object(fields) => {
                        if let Some(payload) = fields.get_mut("ChangeZonePopulationSettled") {
                            payload["settled"] = serde_json::json!({
                                "policy_count": 0, "arrival_count": 0
                            });
                        }
                        if let Some(payload) = fields.get_mut("DestroyPopulationSettled") {
                            payload["settled_destroyed"] = serde_json::json!([]);
                        }
                        for child in fields.values_mut() {
                            settle_population(child);
                        }
                    }
                    serde_json::Value::Array(values) => {
                        for child in values {
                            settle_population(child);
                        }
                    }
                    _ => {}
                }
            }
            settle_population(&mut wire);
            assert!(
                serde_json::from_value::<GameState>(wire).is_err(),
                "destroy={destroy}"
            );
        }
    }

    #[test]
    fn staged_population_decode_rejects_published_return_result() {
        use crate::types::ability::ReturnResultId;
        use crate::types::game_state::ReturnResultOccurrenceId;
        let mut state = parked_population_for_decode(false);
        let occurrence = ReturnResultOccurrenceId(1);
        let result = ReturnResultId(1);
        state.next_return_result_occurrence_id = 2;
        state.active_return_result_occurrence = Some(occurrence);
        state
            .return_result_frames
            .insert(occurrence, Default::default());
        let Some(BatchCompletion::ChangeZonePopulationSettled { tail, .. }) =
            &mut state.active_batch_delivery_mut().unwrap().completion
        else {
            unreachable!()
        };
        tail.pending_return_result_producer = Some((occurrence, result));
        let valid = serde_json::to_value(&state).unwrap();
        serde_json::from_value::<GameState>(valid).expect("valid unpublished population result");
        state
            .return_result_frames
            .get_mut(&occurrence)
            .unwrap()
            .insert(result, vec![]);
        let malformed = serde_json::to_value(&state).unwrap();
        assert!(serde_json::from_value::<GameState>(malformed).is_err());
    }

    #[test]
    fn staged_validator_only_accepts_submitted_arrangements_during_approval() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let group_id = replacement_origin_group(&mut state, &[a], LibraryPosition::Top);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        let staged = owner.staged.as_mut().unwrap();
        staged.stage = BatchDeliveryStage::Approving;
        for member in &mut staged.members {
            member.state = StagedBatchMemberState::Unconsulted;
            member.approved_event = None;
        }
        sync_staged_requests(&mut owner);
        assert!(
            owner.validate_staged().is_err(),
            "approval cannot retain an unsubmitted arrangement"
        );
        owner.staged.as_mut().unwrap().arrangements[0].status = StagedArrangementStatus::Submitted;
        owner
            .validate_staged()
            .expect("a submitted seed can precede consultation of the remaining members");
    }

    #[test]
    fn staged_arrangement_prompt_and_payload_are_redacted_for_every_viewer() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let group_id = replacement_origin_group(&mut state, &[a, b], LibraryPosition::Bottom);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
        put_owner(&mut state, owner);
        let (offered, _) = arrangement_prompt(&state);

        // 0/1 are the players (actor/opponent); 7 is a spectator with no seat.
        for viewer in [PlayerId(0), PlayerId(1), PlayerId(7)] {
            let projected = crate::game::visibility::filter_state_for_viewer(&state, viewer);
            assert!(
                projected.active_batch_delivery().is_none(),
                "no staged owner is projected to viewer {viewer:?}"
            );
            let WaitingFor::DigRestSplitChoice {
                cards, completion, ..
            } = &projected.waiting_for
            else {
                panic!("the prompt survives as a prompt");
            };
            assert_eq!(
                cards.len(),
                offered.len(),
                "the offered card count is visible to every viewer"
            );
            assert!(
                completion.is_none(),
                "the batch continuation (and its group id) is stripped for {viewer:?}"
            );
            let wire = serde_json::to_string(&projected).expect("projection serializes");
            assert!(
                !wire.contains("\"staged\""),
                "the staged payload never reaches a client projection"
            );
        }
        let spectator = crate::game::visibility::filter_state_for_viewer(&state, PlayerId(7));
        let wire = serde_json::to_string(&spectator).unwrap();
        assert!(!wire.contains("logical_group_id"));
    }

    #[test]
    fn mixed_destination_batch_stays_on_the_legacy_path() {
        let mut state = GameState::new_two_player(42);
        let source = effect_source(&mut state);
        let milled = library_card(&mut state, 10);
        let creature = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Stays put".to_string(),
            Zone::Battlefield,
        );
        let mixed = vec![
            ZoneMoveRequest::effect(milled, Zone::Graveyard, source),
            ZoneMoveRequest::effect(creature, Zone::Battlefield, source),
        ];
        assert!(
            !staged_batch_admission(&state, &mixed),
            "a battlefield-destination member keeps the batch on the legacy path"
        );
        assert!(staged_batch_admission(
            &state,
            &[ZoneMoveRequest::effect(milled, Zone::Graveyard, source)]
        ));
        let mut events = Vec::new();
        let result = move_objects_simultaneously(&mut state, mixed, &mut events);
        assert!(matches!(result, BatchMoveResult::Done));
        assert!(state.active_batch_delivery().is_none());
        assert_eq!(state.objects[&milled].zone, Zone::Graveyard);
        assert_eq!(
            state.objects[&creature].zone,
            Zone::Battlefield,
            "the refused member remains on the legacy path too"
        );
    }

    fn library_order_member(
        identity: ObjectIncarnationRef,
        origin: Zone,
    ) -> MassLibraryOrderMember {
        MassLibraryOrderMember::card(identity, origin)
    }

    #[test]
    fn arrangement_rank_offsets_ignore_members_never_inserted() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let sentinel = library_card(&mut state, 10);
        let b = library_card(&mut state, 11);
        let c = library_card(&mut state, 12);
        // A is a token outside the battlefield: `move_to_library_at_index`
        // refuses it, so it is Remained and never inserted.
        let a = create_object(
            &mut state,
            CardId(13),
            PlayerId(0),
            "Refused token".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&a).unwrap().is_token = true;

        let approved = |id: ObjectId| {
            let mut event = approved_library_event(&state, id, LibraryPosition::Bottom);
            if let ProposedEvent::ZoneChange {
                from,
                library_placement,
                ..
            } = &mut event
            {
                *from = state.objects[&id].zone;
                *library_placement = Some(LibraryPosition::Bottom);
            }
            event
        };
        let members = vec![
            staged_member(
                &state,
                a,
                Some(approved(a)),
                StagedBatchMemberState::Approved,
                Zone::Exile,
                None,
            ),
            staged_member(
                &state,
                b,
                Some(approved(b)),
                StagedBatchMemberState::Approved,
                Zone::Library,
                None,
            ),
            staged_member(
                &state,
                c,
                Some(approved(c)),
                StagedBatchMemberState::Approved,
                Zone::Library,
                None,
            ),
        ];
        let group_id = push_approved_staged(&mut state, members, BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group_id);
        {
            let staged = owner.staged.as_mut().unwrap();
            staged.stage = BatchDeliveryStage::Delivering;
            staged.cursor = 0;
            staged.arrangements = vec![StagedLibraryArrangement {
                position: LibraryPosition::Bottom,
                batch: MassLibraryOrderBatch {
                    owner: PlayerId(0),
                    arranger: None,
                    members: staged
                        .members
                        .iter()
                        .map(|member| library_order_member(member.identity, Zone::Library))
                        .collect(),
                },
                status: StagedArrangementStatus::Derived,
            }];
        }
        let mut events = Vec::new();
        assert_eq!(
            deliver_staged_members(&mut state, &mut owner, &mut events),
            StagedStop::Delivered
        );
        let staged = owner.staged.as_ref().unwrap();
        assert_eq!(
            staged.members[0].terminal,
            Some(ZoneMoveCompletion::Remained),
            "a refused token is Remained and not inserted"
        );
        assert_eq!(state.objects[&a].zone, Zone::Exile);
        let library: Vec<ObjectId> = state.players[0].library.iter().copied().collect();
        assert_eq!(
            library,
            [sentinel, b, c],
            "the later insertions keep their submitted order at the tail"
        );
    }

    #[test]
    fn staged_approval_owner_requires_the_exact_parked_boundary() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let identity = ObjectIncarnationRef::from_object(&state.objects[&a]);
        let prepared = ProposedEvent::zone_change(a, Zone::Graveyard, Zone::Library, None);
        let mut changed = prepared.clone();
        if let ProposedEvent::ZoneChange { to, .. } = &mut changed {
            *to = Zone::Exile;
        }
        let member = staged_member(
            &state,
            a,
            Some(prepared.clone()),
            StagedBatchMemberState::Unconsulted,
            Zone::Graveyard,
            None,
        );
        let group_id =
            push_approved_staged(&mut state, vec![member], BatchDeliveryStage::Approving);
        let parked = PendingZoneChangeDelivery::new(identity, changed);
        let mut owner = take_owner(&mut state, group_id);
        owner.paused_current = Some(parked.clone());
        put_owner(&mut state, owner);
        assert!(staged_approval_owner(&state, &parked).is_some());
        let mut owner = take_owner(&mut state, group_id);
        owner.paused_current = Some(PendingZoneChangeDelivery::new(identity, prepared));
        put_owner(&mut state, owner);
        assert!(
            staged_approval_owner(&state, &parked).is_none(),
            "a different parked proposal cannot be consumed as this member's approval"
        );
    }
    #[test]
    fn approving_drain_retains_a_live_child_then_finalizes() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let mut member = staged_member(
            &state,
            a,
            None,
            StagedBatchMemberState::PreventedWithChild,
            Zone::Graveyard,
            None,
        );
        member.pending_outcome = Some(StagedMemberOutcome::Prevented);
        let group_id =
            push_approved_staged(&mut state, vec![member], BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group_id);
        owner.staged.as_mut().unwrap().cursor = 0;
        put_owner(&mut state, owner);
        state.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        let mut events = Vec::new();
        drain_pending_batch_deliveries(&mut state, &mut events);
        let owner = state
            .active_batch_delivery()
            .expect("the batch stays parked");
        let staged = owner.staged.as_ref().unwrap();
        assert_eq!(
            staged.members[0].state,
            StagedBatchMemberState::PreventedWithChild,
            "a live child is not finalized early"
        );
        assert!(staged.members[0].pending_outcome.is_some());

        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        drain_pending_batch_deliveries(&mut state, &mut events);
        assert!(
            state.active_batch_delivery().is_none(),
            "the settled child lets the batch finish"
        );
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn staged_repark_preserves_carried_completion_and_deferred_events() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let group_id = replacement_origin_group(&mut state, &[a, b], LibraryPosition::Bottom);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
        owner.completion = Some(BatchCompletion::ReturnAsAuraNoTargetComplete { source_id: a });
        owner.attempted = vec![a, b];
        owner.zone_change_record_start = 7;
        owner.deferred_events = vec![GameEvent::ManaAdded {
            player_id: PlayerId(0),
            mana_type: crate::types::mana::ManaType::Colorless,
            source_id: a,
            tap_state: crate::types::events::ManaTapState::from_tap(false),
        }];
        put_owner(&mut state, owner);
        let mut events = Vec::new();
        drain_pending_batch_deliveries(&mut state, &mut events);
        let owner = state.active_batch_delivery().expect("arranging re-parks");
        assert_eq!(
            owner.completion,
            Some(BatchCompletion::ReturnAsAuraNoTargetComplete { source_id: a })
        );
        assert_eq!(owner.attempted, vec![a, b]);
        assert_eq!(owner.zone_change_record_start, 7);
        assert_eq!(owner.deferred_events.len(), 1);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::DigRestSplitChoice { .. }
        ));
    }

    /// CR 608.2c + CR 608.2d: settlement may itself expose the completion's
    /// ordering choice; only that answer may resume the shared epilogue.
    #[test]
    fn staged_settle_with_live_completion_prompt_returns_parked() {
        // Infrastructure exception: Abundance is the printed analogue for the
        // RevealRestPile completion. Its kept Library→Hand move is never
        // admitted by production staging; this synthetic owner isolates the
        // live completion prompt's handback.
        let mut state = GameState::new_two_player(0x326);
        let source = effect_source(&mut state);
        let kept = library_card(&mut state, 1);
        let misses = vec![library_card(&mut state, 2), library_card(&mut state, 3)];
        let member = staged_member(
            &state,
            kept,
            None,
            StagedBatchMemberState::InnerConsulting,
            Zone::Hand,
            None,
        );
        let group_id =
            push_approved_staged(&mut state, vec![member], BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group_id);
        owner.completion = Some(BatchCompletion::RevealRestPile {
            delivery_stage: DigDeliveryStage::Rest,
            player: PlayerId(0),
            source_id: Some(source),
            rest_cards: misses.clone(),
            rest_destination: Zone::Library,
            rest_order: DigRestOrder::PlayerChoice,
            rest_split_top_count: None,
            clear_markers: misses.clone(),
            publish_tracked_set: None,
            publish_tracked_set_cause: None,
            emit_reveal_until_resolved: None,
            reveal_until_hit_snapshot: None,
            manifested_for_continuation: None,
            kept_delivery: Default::default(),
            continuation_targets: Vec::new(),
            rest_delivery: Default::default(),
        });
        put_owner(&mut state, owner);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let handback = record_staged_batch_consultation_result(
            &mut state,
            group_id,
            StagedMemberOutcome::Approved(ProposedEvent::zone_change(
                kept,
                Zone::Library,
                Zone::Hand,
                Some(source),
            )),
            &mut Vec::new(),
        );
        let StagedConsultationHandback::Parked(waiting) = handback else {
            panic!("settlement must preserve the live completion prompt");
        };
        let WaitingFor::RevealUntilBottomOrder { player, cards, .. } = &*waiting else {
            panic!("expected remainder ordering, got {waiting:?}");
        };
        assert_eq!(*player, PlayerId(0));
        assert_eq!(cards, &misses);
        assert_eq!(state.waiting_for, *waiting);
        assert_eq!(state.objects[&kept].zone, Zone::Hand);
        assert!(misses
            .iter()
            .all(|id| state.objects[id].zone == Zone::Library));
        assert!(state.active_batch_delivery().is_none());
    }

    #[test]
    fn staged_completion_runs_exactly_once() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        library_card(&mut state, 10);
        let a = graveyard_card(&mut state, 1);
        let members = vec![staged_member(
            &state,
            a,
            Some(approved_library_event(&state, a, LibraryPosition::Bottom)),
            StagedBatchMemberState::Approved,
            Zone::Graveyard,
            None,
        )];
        let _group_id = push_approved_staged(&mut state, members, BatchDeliveryStage::Delivering);
        {
            let owner = state
                .resolution_stack
                .active_batch_delivery_mut()
                .expect("staged owner");
            owner.staged.as_mut().unwrap().cursor = 0;
            owner.completion = Some(BatchCompletion::ReturnAsAuraNoTargetComplete { source_id: a });
        }
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let mut events = Vec::new();
        drain_pending_batch_deliveries(&mut state, &mut events);
        assert!(state.active_batch_delivery().is_none(), "settled once");
        let completions = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved { source_id, .. } if *source_id == a
                )
            })
            .count();
        assert_eq!(completions, 1, "the completion runs exactly once");
        drain_pending_batch_deliveries(&mut state, &mut events);
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        GameEvent::EffectResolved { source_id, .. } if *source_id == a
                    )
                })
                .count(),
            1,
            "a second drain cannot re-run a settled completion"
        );
        assert!(state.active_batch_delivery().is_none());
    }

    #[test]
    fn staged_approving_pause_round_trips() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let prepared = ProposedEvent::zone_change(a, Zone::Graveyard, Zone::Library, None);
        let member = staged_member(
            &state,
            a,
            None,
            StagedBatchMemberState::InnerConsulting,
            Zone::Graveyard,
            None,
        );
        let group_id =
            push_approved_staged(&mut state, vec![member], BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group_id);
        let identity = owner.staged.as_ref().unwrap().members[0].identity;
        owner.paused_current = Some(PendingZoneChangeDelivery::new(identity, prepared));
        put_owner(&mut state, owner);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let before = state.active_batch_delivery().unwrap().staged.clone();
        let wire = serde_json::to_value(&state).expect("paused staged state serializes");
        let restored: GameState =
            serde_json::from_value(wire).expect("paused staged state decodes");
        assert_eq!(
            restored.active_batch_delivery().unwrap().staged,
            before,
            "an Approving pause round-trips its cursor members and boundary"
        );
    }

    fn run_permutation(permutation: [bool; 2]) -> (GameState, GameState, Vec<GameEvent>) {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        library_card(&mut state, 10);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let group_id = replacement_origin_group(&mut state, &[a, b], LibraryPosition::Top);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
        put_owner(&mut state, owner);
        let (offered, top_count) = arrangement_prompt(&state);
        let submitted = if permutation[0] {
            vec![offered[1], offered[0]]
        } else {
            offered.clone()
        };
        let before = state.clone();
        let mut events = Vec::new();
        submit_staged_batch_arrangement(
            &mut state,
            group_id,
            PlayerId(0),
            &offered,
            top_count,
            DigRestSplitScope::OrderOnly,
            &submitted,
            &mut events,
        )
        .unwrap();
        (before, state, events)
    }

    #[test]
    fn staged_delivery_public_events_are_permutation_independent() {
        let first = run_permutation([false, false]);
        let second = run_permutation([true, false]);
        assert_eq!(
            first.2, second.2,
            "the public occurrence order cannot reveal the private rank order"
        );
        for viewer in [PlayerId(0), PlayerId(1)] {
            assert_eq!(
                crate::game::visibility::filter_events_for_viewer(&first.2, &first.1, viewer),
                crate::game::visibility::filter_events_for_viewer(&second.2, &second.1, viewer),
                "viewer {viewer:?} receives identical filtered events for both permutations"
            );
            assert_eq!(
                crate::game::log::resolve_log_entries(&first.2, &first.0, &first.1),
                crate::game::log::resolve_log_entries(&second.2, &second.0, &second.1),
                "the public log is identical for both permutations"
            );
        }
    }

    #[test]
    fn delivery_stage_pause_retains_the_member_slice_and_terminal() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let creature = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Delivered then paused".to_string(),
            Zone::Battlefield,
        );
        let event = ProposedEvent::zone_change(creature, Zone::Battlefield, Zone::Graveyard, None);
        let member = staged_member(
            &state,
            creature,
            Some(event.clone()),
            StagedBatchMemberState::Approved,
            Zone::Graveyard,
            None,
        );
        // A second member keeps the batch parked after the reconciliation so
        // the retained occurrence can be inspected.
        let trailing = graveyard_card(&mut state, 21);
        let mut trailing_member = staged_member(
            &state,
            trailing,
            Some(ProposedEvent::zone_change(
                trailing,
                Zone::Graveyard,
                Zone::Graveyard,
                None,
            )),
            StagedBatchMemberState::Unconsulted,
            Zone::Graveyard,
            None,
        );
        trailing_member.announcement_index = 1;
        let group_id = push_approved_staged(
            &mut state,
            vec![member.clone(), trailing_member],
            BatchDeliveryStage::Delivering,
        );
        // The real delivery slice: a direct zone move whose record carries the
        // member's exact incarnation.
        let mut delivered = Vec::new();
        crate::game::zones::move_to_zone(&mut state, creature, Zone::Graveyard, &mut delivered);
        let paused = staged_delivery_pause(&state, group_id, &member, &event, &delivered);
        assert_eq!(
            paused.terminal_completion_after_resume(),
            ZoneMoveCompletion::Moved,
            "the retained slice classifies the pause"
        );
        let mut owner = take_owner(&mut state, group_id);
        owner.staged.as_mut().unwrap().cursor = 0;
        owner.paused_current = Some(paused);
        put_owner(&mut state, owner);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let mut events = Vec::new();
        drain_pending_batch_deliveries(&mut state, &mut events);
        let owner = state
            .active_batch_delivery()
            .expect("the trailing unconsulted member reprieves the batch");
        let staged = owner.staged.as_ref().unwrap();
        assert_eq!(
            staged.members[0].terminal,
            Some(ZoneMoveCompletion::Moved),
            "the resumed member is Moved, not misclassified Remained"
        );
        assert_eq!(staged.cursor, 1);
        assert!(
            owner
                .logical_zone_change_group
                .all_origin_occurrences
                .iter()
                .any(|occurrence| matches!(
                    occurrence.event,
                    GameEvent::ZoneChanged { object_id, .. } if object_id == creature
                )),
            "the paused member's occurrence enters the batch's retained list"
        );
    }

    #[test]
    fn staged_discard_choice_child_reaches_the_drain_through_resume_frames() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let mut member = staged_member(
            &state,
            a,
            None,
            StagedBatchMemberState::PreventedWithChild,
            Zone::Graveyard,
            None,
        );
        member.pending_outcome = Some(StagedMemberOutcome::Prevented);
        let group_id =
            push_approved_staged(&mut state, vec![member], BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group_id);
        owner.staged.as_mut().unwrap().cursor = 0;
        put_owner(&mut state, owner);
        state.waiting_for = WaitingFor::DiscardChoice {
            player: PlayerId(0),
            count: 1,
            cards: vec![a],
            source_id: a,
            effect_kind: EffectKind::Discard,
            up_to: false,
            unless_filter: None,
            discard_frame: None,
        };
        let mut events = Vec::new();
        crate::game::effects::resume_resolution_frames(&mut state, &mut events);
        let staged = state
            .active_batch_delivery()
            .expect("a live child keeps the batch parked")
            .staged
            .as_ref()
            .unwrap();
        assert_eq!(
            staged.members[0].state,
            StagedBatchMemberState::PreventedWithChild,
            "a live DiscardChoice child is not finalized"
        );
        assert!(staged.members[0].pending_outcome.is_some());

        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        crate::game::effects::resume_resolution_frames(&mut state, &mut events);
        assert!(
            state.active_batch_delivery().is_none(),
            "the answered child reaches the drain through resume_resolution_frames"
        );
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn staged_approval_defers_a_nested_replacement_result() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let identity = ObjectIncarnationRef::from_object(&state.objects[&a]);
        let event = ProposedEvent::zone_change(a, Zone::Graveyard, Zone::Library, None);
        let member = staged_member(
            &state,
            a,
            None,
            StagedBatchMemberState::InnerConsulting,
            Zone::Graveyard,
            None,
        );
        let group_id =
            push_approved_staged(&mut state, vec![member], BatchDeliveryStage::Approving);
        let parked = PendingZoneChangeDelivery::new(identity, event.clone());
        let mut owner = take_owner(&mut state, group_id);
        owner.staged.as_mut().unwrap().cursor = 0;
        owner.staged.as_mut().unwrap().members[0].pending_outcome =
            Some(StagedMemberOutcome::Prevented);
        owner.paused_current = Some(parked.clone());
        put_owner(&mut state, owner);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        assert_eq!(
            staged_approval_owner(&state, &parked).map(|(_, fresh)| fresh),
            Some(false),
            "a retained outcome defers the nested result"
        );
        let mut events = Vec::new();
        let _ = record_staged_batch_consultation_result(
            &mut state,
            group_id,
            StagedMemberOutcome::Approved(event),
            &mut events,
        );
        let staged = state
            .active_batch_delivery()
            .expect("the batch stays parked")
            .staged
            .as_ref()
            .unwrap();
        assert_eq!(
            staged.members[0].pending_outcome,
            Some(StagedMemberOutcome::Prevented),
            "the child's retained outcome is never overwritten"
        );
    }

    #[test]
    fn staged_arranging_pause_defers_occurrences_until_delivery() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        library_card(&mut state, 10);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let group_id = replacement_origin_group(&mut state, &[a, b], LibraryPosition::Top);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
        put_owner(&mut state, owner);
        let (offered, top_count) = arrangement_prompt(&state);

        assert!(
            state
                .active_batch_delivery()
                .unwrap()
                .logical_zone_change_group
                .all_origin_occurrences
                .is_empty(),
            "an Arranging pause appends no occurrence"
        );
        assert!(state.consumed_before_priority_trigger_events.is_empty());
        let mut events = Vec::new();
        drain_pending_batch_deliveries(&mut state, &mut events);
        assert!(
            state
                .active_batch_delivery()
                .unwrap()
                .logical_zone_change_group
                .all_origin_occurrences
                .is_empty(),
            "re-parking the Arranging prompt appends nothing"
        );

        submit_staged_batch_arrangement(
            &mut state,
            group_id,
            PlayerId(0),
            &offered,
            top_count,
            DigRestSplitScope::OrderOnly,
            &[offered[1], offered[0]],
            &mut events,
        )
        .expect("a full permutation is legal");
        assert!(state.active_batch_delivery().is_none());
        let delivered: Vec<ObjectId> = state
            .consumed_before_priority_trigger_events
            .iter()
            .filter_map(|occurrence| match &occurrence.event {
                GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            delivered,
            [a, b],
            "after delivery the group's occurrences are announcement order"
        );
    }

    #[test]
    fn completion_and_battlefield_terminal_are_recorded_before_collection() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let creature = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Prevented permanent".to_string(),
            Zone::Battlefield,
        );
        let mut member = staged_member(
            &state,
            creature,
            None,
            StagedBatchMemberState::Terminal,
            Zone::Graveyard,
            None,
        );
        member.terminal = Some(ZoneMoveCompletion::Prevented);
        let _group_id =
            push_approved_staged(&mut state, vec![member], BatchDeliveryStage::Delivering);
        {
            let owner = state
                .resolution_stack
                .active_batch_delivery_mut()
                .expect("staged owner");
            owner.completion = Some(BatchCompletion::ReturnAsAuraNoTargetComplete {
                source_id: creature,
            });
        }
        // The sweep alone records the Pending battlefield slot before the
        // complete collection runs.
        let mut probe = state.active_batch_delivery().unwrap().clone();
        record_staged_terminals_for_settlement(&mut probe);
        assert_eq!(
            probe.logical_zone_change_group.terminal_outcomes[0],
            crate::types::game_state::LogicalZoneChangeTerminalOutcome::Prevented
        );

        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let mut events = Vec::new();
        drain_pending_batch_deliveries(&mut state, &mut events);
        assert!(state.active_batch_delivery().is_none());
        let completions = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved { source_id, .. } if *source_id == creature
                )
            })
            .count();
        assert_eq!(completions, 1, "the completion runs exactly once");
    }

    #[test]
    fn drain_paused_reconciliation_dedupes_a_prevented_terminal() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let creature = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Prevented pause".to_string(),
            Zone::Battlefield,
        );
        let member = staged_member(
            &state,
            creature,
            None,
            StagedBatchMemberState::Approved,
            Zone::Graveyard,
            None,
        );
        let identity = member.identity;
        let _group_id =
            push_approved_staged(&mut state, vec![member], BatchDeliveryStage::Delivering);
        {
            let owner = state
                .resolution_stack
                .active_batch_delivery_mut()
                .expect("staged owner");
            owner.staged.as_mut().unwrap().cursor = 0;
            owner.paused_current = Some(PendingZoneChangeDelivery {
                terminal_completion: Some(ZoneMoveCompletion::Prevented),
                ..PendingZoneChangeDelivery::new(
                    identity,
                    ProposedEvent::zone_change(creature, Zone::Battlefield, Zone::Graveyard, None),
                )
            });
            owner.completion = Some(BatchCompletion::ReturnAsAuraNoTargetComplete {
                source_id: creature,
            });
        }
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let mut events = Vec::new();
        drain_pending_batch_deliveries(&mut state, &mut events);
        assert!(
            state.active_batch_delivery().is_none(),
            "the pause reconciliation and settlement both complete"
        );
        let completions = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved { source_id, .. } if *source_id == creature
                )
            })
            .count();
        assert_eq!(completions, 1, "the deduped sweep does not panic or rerun");
    }

    #[test]
    fn bottom_arrangement_submission_sets_submitted_then_delivers() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let p = library_card(&mut state, 10);
        let top_a = graveyard_card(&mut state, 1);
        let top_b = graveyard_card(&mut state, 2);
        let bottom_a = graveyard_card(&mut state, 3);
        let bottom_b = graveyard_card(&mut state, 4);
        let members = [top_a, top_b]
            .into_iter()
            .map(|id| {
                staged_member(
                    &state,
                    id,
                    Some(approved_library_event(&state, id, LibraryPosition::Top)),
                    StagedBatchMemberState::Approved,
                    Zone::Graveyard,
                    None,
                )
            })
            .chain([bottom_a, bottom_b].into_iter().map(|id| {
                staged_member(
                    &state,
                    id,
                    Some(approved_library_event(&state, id, LibraryPosition::Bottom)),
                    StagedBatchMemberState::Approved,
                    Zone::Graveyard,
                    None,
                )
            }))
            .collect();
        let group_id = push_approved_staged(&mut state, members, BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
        put_owner(&mut state, owner);
        let (top_cards, top_count) = arrangement_prompt(&state);
        let mut events = Vec::new();
        submit_staged_batch_arrangement(
            &mut state,
            group_id,
            PlayerId(0),
            &top_cards,
            top_count,
            DigRestSplitScope::OrderOnly,
            &[top_cards[1], top_cards[0]],
            &mut events,
        )
        .expect("top permutation");
        {
            let staged = state
                .active_batch_delivery()
                .expect("the bottom prompt keeps the batch parked")
                .staged
                .as_ref()
                .unwrap();
            assert_eq!(
                staged.arrangements[0].status,
                StagedArrangementStatus::Submitted,
                "the answered group is final"
            );
            assert_eq!(
                staged.arrangements[0]
                    .batch
                    .members
                    .iter()
                    .map(|member| member.identity.object_id)
                    .collect::<Vec<_>>(),
                [top_cards[1], top_cards[0]]
            );
        }
        let (bottom_cards, bottom_top_count) = arrangement_prompt(&state);
        assert_eq!(bottom_top_count, 0, "a Bottom group is all bottom");
        submit_staged_batch_arrangement(
            &mut state,
            group_id,
            PlayerId(0),
            &bottom_cards,
            bottom_top_count,
            DigRestSplitScope::OrderOnly,
            &[bottom_cards[1], bottom_cards[0]],
            &mut events,
        )
        .expect("bottom permutation");
        assert!(state.active_batch_delivery().is_none());
        let library: Vec<ObjectId> = state.players[0].library.iter().copied().collect();
        assert_eq!(
            library,
            [
                top_cards[1],
                top_cards[0],
                p,
                bottom_cards[1],
                bottom_cards[0]
            ]
        );
    }

    #[test]
    fn legacy_staged_absent_batch_resumes_the_old_path() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let milled = library_card(&mut state, 10);
        let request = staged_request(&state, milled, Zone::Graveyard, None);
        let group =
            crate::game::triggers::allocate_logical_zone_change_group(&mut state, &[milled]);
        state.push_batch_delivery(PendingBatchDeliveries {
            component_delivery_parent: None,
            logical_zone_change_group: group,
            paused_current: None,
            remaining: vec![milled],
            destination: Zone::Graveyard,
            source_id: None,
            enter_tapped: EtbTapState::Unspecified,
            exile_tracking: ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: None,
            replacement_applied: HashSet::new(),
            requests: vec![request],
            attempted: vec![milled],
            zone_change_record_start: state.zone_changes_this_turn.len(),
            deferred_events: Vec::new(),
            staged: None,
        });
        let mut events = Vec::new();
        drain_pending_batch_deliveries(&mut state, &mut events);
        assert!(state.active_batch_delivery().is_none());
        assert_eq!(state.objects[&milled].zone, Zone::Graveyard);
    }

    #[test]
    fn stale_incarnation_and_unprompted_submissions_reject() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let b = graveyard_card(&mut state, 2);
        let group_id = replacement_origin_group(&mut state, &[a, b], LibraryPosition::Bottom);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_some());
        put_owner(&mut state, owner);
        let (offered, top_count) = arrangement_prompt(&state);
        let mut events = Vec::new();
        // Stale incarnation: the offered object moves before the answer.
        crate::game::zones::move_to_zone(&mut state, b, Zone::Exile, &mut events);
        assert!(submit_staged_batch_arrangement(
            &mut state,
            group_id,
            PlayerId(0),
            &offered,
            top_count,
            DigRestSplitScope::OrderOnly,
            &offered,
            &mut events,
        )
        .is_err());
        assert!(state.active_batch_delivery().is_some());
        // A delivered-stage batch is not taking arrangement submissions.
        {
            let owner = state
                .resolution_stack
                .active_batch_delivery_mut()
                .expect("staged owner");
            owner.staged.as_mut().unwrap().stage = BatchDeliveryStage::Delivering;
        }
        assert!(submit_staged_batch_arrangement(
            &mut state,
            group_id,
            PlayerId(0),
            &offered,
            top_count,
            DigRestSplitScope::OrderOnly,
            &offered,
            &mut events,
        )
        .is_err());
    }

    #[test]
    fn zero_group_batch_prompts_nothing() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        let a = graveyard_card(&mut state, 1);
        let member = staged_member(
            &state,
            a,
            Some(ProposedEvent::zone_change(
                a,
                Zone::Graveyard,
                Zone::Graveyard,
                None,
            )),
            StagedBatchMemberState::Approved,
            Zone::Graveyard,
            None,
        );
        let group_id =
            push_approved_staged(&mut state, vec![member], BatchDeliveryStage::Approving);
        let mut owner = take_owner(&mut state, group_id);
        begin_staged_arrangement(&state, &mut owner);
        assert!(owner.staged.as_ref().unwrap().arrangements.is_empty());
        assert_eq!(
            owner.staged.as_ref().unwrap().stage,
            BatchDeliveryStage::Delivering
        );
        assert!(publish_next_staged_arrangement(&mut state, &mut owner).is_none());
        put_owner(&mut state, owner);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn settlement_records_prevented_terminals_once_and_dedupes_a_paused_terminal() {
        let mut state = GameState::new_two_player(42);
        effect_source(&mut state);
        // A battlefield-origin member that is prevented stays on the field.
        let creature = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Prevented creature".to_string(),
            Zone::Battlefield,
        );
        let moved = graveyard_card(&mut state, 2);
        let mut prevented = staged_member(
            &state,
            creature,
            None,
            StagedBatchMemberState::Terminal,
            Zone::Graveyard,
            None,
        );
        prevented.terminal = Some(ZoneMoveCompletion::Prevented);
        let mut delivered = staged_member(
            &state,
            moved,
            Some(approved_library_event(
                &state,
                moved,
                LibraryPosition::Bottom,
            )),
            StagedBatchMemberState::Terminal,
            Zone::Graveyard,
            None,
        );
        delivered.terminal = Some(ZoneMoveCompletion::Moved);
        let _group_id = push_approved_staged(
            &mut state,
            vec![prevented, delivered],
            BatchDeliveryStage::Delivering,
        );
        {
            let owner = state
                .resolution_stack
                .active_batch_delivery_mut()
                .expect("staged owner");
            let group = &mut owner.logical_zone_change_group;
            group
                .record_delivery_completion(creature, ZoneMoveCompletion::Prevented)
                .expect("first recording");
            group
                .record_delivery_completion(moved, ZoneMoveCompletion::Remained)
                .expect("paused terminal recorded before settlement");
        }
        let pending = state
            .resolution_stack
            .take_active_batch_delivery()
            .unwrap()
            .unwrap();
        let mut events = Vec::new();
        settle_batch_owner(&mut state, pending, Vec::new(), &mut events);
        assert!(state.active_batch_delivery().is_none(), "the owner retires");
    }

    /// D10: the staged Destroy terminal mapping. `CreatureDestroyed` is emitted
    /// exactly once for an outer-approved member with a non-abandoned terminal
    /// (Moved, Remained, inner Prevented) and never for an outer redirect,
    /// outer prevention, pre-proposal guard or abandoned member; settlement
    /// publishes the same set as `settled_destroyed`.
    #[test]
    fn d10_staged_destroy_terminal_mapping_and_creature_destroyed() {
        fn battlefield_object(state: &mut GameState, card: u64, name: &str) -> ObjectId {
            create_object(
                state,
                CardId(card),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            )
        }

        fn destroyed_count(events: &[GameEvent], id: ObjectId) -> usize {
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        GameEvent::CreatureDestroyed { object_id, .. } if *object_id == id
                    )
                })
                .count()
        }

        let mut state = GameState::new_two_player(42);
        let source = effect_source(&mut state);
        let outer_redirect = battlefield_object(&mut state, 701, "Outer redirect");
        let outer_prevented = battlefield_object(&mut state, 702, "Outer prevented");
        let inner_prevented = battlefield_object(&mut state, 703, "Inner prevented");
        let moved = battlefield_object(&mut state, 704, "Moved");
        let remained = battlefield_object(&mut state, 705, "Remained");
        let guarded = battlefield_object(&mut state, 706, "Guarded");
        let abandoned = battlefield_object(&mut state, 707, "Abandoned");
        let announced = [
            outer_redirect,
            outer_prevented,
            inner_prevented,
            moved,
            remained,
            guarded,
            abandoned,
        ];
        let requests: Vec<ZoneMoveRequest> = announced
            .iter()
            .map(|id| ZoneMoveRequest::effect(*id, Zone::Graveyard, source))
            .collect();
        let group =
            crate::game::triggers::allocate_logical_zone_change_group(&mut state, &announced);
        let mut staged = new_staged_batch(&state, &requests, Vec::new());
        for member in &mut staged.members {
            member.operation = StagedMemberOperation::Destroy(StagedDestroyOperation {
                source,
                cant_regenerate: false,
                outer: StagedDestroyOuterApproval::Unapproved,
            });
        }
        let mut owner = new_staged_owner(&state, &requests, group, staged, None);
        let mut events = Vec::new();

        // CR 614.6: the outer Destroy replacement redirected straight to a
        // zone change. It is the final event and never sets the approval flag.
        record_staged_member_approval(
            &mut owner,
            0,
            ProposedEvent::zone_change(
                outer_redirect,
                Zone::Battlefield,
                Zone::Exile,
                Some(source),
            ),
        );
        mark_staged_member_terminal(
            &mut state,
            &mut owner,
            0,
            ZoneMoveCompletion::Moved,
            &mut events,
        );

        // Outer Prevented (regeneration / shield counter / Umbra): terminal
        // Prevented, no approval, no `CreatureDestroyed`.
        mark_staged_member_terminal(
            &mut state,
            &mut owner,
            1,
            ZoneMoveCompletion::Prevented,
            &mut events,
        );

        // Inner Prevented: the outer Destroy was approved, so the singleton
        // rule still emits one `CreatureDestroyed` (F16 preserved).
        approve_staged_destroy_outer(&mut owner, 2);
        mark_staged_member_terminal(
            &mut state,
            &mut owner,
            2,
            ZoneMoveCompletion::Prevented,
            &mut events,
        );

        // Inner Execute redirected back to the battlefield: terminal Remained,
        // but the outer approval still emits one `CreatureDestroyed`.
        approve_staged_destroy_outer(&mut owner, 3);
        mark_staged_member_terminal(
            &mut state,
            &mut owner,
            3,
            ZoneMoveCompletion::Moved,
            &mut events,
        );
        approve_staged_destroy_outer(&mut owner, 4);
        mark_staged_member_terminal(
            &mut state,
            &mut owner,
            4,
            ZoneMoveCompletion::Remained,
            &mut events,
        );

        // Guard Remained: no proposal was ever made, so no event.
        mark_staged_member_terminal(
            &mut state,
            &mut owner,
            5,
            ZoneMoveCompletion::Remained,
            &mut events,
        );

        // An abandoned member is excluded even when the outer was approved.
        approve_staged_destroy_outer(&mut owner, 6);
        {
            let member = &mut owner.staged.as_mut().unwrap().members[6];
            member.terminal = Some(ZoneMoveCompletion::Moved);
            member.state = StagedBatchMemberState::Abandoned;
        }

        for id in [outer_redirect, outer_prevented, guarded, abandoned] {
            assert_eq!(
                destroyed_count(&events, id),
                0,
                "no CreatureDestroyed for a non-approved or abandoned terminal"
            );
        }
        for id in [inner_prevented, moved, remained] {
            assert_eq!(
                destroyed_count(&events, id),
                1,
                "exactly one CreatureDestroyed for an outer-approved non-abandoned terminal"
            );
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CreatureDestroyed { .. }))
                .count(),
            3
        );

        let members = owner.staged.as_ref().unwrap().members.clone();
        let mut completion = BatchCompletion::DestroyPopulationSettled {
            ability: Box::new(ResolvedAbility::new(
                Effect::DestroyAll {
                    target: TargetFilter::None,
                    cant_regenerate: false,
                },
                vec![],
                source,
                PlayerId(0),
            )),
            settled_destroyed: None,
        };
        crate::types::game_state::settle_batch_delivery_outcome(
            &mut completion,
            &state,
            &owner.logical_zone_change_group,
            &members,
        );
        let BatchCompletion::DestroyPopulationSettled {
            settled_destroyed, ..
        } = completion
        else {
            panic!("destroy completion retains its variant");
        };
        assert_eq!(
            settled_destroyed.expect("settlement fills the payload"),
            vec![inner_prevented, moved, remained],
            "settled_destroyed is the same outer-approved, non-abandoned set"
        );

        // The pre-proposal guard: an indestructible member is `Remained` before
        // the outer `Destroy` is ever proposed.
        let mut guard_state = GameState::new_two_player(42);
        let guard_source = effect_source(&mut guard_state);
        let titan = battlefield_object(&mut guard_state, 708, "Adamant Titan");
        guard_state
            .objects
            .get_mut(&titan)
            .unwrap()
            .keywords
            .push(Keyword::Indestructible);
        let guard_requests = vec![ZoneMoveRequest::effect(
            titan,
            Zone::Graveyard,
            guard_source,
        )];
        let guard_group =
            crate::game::triggers::allocate_logical_zone_change_group(&mut guard_state, &[titan]);
        let mut guard_staged = new_staged_batch(&guard_state, &guard_requests, Vec::new());
        let operation = StagedDestroyOperation {
            source: guard_source,
            cant_regenerate: false,
            outer: StagedDestroyOuterApproval::Unapproved,
        };
        guard_staged.members[0].operation = StagedMemberOperation::Destroy(operation);
        let mut guard_owner = new_staged_owner(
            &guard_state,
            &guard_requests,
            guard_group,
            guard_staged,
            None,
        );
        let guard_member = guard_owner.staged.as_ref().unwrap().members[0].clone();
        let mut guard_events = Vec::new();
        assert_eq!(
            consult_staged_destroy_outer(
                &mut guard_state,
                &mut guard_owner,
                &mut guard_events,
                guard_member,
                operation,
            ),
            StagedConsult::Continue
        );
        let guard_member = &guard_owner.staged.as_ref().unwrap().members[0];
        assert_eq!(
            guard_member.terminal,
            Some(ZoneMoveCompletion::Remained),
            "the guard is a Remained terminal, not a destruction"
        );
        assert!(matches!(
            guard_member.operation,
            StagedMemberOperation::Destroy(StagedDestroyOperation {
                outer: StagedDestroyOuterApproval::Unapproved,
                ..
            })
        ));
        assert!(guard_events.is_empty());
    }
}
