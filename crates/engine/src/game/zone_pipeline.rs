//! Unified zone-change pipeline.
//!
//! Layer discipline (PLAN §2): `zones.rs` keeps every guard that must hold
//! unconditionally (CR 111.8 token guard, CR 614.1d ETB block, CR 400.7 cleanup,
//! `GameEvent::ZoneChanged` emission); this module owns the "would"-semantics
//! layer (CR 614.1 / 614.6 replacement consult, CR 616.1 choices, CR 614.1c
//! enters-with seeding) plus the CR 303.4f aura-host choice.

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    AdditionalCostInstancePayment, CastTimingPermission, CostPaidObjectSnapshot, Duration, Effect,
    EffectKind, KickerVariant, LibraryPosition, ResolvedAbility, StaticDefinition, TargetFilter,
    TargetRef,
};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    BatchCompletion, EnteringAuraAuthority, ExileLinkKind, GameState, LiminalEntryKind,
    LogicalZoneChangeGroup, MergedCardComponentRoute, PendingBatchDeliveries,
    PendingBatchZoneChangeCause, PendingBatchZoneMoveRequest, PendingCounterPostAction,
    PendingLiminalEntryResume, PendingZoneChangeDelivery, PostReplacementDrainOwner, WaitingFor,
    ZoneDeliveryExileTracking, ZoneMoveCompletion,
};
use std::collections::HashSet;

use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{AppliedReplacementKey, ProposedEvent};
use crate::types::zones::{EtbTapState, Zone};

use crate::game::effects::change_zone::shuffle_library;
use crate::game::game_object::{AttachTarget, GameObject};
use crate::types::ability::FaceDownProfile;

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
    /// - `DebugCommand`: operator intent is "force the state".
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
}

/// Exile-link context carried through the delivery tail. Replaces the old
/// `track_exiled_by_source: bool` (no-bool rule): duration-bound links and
/// `exiled_by_source` bookkeeping always travel together, so they fold into one
/// struct that also rides in `DeliveryCtx`.
#[derive(Default)]
pub struct ExileLinkSpec {
    /// `Some(Duration::UntilHostLeavesPlay)` installs a return-on-source-leave
    /// link; other durations / `None` fall back to `tracking`.
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
    pub face_down_in_exile: bool,
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
            library_placement: self.placement,
            exile_duration: self.exile_links.duration,
            exile_controller: self.exile_links.controller,
            exile_tracking: self.exile_links.tracking,
            replacement_applied: self.replacement_applied,
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
            face_down_in_exile: false,
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
            face_down_in_exile: false,
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
            face_down_in_exile: false,
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
            face_down_in_exile: false,
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
    /// (CR 614.5, PLAN Risk #5).
    pub fn draw(object_id: ObjectId, seed_applied: HashSet<AppliedReplacementKey>) -> Self {
        Self {
            object_id,
            to: Zone::Hand,
            cause: ZoneChangeCause::Draw {
                seed_applied: seed_applied.clone(),
            },
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: seed_applied,
            face_down_in_exile: false,
        }
    }

