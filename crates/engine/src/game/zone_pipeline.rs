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
    BatchCompletion, ExileLinkKind, GameState, LiminalEntryKind, LogicalZoneChangeGroup,
    MergedCardComponentRoute, PendingBatchDeliveries, PendingBatchZoneChangeCause,
    PendingBatchZoneMoveRequest, PendingCounterPostAction, PendingLiminalEntryResume,
    PendingZoneChangeDelivery, PostReplacementDrainOwner, WaitingFor, ZoneDeliveryExileTracking,
    ZoneMoveCompletion,
};
use std::collections::HashSet;

use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{AppliedReplacementKey, ProposedEvent};
use crate::types::zones::{EtbTapState, Zone};

use crate::game::effects::change_zone::shuffle_library;
use crate::game::game_object::AttachTarget;
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
    /// CR 712.14a. Genuinely two-valued (enters showing back face or not) — no
    /// Unspecified third state to preserve, unlike `enter_tapped`.
    pub enter_transformed: bool,
    /// CR 110.2a controller override ("enters under your control").
    pub controller_override: Option<PlayerId>,
    /// CR 122.1 + CR 614.1c effect-driven enter-with counters.
    pub enter_with_counters: Vec<(CounterType, u32)>,
    /// CR 708.2a + CR 708.3 face-down entry profile.
    pub face_down_profile: Option<FaceDownProfile>,
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
            enter_transformed: self.mods.enter_transformed,
            controller_override: self.mods.controller_override,
            enter_with_counters: self.mods.enter_with_counters,
            face_down_profile: self.mods.face_down_profile,
            attach_to: self.mods.attach_to,
            library_placement: self.placement,
            exile_duration: self.exile_links.duration,
            exile_tracking: self.exile_links.tracking,
            replacement_applied: self.replacement_applied,
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
                enter_transformed: pending.enter_transformed,
                controller_override: pending.controller_override,
                enter_with_counters: pending.enter_with_counters,
                face_down_profile: pending.face_down_profile,
                attach_to: pending.attach_to,
            },
            placement: pending.library_placement,
            exile_links: ExileLinkSpec {
                duration: pending.exile_duration,
                tracking: pending.exile_tracking,
            },
            replacement_applied: pending.replacement_applied,
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

    /// Library placement override (`LibraryPosition::Top` / `Bottom` /
    /// `NthFromTop`). Only meaningful when `to == Zone::Library`.
    pub fn at_library_position(mut self, position: LibraryPosition) -> Self {
        self.placement = Some(position);
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
/// (CR 400.7d: kicker / additional-cost / convoke / cast-timing memory) is
/// preserved structurally by the delivery itself — see [`CastLinkSnapshot`].
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
    if delivery_events.iter().any(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged { record, .. }
                if record
                    .trigger_source_context()
                    .is_some_and(|context| context.identity.reference == member)
        )
    }) {
        ZoneMoveCompletion::Moved
    } else {
        ZoneMoveCompletion::Remained
    }
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
        // remains in place and ceases to exist at the next SBA (CR 111.7).
        if zones::token_is_outside_battlefield_and_stack(obj) {
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
    // Phase E tranche 2: 11 raw library-position callers still bypass this consult
    // by calling `zones::move_to_library_position` / `move_to_library_at_index`
    // directly instead of routing through `move_object`'s placement arm. They are:
    //   - engine_resolution_choices.rs (×5)
    //   - reveal_until.rs:~400 (`shuffle_to_bottom`)
    //   - drawn_this_turn_choice.rs:~114
    //   - discover.rs:~103 (put-back of unhit cards)
    //   - put_on_top.rs:~153 / ~158
    //   - cascade.rs:~154 (bottom-in-random-order)
    // Migrating each onto this arm is a guaranteed no-op today (zero pool
    // `Moved` defs target the library) but pins the redirect consult for the
    // future. Re-verify the census before lifting:
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
            if let ProposedEvent::ZoneChange { applied, .. } = &mut proposed {
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
                    replacement::park_waiting_for(state, player);
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
        if let ProposedEvent::ZoneChange { applied, .. } = &mut proposed {
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
                replacement::park_waiting_for(state, player);
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
            return ZoneMoveTerminalResult::Completed(zone_move_completion_from_delivery(
                member,
                &events[delivery_start..],
            ));
        }
        let mut proposed = ProposedEvent::zone_change(req.object_id, from_zone, req.to, source_id);
        if let ProposedEvent::ZoneChange {
            enter_transformed,
            enter_tapped,
            controller_override,
            enter_with_counters,
            face_down_profile,
            applied,
            ..
        } = &mut proposed
        {
            *enter_transformed = req.mods.enter_transformed;
            if !req.mods.enter_tapped.is_unspecified() {
                *enter_tapped = req.mods.enter_tapped;
            }
            *controller_override = req.mods.controller_override;
            enter_with_counters.extend(req.mods.enter_with_counters.iter().cloned());
            *face_down_profile = req.mods.face_down_profile.clone().map(Box::new);
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
        req.mods.controller_override,
        &req.mods.enter_with_counters,
        req.mods.face_down_profile.as_ref(),
        track_exiled_by_source,
        None,
        None,
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
        let anticipated_pause = anticipated_zone_change_delivery(state, &req);
        let delivery_start = events.len();
        let object_id = req.object_id;
        match move_object_with_terminal(state, req, events) {
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
                let paused_current = state
                    .pending_zone_change_delivery_from_replacement()
                    .or_else(|| {
                        anticipated_pause.map(|mut boundary| {
                            boundary.append_delivery_events(&events[delivery_start..]);
                            boundary
                        })
                    })
                    .expect("parked batch zone change must retain an exact boundary");
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
        enter_transformed,
        controller_override,
        enter_with_counters,
        face_down_profile,
        attach_to,
        applied,
        ..
    } = &mut expected_event
    {
        *enter_tapped = request.mods.enter_tapped;
        *enter_transformed = request.mods.enter_transformed;
        *controller_override = request.mods.controller_override;
        *enter_with_counters = request.mods.enter_with_counters.clone();
        *face_down_profile = request.mods.face_down_profile.clone().map(Box::new);
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
    exile_tracking: ZoneDeliveryExileTracking,
    drain: PostReplacementDrainOwner,
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
        exile_tracking,
        drain,
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

fn aura_enchant_filter(state: &GameState, object_id: ObjectId) -> Option<TargetFilter> {
    let obj = state.objects.get(&object_id)?;
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

fn legal_aura_attachment_targets(
    state: &GameState,
    aura_id: ObjectId,
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
        .filter(|id| crate::game::effects::attach::can_attach_to_object(state, aura_id, *id))
        .map(TargetRef::Object)
        .collect();

    targets.extend(state.players.iter().filter_map(|player| {
        if player.is_eliminated || player.is_phased_out() {
            return None;
        }
        if crate::game::filter::player_matches_target_filter_in_state(
            state,
            enchant_filter,
            player.id,
            Some(controller),
        ) {
            Some(TargetRef::Player(player.id))
        } else {
            None
        }
    }));

    targets
}

/// Disposition of an object that has just become an Aura while already on the
/// battlefield (the copy path — see [`resolve_entering_aura_attachment`]).
pub(crate) enum EnteringAuraAttachment {
    /// The object is not an Aura needing attachment (not an Aura, an Aura that's
    /// also a creature per CR 303.4d, or already attached).
    NotApplicable,
    /// Attachment resolved without a player choice — either auto-attached to the
    /// sole legal host, or deliberately left unattached because there is no legal
    /// host (CR 303.4g; the CR 704.5m unattached-Aura SBA will handle it).
    Resolved,
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
/// it enchants. CR 303.4g: with no legal host the Aura would not enter at all;
/// the engine's post-entry equivalent is to leave it unattached so the
/// unattached-Aura SBA (CR 704.5m) moves it to the graveyard on the next check.
pub(crate) fn resolve_entering_aura_attachment(
    state: &mut GameState,
    object_id: ObjectId,
) -> EnteringAuraAttachment {
    let Some(enchant_filter) = aura_enchant_filter(state, object_id) else {
        return EnteringAuraAttachment::NotApplicable;
    };
    let Some(obj) = state.objects.get(&object_id) else {
        return EnteringAuraAttachment::NotApplicable;
    };
    // CR 303.4 + CR 704.5m: entry-time attachment only applies to an Aura that is
    // actually on the battlefield. Defensive guard — if an intermediate entry
    // trigger or replacement moved the realized copy off the battlefield before
    // this runs (it is the LAST step of `finish_copy_target_choice_entry`),
    // attaching it or prompting for a host of a non-battlefield Aura would be
    // invalid state; do nothing and let it resolve wherever it now lives.
    if obj.zone != Zone::Battlefield {
        return EnteringAuraAttachment::NotApplicable;
    }
    // Only resolve entry attachment for an as-yet-unattached Aura; a copy that
    // was already attached by some other effect must not be re-homed here.
    if obj.attached_to.is_some() {
        return EnteringAuraAttachment::NotApplicable;
    }
    let controller = obj.controller;
    let legal_targets =
        legal_aura_attachment_targets(state, object_id, controller, &enchant_filter);
    match legal_targets.as_slice() {
        // CR 303.4g: no legal host — leave unattached for the CR 704.5m SBA.
        [] => EnteringAuraAttachment::Resolved,
        [TargetRef::Object(id)] => {
            crate::game::effects::attach::attach_to(state, object_id, *id);
            EnteringAuraAttachment::Resolved
        }
        [TargetRef::Player(id)] => {
            crate::game::effects::attach::attach_to_player(state, object_id, *id);
            EnteringAuraAttachment::Resolved
        }
        _ => EnteringAuraAttachment::NeedsChoice {
            controller,
            legal_targets,
        },
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
        enter_with_counters,
        controller_override: ctrl_override,
        face_down_profile,
        enter_as_copy,
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
        match (to, library_placement.as_ref()) {
            (Zone::Library, Some(position)) => {
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
                    crate::game::merge::put_component_into_zone(state, object_id, to, events);
                } else {
                    zones::move_to_zone(state, object_id, to, events);
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
        if to == Zone::Battlefield || from == Zone::Battlefield {
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
        if should_tap.resolve(false) && to == Zone::Battlefield {
            if let Some(obj) = state.objects.get_mut(&object_id) {
                obj.tapped = true;
            }
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
                if let Some(obj) = state.objects.get_mut(&object_id) {
                    obj.entered_via_ability_source = Some(src);
                }
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
                    exile_tracking,
                    drain,
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
                    exile_tracking,
                    drain,
                    None,
                );
            }
        }
        return apply_zone_delivery_tail(
            state,
            object_id,
            from,
            to,
            cause,
            source_id,
            duration,
            exile_tracking,
            drain,
            library_placement.as_ref(),
            events,
        );
    }
    ZoneDeliveryResult::Done
}

fn replacement_pause_delivery_result(state: &GameState) -> ZoneDeliveryResult {
    match &state.waiting_for {
        WaitingFor::ReplacementChoice { player, .. }
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
pub(crate) fn execute_zone_move_with_terminal(
    state: &mut GameState,
    obj_id: ObjectId,
    from_zone: Zone,
    dest_zone: Zone,
    source_id: ObjectId,
    duration: Option<&Duration>,
    enter_transformed: bool,
    enter_tapped: EtbTapState,
    controller_override: Option<PlayerId>,
    effect_enter_with_counters: &[(CounterType, u32)],
    face_down_profile: Option<&crate::types::ability::FaceDownProfile>,
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
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
        controller_override,
        effect_enter_with_counters,
        face_down_profile,
        track_exiled_by_source,
        library_placement,
        enter_attached_to,
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
    controller_override: Option<PlayerId>,
    effect_enter_with_counters: &[(CounterType, u32)],
    face_down_profile: Option<&crate::types::ability::FaceDownProfile>,
    track_exiled_by_source: bool,
    library_placement: Option<LibraryPosition>,
    enter_attached_to: Option<AttachTarget>,
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
    if let ProposedEvent::ZoneChange { applied, .. } = &mut proposed {
        *applied = replacement_applied;
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
    if dest_zone == Zone::Battlefield {
        if let Some(obj) = state
            .liminal_entries
            .get(&obj_id)
            .map(|entry| &entry.object)
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
                        back.loyalty,
                        back.defense,
                        &back.card_types,
                    )
                }
                _ => crate::game::printed_cards::intrinsic_etb_counters(obj),
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
            if let ProposedEvent::ZoneChange {
                object_id,
                to: Zone::Battlefield,
                attach_to,
                controller_override,
                ..
            } = &mut event
            {
                if attach_to.is_none() {
                    if let Some(enchant_filter) = aura_enchant_filter(state, *object_id) {
                        let controller = (*controller_override)
                            .or_else(|| state.objects.get(object_id).map(|obj| obj.controller))
                            .unwrap_or(PlayerId(0));
                        let legal_targets = legal_aura_attachment_targets(
                            state,
                            *object_id,
                            controller,
                            &enchant_filter,
                        );
                        match legal_targets.as_slice() {
                            [] => {
                                return ZoneMoveTerminalResult::Completed(
                                    ZoneMoveCompletion::Remained,
                                );
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
            }
            if let Some((controller, aura_id, legal_targets)) = pending_aura_choice {
                let delivery_start = events.len();
                match deliver_replaced_zone_change(
                    state,
                    event,
                    Some(source_id),
                    duration,
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
            replacement::park_waiting_for(state, player);
            ZoneMoveTerminalResult::NeedsChoice(player)
        }
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