    /// CR 601.2a: casting moves the card from where it is to the stack — part
    /// of the casting process, exempt from the replacement consult.
    pub fn casting_to_stack(object_id: ObjectId, source: ObjectId) -> Self {
        Self {
            object_id,
            to: Zone::Stack,
            cause: ZoneChangeCause::CastingToStack { source },
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
            replacement_applied: HashSet::new(),
            face_down_in_exile: false,
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
            face_down_in_exile: false,
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
            face_down_in_exile: false,
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
            face_down_in_exile: false,
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
            face_down_in_exile: false,
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
        self.face_down_in_exile = true;
        self
    }

    /// CR 614.5: seed a child/modified move with the replacements already
    /// applied to its originating event.
    pub fn with_replacement_applied(mut self, applied: HashSet<AppliedReplacementKey>) -> Self {
        self.replacement_applied = applied;
        self
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
    /// CR 701.24a: the library placement to honor when the delivered destination
    /// is the library. Threaded by the W3 resume path
    /// (`handle_replacement_choice`) from the parked `PendingReplacement`;
    /// `None` for every other `deliver` caller (a placement is not a shuffle, so
    /// `None` means the tail's auto-shuffle convention applies).
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

pub(crate) fn move_object_with_terminal(
    state: &mut GameState,
    req: ZoneMoveRequest,
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
    {
        let obj = state
            .objects
            .get(&req.object_id)
            .expect("object exists (zone read above)");
        // CR 111.8: A token that has left the battlefield can't change zones; it
        // remains in place and ceases to exist at the next SBA (CR 111.7). An
        // exact CR 601.2a pending spell plus its announcement placeholder makes
        // the retained-origin representation stack-resident until this delivery.
        if zones::token_is_outside_battlefield_and_stack(state, obj) {
            return ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained);
        }
        // CR 603.2g + CR 603.6a: A Battlefield -> Battlefield move does not put a
        // permanent onto the battlefield — no entry event occurs, so no
        // would-style replacement should be consulted (and the primitive would
        // reject it as a no-op regardless), mirroring the `zones::move_to_zone`
        // no-op guard.
        if from_zone == Zone::Battlefield && req.to == Zone::Battlefield {
            return ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained);
        }
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
                chain_referent,
                ..
            } = &mut proposed
            {
                *chain_referent = req.mods.chain_referent;
                *applied = req.replacement_applied.clone();
            }
            return match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    let delivery_start = events.len();
                    match deliver_replaced_zone_change(
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
                    // CR 701.24a: stash the requested library placement on the
                    // parked record so the resume path
                    // (`engine_replacement::handle_replacement_choice`) threads it
                    // back into the delivery. Without this the resume hardcodes
                    // `library_placement: None` and the delivery tail auto-shuffles,
                    // randomizing the requested position away. Unreachable today (no
                    // pool `Moved` def targets the library, so a placement consult
                    // never reaches a choice), but threaded for correctness — see
                    // the `library_placement_parked_resume_honors_position` unit
                    // test for the synthetic-redirect coverage.
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
            chain_referent,
            ..
        } = &mut proposed
        {
            *chain_referent = req.mods.chain_referent;
            *applied = req.replacement_applied;
            applied.extend(seed_applied);
        }
        return match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                let delivery_start = events.len();
                match deliver_replaced_zone_change(
                    state,
                    event,
                    source_id,
                    exile_links.duration.as_ref(),
                    exile_links.controller,
                    track_exiled_by_source,
                    PostReplacementDrainOwner::DeliveryTail,
                    None,
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
    // uniform with the consulting path. The intrinsic enters-with-counters
    // seeding (CR 614.1c) is part of the "would" layer and is deliberately NOT
    // applied — matching the raw `move_to_zone` behavior these callers replace.
    if req.cause.is_exempt() {
        // DebugCommand is FULLY inert: operator intent is "force the state" for
        // scenario setup, so the delivery tail's battlefield arms must not fire
        // either — CR 614.1c "enters with an additional counter" statics
        // (Kalain class) must not mint counters onto a debug-staged creature,
        // `pending_etb_counters` from delayed triggers must not be consumed,
        // and the CR 614.12a devour snapshot must not be captured. Route
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
            // pod-lab loop-3 Q5: debug-staged board setup (GameScenario, the
            // shared seam nearly every engine integration test builds boards
            // through) stays maximally conservative and out of scope for the
            // move_to_zone incremental-flush carve-out below — there is zero
            // gameplay perf benefit to incrementalizing test/debug setup, and
            // the blast radius of a subtle divergence here is the whole test
            // suite. Unconditional, regardless of what move_to_zone's own
            // (now axis-gated) internal decision would otherwise have been.
            crate::game::layers::mark_layers_full(state);
            return ZoneMoveTerminalResult::Completed(zone_move_completion_from_delivery(
                member,
                &events[delivery_start..],
            ));
        }
        let mut proposed = ProposedEvent::zone_change(req.object_id, from_zone, req.to, source_id);
        if let ProposedEvent::ZoneChange {
            enter_transformed,
            enter_tapped,
            enters_attacking,
            controller_override,
            enter_with_counters,
            face_down_profile,
            chain_referent,
            applied,
            ..
        } = &mut proposed
        {
            *enter_transformed = req.mods.enter_transformed;
            if !req.mods.enter_tapped.is_unspecified() {
                *enter_tapped = req.mods.enter_tapped;
            }
            *enters_attacking = req.mods.enters_attacking;
            *controller_override = req.mods.controller_override;
            enter_with_counters.extend(req.mods.enter_with_counters.iter().cloned());
            *face_down_profile = req.mods.face_down_profile.clone().map(Box::new);
            *chain_referent = req.mods.chain_referent;
            *applied = req.replacement_applied;
        }
        let approved = ApprovedZoneChange::seal(proposed);
        let delivery_start = events.len();
        return match deliver(
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
        req.mods.chain_referent,
        track_exiled_by_source,
        None,
        None,
        exile_links.controller,
        req.replacement_applied,
        events,
    )
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
    let event_start = events.len();
    let zone_change_record_start = state.zone_changes_this_turn.len();
    let ids: Vec<ObjectId> = reqs.iter().map(|r| r.object_id).collect();
    let logical_zone_change_group =
        crate::game::triggers::allocate_logical_zone_change_group(state, &ids);
    let destination = reqs.first().map(|r| r.to);
    match deliver_batch(
        state,
        reqs,
        logical_zone_change_group,
        BatchDeliveryParking::NewChild,
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
            );
            // Synchronous completion (the common single-redirect path): run the
            // cleanup now, and surface a pause it raises to the enclosing caller.
            completion.map_or(BatchMoveResult::Done, |completion| {
                run_batch_completion(state, completion, events)
            })
        }
        BatchDeliveryResult::NeedsChoice => {
            // `deliver_batch` always stashes the exact owner, including when the
            // paused object was the last member and the undelivered tail is empty.
            // Attach the completion to that same owner so its drain can run it
            // once, after logical settlement.
            let pending = ensure_batch_record(state, destination.unwrap_or(Zone::Graveyard));
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
fn deliver_batch(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    mut logical_zone_change_group: LogicalZoneChangeGroup,
    parking: BatchDeliveryParking,
    events: &mut Vec<GameEvent>,
) -> BatchDeliveryResult {
    let segment_start = events.len();
    let mut queue = reqs.into_iter();
    while let Some(req) = queue.next() {
        let destination = req.to;
        let face_down_in_exile = req.face_down_in_exile;
        let anticipated_pause = anticipated_zone_change_delivery(state, &req);
        let delivery_start = events.len();
        let object_id = req.object_id;
        match move_object_with_terminal(state, req, events) {
            ZoneMoveTerminalResult::Completed(completion) => {
                if face_down_in_exile {
                    mark_face_down_if_exiled(state, object_id);
                }
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
                let mut paused_current = state
                    .pending_zone_change_delivery_from_replacement()
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
                // resume path. No batch flow targets a battlefield aura entry
                // today (mill destinations are graveyard/exile/hand; mass bounce
                // returns to hand/library), so this arm is unreachable for the
                // current batch callers; stop and stash the tail so a future
                // battlefield-entry batch does not silently drop its remainder.
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

fn mark_face_down_if_exiled(state: &mut GameState, object_id: ObjectId) {
    if let Some(object) = state
        .objects
        .get_mut(&object_id)
        .filter(|object| object.zone == Zone::Exile)
    {
        object.face_down = true;
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
    };
    match parking {
        BatchDeliveryParking::NewChild => state.push_batch_delivery(pending),
        BatchDeliveryParking::ReparkActive => state
            .replace_active_batch_delivery(pending)
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
        chain_referent,
        attach_to,
        applied,
        ..
    } = &mut expected_event
    {
        *enter_tapped = request.mods.enter_tapped;
        *enters_attacking = request.mods.enters_attacking;
        *enter_transformed = request.mods.enter_transformed;
        *controller_override = request.mods.controller_override;
        *enter_with_counters = request.mods.enter_with_counters.clone();
        *face_down_profile = request.mods.face_down_profile.clone().map(Box::new);
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
    if let Some(pending) = state.active_batch_delivery().cloned() {
        let PendingBatchDeliveries {
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
        if let Some(paused_current) = paused_current {
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
        match deliver_batch(
            state,
            reqs,
            logical_zone_change_group,
            BatchDeliveryParking::ReparkActive,
            events,
        ) {
            BatchDeliveryResult::Done(mut logical_zone_change_group) => {
                crate::game::triggers::complete_logical_zone_trigger_collection(
                    state,
                    &mut logical_zone_change_group,
                    events,
                )
                .expect("completed batch drain owns every terminal zone-change outcome");
                crate::game::triggers::sync_logical_zone_change_departure_stamps(
                    &logical_zone_change_group,
                    &mut deferred_events,
                );
                deferred_events.append(events);
                events.append(&mut deferred_events);
                // This completed owner has already collected every one of its
                // retained ZoneChanged occurrences.  The replacement-resume
                // action still reaches the generic priority scan, so claim the
                // exact occurrences now rather than collecting them a second
                // time through that scan.
                crate::game::triggers::mark_logical_zone_events_consumed_before_priority(
                    state,
                    &logical_zone_change_group,
                    events,
                );
                state
                    .take_active_batch_delivery()
                    .expect("settled batch delivery must own the active frame")
                    .expect("settled batch delivery frame must exist");
                // CR 603.10a + CR 616.1: logical settlement has completed before
                // the one post-batch cleanup can run.
                if let Some(completion) = completion {
                    // The parked/settled result is deliberately unused here: the
                    // drain's callers are state-mediated (engine_replacement
                    // re-reads `state.waiting_for` after the drain and gates
                    // every later drain stage on Priority), so a completion that
                    // parks a new CR 616.1 choice propagates via the parked
                    // prompt + fresh BatchDelivery frame, not via
                    // this return value. Witnessed by the compound double-pause
                    // test (miss batch redirect, then hit-delivery redirect).
                    let _ = run_batch_completion(state, completion, events);
                }
            }
            BatchDeliveryResult::NeedsChoice => {
                // `deliver_batch` already re-parked the exact same owner,
                // including a pause on its final member. Re-attach only the
                // completion and external output; never replace that owner.
                let reparking = ensure_batch_record(state, destination);
                reparking.completion = completion;
                reparking.attempted = attempted;
                reparking.zone_change_record_start = zone_change_record_start;
                deferred_events.append(events);
                reparking.deferred_events = deferred_events;
            }
        }
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
    let track_exiled_by_source = matches!(
        ctx.exile_links.tracking,
        ZoneDeliveryExileTracking::TrackBySource
    );
    deliver_replaced_zone_change(
        state,
        approved.event,
        ctx.source_id,
        ctx.exile_links.duration.as_ref(),
        ctx.exile_links.controller,
        track_exiled_by_source,
        ctx.drain,
        // CR 701.24a: most `deliver` callers (bucket-A destroy / sacrifice / SBA /
        // land play) carry no library placement — those are graveyard /
        // battlefield destinations. The W3 resume path is the lone caller that
        // threads a `Some(..)` here, so a parked Library-targeting redirect lands
        // at the requested index instead of the tail auto-shuffling it away.
        ctx.library_placement,
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
        if crate::game::filter::matches_target_filter(state, object_id, affected, &ctx) {
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
                Some(Duration::UntilHostLeavesPlay) => {
                    Some(ExileLinkKind::UntilSourceLeaves { return_zone: from })
                }
                Some(Duration::UntilOpponentBecomesMonarch) => {
                    exile_controller.map(|controller| ExileLinkKind::UntilOpponentBecomesMonarch {
                        return_zone: from,
                        controller,
                    })
                }
                _ if matches!(exile_tracking, ZoneDeliveryExileTracking::TrackBySource) => {
                    Some(ExileLinkKind::TrackedBySource)
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
    state
        .liminal_entries
        .get(&object_id)
        .map(|entry| entry.object.projected())
        .or_else(|| state.objects.get(&object_id))
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
    use crate::types::ability::{ControllerRef, TypeFilter, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;

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
        use crate::types::ability::{AbilityDefinition, AbilityKind, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

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
        use crate::types::ability::{AbilityDefinition, AbilityKind, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

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
        use crate::types::ability::ChosenAttribute;
        use crate::types::keywords::ProtectionTarget;
        use crate::types::statics::StaticMode;

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

/// Deliver a zone-change event that has already passed through replacement.
///
/// `library_placement` (CR 701.24a): when the event's delivered destination is
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
    library_placement: Option<LibraryPosition>,
    events: &mut Vec<GameEvent>,
) -> ZoneDeliveryResult {
    if let ProposedEvent::ZoneChange {
        object_id,
        from,
        to,
        cause,
        attach_to,
        enter_transformed: should_transform,
        enter_tapped: should_tap,
        enters_attacking,
        enter_with_counters,
        controller_override: ctrl_override,
        face_down_profile,
        chain_referent,
        enter_as_copy,
        discard_frame,
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
            return match crate::game::merge::move_merged_permanent_on_leave(
                state, object_id, to, &applied, events,
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
        match (to, library_placement.as_ref()) {
            (Zone::Library, Some(position)) => {
                took_plain_zone_transfer = false;
                let index = match position {
                    LibraryPosition::Top => Some(0),
                    LibraryPosition::Bottom => None,
                    // CR: `NthFromTop { n }` is 1-based ("second from the top"
                    // => n=2, index 1); `move_to_library_at_index` is 0-based.
                    LibraryPosition::NthFromTop { n } => Some(n.saturating_sub(1) as usize),
                    // CR 401.7: "beneath the top N cards" only flows from the
                    // `PutAtLibraryPosition` resolver (direct move), never this
                    // path. Exhaustiveness arm: honor a literal depth; a
                    // runtime-resolved depth needs the originating ability.
                    LibraryPosition::BeneathTop { depth } => match depth {
                        crate::types::ability::QuantityExpr::Fixed { value } => {
                            Some((*value).max(0) as usize)
                        }
                        _ => None,
                    },
                    // Digital-only Alchemy: `RandomWithinTop` only flows from the
                    // Conjure resolver (`conjure.rs`), which places the card
                    // directly and never routes through this path. Exhaustiveness
                    // arm: default placement.
                    LibraryPosition::RandomWithinTop { .. } => None,
                };
                zones::move_to_library_at_index(state, object_id, index, events);
            }
            _ => {
                if split_component_survivor.is_some() {
                    // CR 903.9b + CR 903.9c: this component has completed its
                    // replacement consult. Deliver the resulting destination
                    // with the CR 730.3 `from: None` event shape rather than
                    // pretending it independently left the battlefield.
                    took_plain_zone_transfer = false;
                    crate::game::merge::put_component_into_zone(state, object_id, to, events);
                } else {
                    took_plain_zone_transfer = true;
                    // CR 712.14a: carry the effect-driven "enters transformed"
                    // intent into the battlefield-entry guard so a non-permanent
                    // FRONT face (e.g. instant/sorcery) may enter as its PERMANENT
                    // back face. `should_transform` is destructured from
                    // `ProposedEvent::ZoneChange.enter_transformed` above; the
                    // flag is inert for any non-battlefield destination (the guard
                    // gates on `to == Battlefield`).
                    zones::move_to_zone_with_entry_flags(
                        state,
                        object_id,
                        to,
                        events,
                        should_transform,
                    );
                }
            }
        }
        // CR 730.3e: the survivor split (inside `move_to_zone` above) has consumed
        // any clause-2 routing override; clear it so it never leaks into a later
        // unrelated move. Purely synchronous lifetime (set → consumed → cleared in
        // this one delivery), so it never crosses a pause.
        state.merged_card_component_route = None;
        // CR 614.1d: determine whether the object actually entered the battlefield.
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
                    duration_subject_id: copy.source_id,
                    duration: copy.sacrifice_at.unwrap_or(Duration::Permanent),
                    values: *copy.values,
                    display_source: copy.display_source,
                    printed_ref: copy.printed_ref,
                    token_image_ref: copy.token_image_ref,
                    additional_modifications: copy.additional_modifications,
                    effect_kind: EffectKind::BecomeCopy,
                };
                let _ = crate::game::effects::become_copy::apply_precomputed_copy_values(
                    state, object_id, payload, events,
                );
            }
        }
        // CR 712.14a: Apply transformation if entering the battlefield transformed.
        if should_transform && to == Zone::Battlefield {
            if let Some(obj) = state.objects.get(&object_id) {
                if obj.back_face.is_some() && !obj.transformed {
                    let _ = crate::game::transform::transform_permanent(state, object_id, events);
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
        if to == Zone::Battlefield {
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
        } else if !enter_with_counters.is_empty() {
            // CR 122.1: Effect-driven counters for non-battlefield
            // destinations — e.g., "exile it with three egg counters
            // on it" (Darigaaz Reincarnated). Apply directly via the
            // shared single-authority resolver so counter-doubling
            // replacements (Doubling Season, Hardened Scales) and
            // event emission stay consistent.
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
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
    exile_controller: Option<PlayerId>,
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
        track_exiled_by_source,
        library_placement,
        enter_attached_to,
        exile_controller,
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
        track_exiled_by_source,
        library_placement,
        enter_attached_to,
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
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
    exile_controller: Option<PlayerId>,
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
        crate::types::zones::ChainReferentIntent::Silent,
        track_exiled_by_source,
        library_placement,
        enter_attached_to,
        exile_controller,
        HashSet::new(),
        events,
    )
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
    // CR 608.2c: whether this entry is the producer a following demonstrative
    // anaphor binds to. Only `move_object_with_terminal` forwards a request's
    // intent; the four public `execute_zone_move*` wrappers are raw movers with
    // no originating instruction to speak for, and pass `Silent`.
    chain_referent: crate::types::zones::ChainReferentIntent,
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
    exile_controller: Option<PlayerId>,
    replacement_applied: HashSet<AppliedReplacementKey>,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveTerminalResult {
    let Some(member) = state
        .objects
        .get(&obj_id)
        .map(ObjectIncarnationRef::from_object)
    else {
        return ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained);
    };
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
        return ZoneMoveTerminalResult::Completed(ZoneMoveCompletion::Remained);
    }
    let mut proposed = ProposedEvent::zone_change(obj_id, from_zone, dest_zone, Some(source_id));
    if let ProposedEvent::ZoneChange {
        applied,
        chain_referent: ref mut intent,
        ..
    } = &mut proposed
    {
        *applied = replacement_applied;
        *intent = chain_referent;
    }

    // CR 712.14a: Set enter_transformed on the proposed event so replacement effects
    // preserve it through the pipeline.
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
    // event so the replacement pipeline preserves it. `Unspecified` leaves the
    // event's default untouched (the originating effect set no explicit state);
    // an explicit `Tapped`/`Untapped` overrides it. Seeding the enum directly
    // (rather than collapsing through a bool) keeps the `Unspecified`-vs-
    // `Untapped` distinction the pipeline carrier `EtbTapState` exists to hold.
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

    // CR 110.2a: Set controller_override on the proposed event so replacement effects
    // see the correct controller through the pipeline.
    if let Some(ctrl) = controller_override {
        if let ProposedEvent::ZoneChange {
            controller_override: ref mut co,
            ..
        } = proposed
        {
            *co = Some(ctrl);
        }
    }

    // CR 708.2a + CR 708.3: Carry the face-down profile on the proposed event so
    // the object is turned face down before it enters the battlefield (after the
    // replacement pipeline runs, in `deliver_replaced_zone_change`).
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

    // CR 306.5b + CR 310.4b + CR 614.1c: Seed the intrinsic "enters with N
    // counters" replacement when a planeswalker or battle enters the
    // battlefield from any source (effect-driven entry — bounce-return,
    // reanimate, blink, etc.). Spell-cast entry is handled in stack.rs.
    //
    // CR 708.2a + CR 708.3: the INTRINSIC seeding below is skipped for a
    // face-down entry — the object enters as the profile's body (a 2/2
    // creature) with no loyalty/defense characteristic, so a manifested
    // planeswalker card must not enter with loyalty counters (issue #7822).
    // Only the intrinsic half is gated: an effect explicitly instructing entry
    // counters on a face-down entrant (`effect_enter_with_counters` below) is
    // a separate instruction whose counters are markers on the resulting
    // object (CR 122.1) and must survive. The gate reads the proposed event,
    // on which the profile was just stamped above.
    let enters_face_down = matches!(
        &proposed,
        ProposedEvent::ZoneChange {
            face_down_profile: Some(_),
            ..
        }
    );
    if dest_zone == Zone::Battlefield && !enters_face_down {
        if let Some(obj) = state
            .liminal_entries
            .get(&obj_id)
            .map(|entry| entry.object.projected())
            .or_else(|| state.objects.get(&obj_id))
        {
            // CR 712.14a + CR 712.18: A permanent entering transformed (e.g. a
            // double-faced card exiled and returned with its back face up, like
            // a creature-front // planeswalker-back DFC) will have its back
            // face's characteristics on the battlefield. The physical face swap
            // happens later in `deliver_replaced_zone_change`, so `obj` still
            // shows its front face here — read the back face's printed
            // loyalty/defense directly so CR 306.5b/310.4b seeds the counter map
            // (the source of truth per CR 306.5c). Without this a transforming
            // planeswalker enters with 0 loyalty counters and dies immediately
            // to CR 704.5i. Ravenous (front-face cast-time) does not apply to an
            // effect-driven transformed entry, so only face counters are seeded.
            let intrinsic = match (enter_transformed, obj.back_face.as_ref()) {
                (true, Some(back)) => {
                    crate::game::printed_cards::intrinsic_entry_counters_for_face(
                        back.printed_loyalty,
                        back.loyalty,
                        None,
                        back.defense,
                        &back.card_types,
                    )
                }
                _ => crate::game::printed_cards::intrinsic_etb_counters(obj, None),
            };
            if !intrinsic.is_empty() {
                if let ProposedEvent::ZoneChange {
                    enter_with_counters,
                    ..
                } = &mut proposed
                {
                    enter_with_counters.extend(intrinsic);
                }
            }
        }
    }
    // CR 122.1 + CR 614.1c: effect-driven enter-with-counters apply to EVERY
    // battlefield entry, face-down included — the explicit instruction is
    // independent of the intrinsic loyalty/defense seeding gated above.
    if dest_zone == Zone::Battlefield {
        // CR 122.1 + CR 614.1c: Seed effect-driven enter-with-counters from
        // `Effect::ChangeZone.enter_with_counters` (Darkness Crystal class:
        // "put target creature card ... onto the battlefield with two
        // additional +1/+1 counters on it"). Only applied for battlefield
        // entries — other destinations (Exile, etc.) carry the counters
        // through to drive `apply_etb_counters` downstream when the object
        // arrives at a counter-bearing zone.
        if !effect_enter_with_counters.is_empty() {
            if let ProposedEvent::ZoneChange {
                enter_with_counters,
                ..
            } = &mut proposed
            {
                enter_with_counters.extend(effect_enter_with_counters.iter().cloned());
            }
        }
    } else if !effect_enter_with_counters.is_empty() {
        // CR 122.1 + CR 614.1c: For non-battlefield destinations (e.g., Exile
        // for "exile it with three egg counters on it"), counters are applied
        // post-move via `apply_etb_counters` directly on the object. The
        // ProposedEvent slot is reserved for battlefield entries that flow
        // through the replacement pipeline.
        if let ProposedEvent::ZoneChange {
            enter_with_counters,
            ..
        } = &mut proposed
        {
            enter_with_counters.extend(effect_enter_with_counters.iter().cloned());
        }
    }

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
        ReplacementResult::Execute(mut event) => {
            let mut pending_aura_choice: Option<(PlayerId, ObjectId, Vec<TargetRef>)> = None;
            // CR 303.4g: set when the unhosted entrant came from the stack and so
            // must be put into its owner's graveyard rather than remain. Acted on
            // after the borrow of `event` ends — the battlefield entry is denied
            // and a FRESH graveyard move is proposed in its place.
            let mut unhosted_to_owners_graveyard = false;
            if let ProposedEvent::ZoneChange {
                object_id,
                from,
                to: Zone::Battlefield,
                attach_to,
                controller_override,
                ..
            } = &mut event
            {
                if attach_to.is_none() {
                    if let Some(enchant_filter) = aura_enchant_filter(state, *object_id) {
                        // CR 614.12: read the entrant's projected characteristics,
                        // not the pre-entry object still stored under this id — for
                        // a meld the latter is the exiled component card and has
                        // the wrong controller as well as the wrong typeline.
                        let controller = (*controller_override)
                            .or_else(|| {
                                entering_object_projection(state, *object_id)
                                    .map(|obj| obj.controller)
                            })
                            .unwrap_or(PlayerId(0));
                        let legal_targets = legal_aura_attachment_targets(
                            state,
                            *object_id,
                            entering_object_projection(state, *object_id),
                            controller,
                            &enchant_filter,
                        );
                        match legal_targets.as_slice() {
                            // CR 303.4g: no legal object or player to enchant, so
                            // this entry does not happen. Decided BEFORE
                            // `deliver_replaced_zone_change`, i.e. before the
                            // object is inserted into the battlefield, before the
                            // meld commit journals anything, and before any entry
                            // event — the rule denies the entry, so nothing may
                            // observe one.
                            [] => {
                                match entering_object_projection(state, *object_id)
                                    .map(|entrant| unhosted_aura_entry(entrant, *from))
                                {
                                    Some(UnhostedAuraEntry::OwnersGraveyard) => {
                                        unhosted_to_owners_graveyard = true;
                                    }
                                    // "The Aura remains in its current zone" —
                                    // and, for a token already in a zone, "isn't
                                    // created" has nothing left to withhold, so
                                    // both leave the object exactly where it is.
                                    Some(UnhostedAuraEntry::NotCreated)
                                    | Some(UnhostedAuraEntry::RemainInCurrentZone)
                                    | None => {
                                        return ZoneMoveTerminalResult::Completed(
                                            ZoneMoveCompletion::Remained,
                                        );
                                    }
                                }
                            }
                            [TargetRef::Object(id)] => {
                                *attach_to =
                                    Some(crate::game::game_object::AttachTarget::Object(*id));
                            }
                            [TargetRef::Player(id)] => {
                                *attach_to =
                                    Some(crate::game::game_object::AttachTarget::Player(*id));
                            }
                            _ => {
                                pending_aura_choice = Some((controller, *object_id, legal_targets))
                            }
                        }
                    }
                }
                // CR 303.4i specified-host Remain is handled after delivery when
                // `attach_to` fails / SBA (CR 704.5m). Pre-move filter checks while
                // the Aura is still in GY falsely Remained legal Gift/Lynde hosts.
            }
            if unhosted_to_owners_graveyard {
                // CR 303.4g: "…the Aura is put into its owner's graveyard instead
                // of entering the battlefield."
                //
                // CR 614.6: the approved battlefield entry never happens, and the
                // graveyard placement that replaces it is a FRESH, never-consulted
                // event — so it routes through the pipeline rather than being
                // written as a destination rewrite of the already-approved event.
                // A board-wide `Moved` graveyard→exile redirect (Rest in Peace /
                // Leyline of the Void) therefore fires on it. Same house decision
                // as `engine_replacement.rs`'s CR 608.3e prevented-permanent
                // graveyard fallback, which is the structural twin of this arm.
                //
                // The already-applied set rides along on the fresh request so no
                // replacement can be spent twice: every def that applied to this
                // entry was consulted against `to: Battlefield`, and the new
                // proposal is `to: Graveyard`, so a battlefield-scoped entry
                // replacement cannot re-match it either way — carrying `applied`
                // makes that structural fact explicit instead of implicit.
                let applied = event.applied_set().clone();
                return move_object_with_terminal(
                    state,
                    ZoneMoveRequest::effect(obj_id, Zone::Graveyard, source_id)
                        .with_replacement_applied(applied),
                    events,
                );
            }
            if let Some((controller, aura_id, legal_targets)) = pending_aura_choice {
                let delivery_start = events.len();
                match deliver_replaced_zone_change(
                    state,
                    event,
                    Some(source_id),
                    duration,
                    exile_controller,
                    track_exiled_by_source,
                    PostReplacementDrainOwner::DeliveryTail,
                    library_placement,
                    events,
                ) {
                    ZoneDeliveryResult::Done => {
                        debug_assert_eq!(
                            zone_move_completion_from_delivery(member, &events[delivery_start..]),
                            ZoneMoveCompletion::Moved,
                            "an Aura host choice follows a completed battlefield entry"
                        );
                    }
                    ZoneDeliveryResult::NeedsChoice(player) => {
                        return ZoneMoveTerminalResult::NeedsChoice(player);
                    }
                }
                state.waiting_for = WaitingFor::ReturnAsAuraTarget {
                    player: controller,
                    source_id,
                    returned_id: aura_id,
                    legal_targets,
                    pending_effect: Box::new(ResolvedAbility::new(
                        Effect::Attach {
                            attachment: TargetFilter::SelfRef,
                            target: TargetFilter::Any,
                        },
                        Vec::new(),
                        source_id,
                        controller,
                    )),
                };
                return ZoneMoveTerminalResult::NeedsAuraAttachmentChoice;
            }
            let delivery_start = events.len();
            match deliver_replaced_zone_change(
                state,
                event,
                Some(source_id),
                duration,
                exile_controller,
                track_exiled_by_source,
                PostReplacementDrainOwner::DeliveryTail,
                library_placement,
                events,
            ) {
                ZoneDeliveryResult::Done => {}
                ZoneDeliveryResult::NeedsChoice(player) => {
                    return ZoneMoveTerminalResult::NeedsChoice(player);
                }
            }
            ZoneMoveTerminalResult::Completed(zone_move_completion_from_delivery(
                member,
                &events[delivery_start..],
            ))
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
            ZoneMoveRequest::casting_to_stack(object_id, object_id),
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
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ReplacementDefinition, TargetFilter,
    };
    use crate::types::identifiers::CardId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::resolution::ResolutionFrame;

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
        use crate::game::engine::apply_as_current;
        use crate::types::ability::ReplacementMode;
        use crate::types::actions::GameAction;

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
        assert!(
            state.pending_replacement.is_some(),
            "the parked record must carry the placement for the resume to thread back"
        );
        assert_eq!(
            state
                .pending_replacement
                .as_ref()
                .and_then(|p| p.library_placement.clone()),
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
        use crate::game::engine::apply_as_current;
        use crate::types::ability::ReplacementMode;
        use crate::types::actions::GameAction;

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

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 1 })
            .expect("decline first optional redirect");
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
                .and_then(|pending| pending.library_placement.clone()),
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
            vec![a, b, first, second],
            "both declined batch moves must land on the bottom in request order"
        );
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
    /// The second redirect is gated by `UnlessControlsMatching` on a sentinel
    /// creature so it is suppressed on the first scan and becomes applicable once
    /// the sentinel is removed between the two choices (a realistic board change
    /// across a paused replacement). Before the fix the re-park reset the
    /// placement to `None`, so the final delivery shuffled — the order assertion
    /// below fails (and the `ShuffledLibrary` absence assertion guards against a
    /// seed-identity permutation false-pass).
    #[test]
    fn library_placement_survives_two_sequential_parks() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::{
            ReplacementCondition, ReplacementMode, TypeFilter, TypedFilter,
        };
        use crate::types::actions::GameAction;

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
                .and_then(|p| p.library_placement.clone()),
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
                .and_then(|p| p.library_placement.clone()),
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
    use crate::game::zones::create_object;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;

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
    /// entry-face rewrite must not be front-regressive, and `transform_permanent`
    /// must still fire after the guarded entry.
    #[test]
    fn mdfc_permanent_front_transformed_entry_still_lands() {
        use crate::game::game_object::BackFaceData;

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
