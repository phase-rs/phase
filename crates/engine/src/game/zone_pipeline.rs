//! Unified zone-change pipeline (Phase A carve-out).
//!
//! This module is the home of the single zone-change entry point. Phase A moves
//! the most-complete pipeline copy (`change_zone::execute_zone_move` and its
//! delivery tail) here verbatim, exposes the new request/cause types and the
//! `move_object` wrapper, and seeds the `ApprovedZoneChange` proof token used to
//! fence delivery in later phases. Existing callers continue to reach the moved
//! functions through `pub(crate) use` shims left at their old `change_zone.rs`
//! paths, so no behavior changes in this phase.
//!
//! Layer discipline (PLAN §2): `zones.rs` keeps every guard that must hold
//! unconditionally (CR 111.8 token guard, CR 614.1d ETB block, CR 400.7 cleanup,
//! `GameEvent::ZoneChanged` emission); this module owns the "would"-semantics
//! layer (CR 614.1 / 614.6 replacement consult, CR 616.1 choices, CR 614.1c
//! enters-with seeding) plus the CR 303.4f aura-host choice.

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    Duration, Effect, LibraryPosition, ResolvedAbility, StaticDefinition, TargetFilter, TargetRef,
};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    BatchCompletion, ExileLink, ExileLinkKind, GameState, PendingBatchDeliveries,
    PendingCounterPostAction, WaitingFor, ZoneDeliveryExileTracking,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
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
//
// Phase A introduces the request/cause/mods vocabulary; the call sites that
// construct each variant land in Phases B–D, so several arms are unconstructed
// in this phase.
#[allow(dead_code)]
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
    // ---- exempt causes: pipeline-internal, replacement consult skipped ----
    /// CR 601.2a: "the player first moves that card ... to the stack" — part of
    /// the casting process, not a discrete replaceable event.
    CastingToStack { source: ObjectId },
    /// CR 103.5: pregame opening draws and mulligan returns.
    PregameProcedure,
    /// CR 800.4a: owner left the game; all objects they own leave the game.
    PlayerLeftGame,
    /// CR 730.3: merged-component routing already inside a delivering move.
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
    /// - `MergedComponentRouting` (CR 730.3): the merged-permanent move already
    ///   consulted replacements; the component split is internal routing.
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
            | ZoneChangeCause::CommanderRuleReturn => false,
            ZoneChangeCause::CastingToStack { .. }
            | ZoneChangeCause::PregameProcedure
            | ZoneChangeCause::PlayerLeftGame
            | ZoneChangeCause::MergedComponentRouting
            | ZoneChangeCause::DebugCommand => true,
        }
    }
}

/// Destination modifiers — the union of what the pipeline copies need to seed
/// onto the proposed `ZoneChange` before the replacement consult.
#[derive(Default)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
}

// Builder constructors are the Phase B+ call-site ergonomics; unused in Phase A.
#[allow(dead_code)]
impl ZoneMoveRequest {
    /// Effect- or ability-driven move with no destination modifiers.
    pub fn effect(object_id: ObjectId, to: Zone, source: ObjectId) -> Self {
        Self {
            object_id,
            to,
            cause: ZoneChangeCause::Effect { source },
            mods: EntryMods::default(),
            placement: None,
            exile_links: ExileLinkSpec::default(),
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
        match &self.cause {
            ZoneChangeCause::Effect { source }
            | ZoneChangeCause::Cost { source }
            | ZoneChangeCause::CastingToStack { source } => Some(*source),
            _ => None,
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
//
// Phase A seeds the token + its three mint paths; the consuming callers
// (`deliver`, the bucket-A migrations) arrive in Phase B, so the field and
// constructors are not yet read in this phase.
#[allow(dead_code)]
pub struct ApprovedZoneChange {
    event: ProposedEvent,
    _seal: (),
}

// Phase B wires every mint path and `deliver` consumer; Phase A only seeds them.
#[allow(dead_code)]
impl ApprovedZoneChange {
    /// The third mint path (PLAN §6.2): seal an event that has already completed
    /// a full replacement pass OUTSIDE this module — the outer Destroy /
    /// Sacrifice / Discard pass lowers into a `ZoneChange` carrying its
    /// `applied: HashSet<ReplacementId>`. Legal ONLY on `ZoneChange` payloads;
    /// returns `Err(event)` for anything else so the caller can fall back.
    /// Re-proposing such an event through `move_object` would discard `applied`
    /// and double-apply Moved definitions / redo CR 616.1 ordering.
    pub(crate) fn approve_post_replacement(
        event: ProposedEvent,
    ) -> Result<ApprovedZoneChange, ProposedEvent> {
        if matches!(event, ProposedEvent::ZoneChange { .. }) {
            Ok(ApprovedZoneChange { event, _seal: () })
        } else {
            Err(event)
        }
    }

    /// Mint internally once `move_object`'s ZoneChange arm has a post-replacement
    /// (or exempt) event ready to deliver.
    fn seal(event: ProposedEvent) -> ApprovedZoneChange {
        ApprovedZoneChange { event, _seal: () }
    }
}

/// Context threaded into `deliver`: the attributed source and exile-link spec.
/// Consumed by the Phase B bucket-A `deliver(approved, ctx)` migrations.
#[allow(dead_code)]
pub(crate) struct DeliveryCtx {
    pub source_id: Option<ObjectId>,
    pub exile_links: ExileLinkSpec,
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

pub(crate) enum ZoneDeliveryResult {
    Done,
    NeedsChoice(PlayerId),
}

/// THE single zone-change entry point (Phase A: thin wrapper over the carved-out
/// `execute_zone_move` engine). Reads `from` from the object's current zone,
/// unpacks `EntryMods` / `ExileLinkSpec`, and runs the proposal through the
/// replacement pipeline + delivery tail.
///
/// In this phase the entry has no production callers yet — call-site migration
/// is Phase B+ — so it preserves the exact behavior of `execute_zone_move` for
/// every modifier combination it forwards.
///
/// `pub(crate)` while `ZoneMoveResult` is `pub(crate)`: every caller lives in the
/// engine crate. (PLAN §1.3 writes `pub fn`; widening to `pub` only matters once
/// a cross-crate consumer exists, which it does not yet.)
pub(crate) fn move_object(
    state: &mut GameState,
    req: ZoneMoveRequest,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    let Some(from_zone) = state.objects.get(&req.object_id).map(|o| o.zone) else {
        // The object no longer exists (already moved / ceased to exist); nothing
        // to do. The unconditional guards in `zones.rs` would no-op anyway.
        return ZoneMoveResult::Done;
    };

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
            return ZoneMoveResult::Done;
        }
        // CR 603.2g + CR 603.6a: A Battlefield -> Battlefield move does not put a
        // permanent onto the battlefield — no entry event occurs, so no
        // would-style replacement should be consulted (and the primitive would
        // reject it as a no-op regardless), mirroring the `zones::move_to_zone`
        // no-op guard.
        if from_zone == Zone::Battlefield && req.to == Zone::Battlefield {
            return ZoneMoveResult::Done;
        }
    }

    // Library-placement arm. A `Some(placement)` request delivers directly to the
    // requested library index via the `move_to_library_at_index` primitive,
    // skipping the replacement consult. Phase D wires the exempt pregame/debug
    // library-bottom and debug top/Nth callers through here; the mulligan and
    // debug call sites pass a placement and rely on this direct delivery.
    //
    // DEFERRED (W3 / PLAN §3.5): running the consult on a library placement —
    // "the consult should still run for future-proofing per the single-entry
    // principle". It is a guaranteed no-op today: no `Moved` replacement in the
    // card pool targets `destination_zone(Library)` (verified: 25 Battlefield /
    // 17 Graveyard / 2 Exile destinations, zero Library; reproduce with
    //   rg -o 'destination_zone\(Zone::\w+\)' crates/engine/src | sort | uniq -c
    // — re-run before lifting this deferral). Completing it correctly
    // also requires gating the CR 701.24a delivery-tail auto-shuffle on
    // placement-absence across the shared `deliver` / `deliver_replaced_zone_change`
    // signatures (a library *placement* must NOT shuffle, but a plain
    // library-destination ZoneChange MUST) — a cross-cutting change with a
    // silent-randomization landmine for zero current correctness gain. The raw
    // `move_to_library_position` / `_at_index` sibling production callers
    // (put_on_top, cascade, discover, reveal_until, drawn_this_turn_choice,
    // engine_resolution_choices) stay on the raw movers until that completion;
    // they are library repositions with no Moved-redirect class to consult.
    if let Some(position) = &req.placement {
        if req.to == Zone::Library {
            let index = match position {
                LibraryPosition::Top => Some(0),
                LibraryPosition::Bottom => None,
                // CR: `NthFromTop { n }` is 1-based ("second from the top" => n=2,
                // index 1); `move_to_library_at_index` is 0-based.
                LibraryPosition::NthFromTop { n } => Some(n.saturating_sub(1) as usize),
            };
            zones::move_to_library_at_index(state, req.object_id, index, events);
            return ZoneMoveResult::Done;
        }
    }

    let source_id = req.source();
    let exile_links = req.exile_links;
    let track_exiled_by_source = matches!(
        exile_links.tracking,
        ZoneDeliveryExileTracking::TrackBySource
    );

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
            zones::move_to_zone(state, req.object_id, req.to, events);
            return ZoneMoveResult::Done;
        }
        let mut proposed = ProposedEvent::zone_change(req.object_id, from_zone, req.to, source_id);
        if let ProposedEvent::ZoneChange {
            enter_transformed,
            enter_tapped,
            controller_override,
            enter_with_counters,
            face_down_profile,
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
        }
        let approved = ApprovedZoneChange::seal(proposed);
        return match deliver(
            state,
            approved,
            DeliveryCtx {
                source_id,
                exile_links,
            },
            events,
        ) {
            ZoneDeliveryResult::Done => ZoneMoveResult::Done,
            ZoneDeliveryResult::NeedsChoice(player) => ZoneMoveResult::NeedsChoice(player),
        };
    }

    execute_zone_move(
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
        events,
    )
}

/// Result of a batch zone-move (`move_objects_simultaneously`).
pub(crate) enum BatchMoveResult {
    /// Every requested object was delivered.
    Done,
    /// A per-object `Moved` replacement surfaced a CR 616.1 choice mid-batch.
    /// `state.waiting_for` is already parked (with the choosing player) and the
    /// undelivered tail is stashed in `state.pending_batch_deliveries`, so the
    /// caller only needs to know that it paused — the resume path
    /// (`drain_pending_batch_deliveries`) finishes the batch.
    NeedsChoice,
}

/// CR 603.10a batch entry: move many objects to one destination through the
/// pipeline as a single simultaneous departure batch (the mill / mass-bounce /
/// SBA pattern). Each object runs through `move_object`, so per-object `Moved`
/// redirects (Rest in Peace / Leyline of the Void class) fire on every one;
/// after the batch completes, CR 603.10a co-departure is stamped over the
/// attempted set. This is universally safe for non-battlefield origins such as
/// a mill: `departed_subset` DOES include the milled cards (it filters on
/// current zone != Battlefield, and a card now in a graveyard passes), but
/// `mark_simultaneous_departures` only stamps `ZoneChanged` events whose
/// `from` is `Some(Zone::Battlefield)` (the zones.rs event gate) — a
/// library-origin move produces no such event, so nothing is stamped.
///
/// On a mid-batch CR 616.1 ordering choice the surfaced prompt is parked and the
/// undelivered tail is stashed in `state.pending_batch_deliveries`; the resume
/// path drains it (`drain_pending_batch_deliveries`). The simultaneous-departure
/// stamp is applied per delivered segment (the realistic single-redirect path
/// never pauses, so the full batch is stamped together; only two simultaneous
/// `Moved` redirects on one object can split a batch — no parsed card does, so
/// the per-segment co-departure grouping in that doubly-rare case is acceptable
/// and documented rather than threaded across the pause boundary).
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
/// a pause can never run it early or twice.
pub(crate) fn move_objects_simultaneously_then(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    completion: Option<BatchCompletion>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    let ids: Vec<ObjectId> = reqs.iter().map(|r| r.object_id).collect();
    let destination = reqs.first().map(|r| r.to);
    match deliver_batch(state, reqs, &ids, events) {
        BatchMoveResult::Done => {
            // Synchronous completion (the common single-redirect path): run the
            // cleanup now.
            if let Some(completion) = completion {
                run_batch_completion(state, completion, events);
            }
            BatchMoveResult::Done
        }
        BatchMoveResult::NeedsChoice => {
            // Paused mid-pile. `deliver_batch` stashed the undelivered tail when
            // it was non-empty; when the paused object was the LAST in the batch
            // the tail is empty and nothing was stashed. Either way, ensure a
            // pending record carries the completion so the drain runs it once the
            // paused object's redirect resolves. `destination` is irrelevant for
            // an empty tail (no object re-delivers), so the first request's
            // destination is a safe placeholder.
            if let Some(completion) = completion {
                ensure_batch_record(state, destination.unwrap_or(Zone::Graveyard)).completion =
                    Some(completion);
            }
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
) {
    crate::game::engine_resolution_choices::run_batch_completion(state, completion, events);
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

/// Return the live parked-batch record, creating an empty-tail one (the
/// paused-on-last-card case) if `deliver_batch` did not stash a tail. Used only
/// to hang a [`BatchCompletion`] off a paused batch.
fn ensure_batch_record(state: &mut GameState, destination: Zone) -> &mut PendingBatchDeliveries {
    state
        .pending_batch_deliveries
        .get_or_insert_with(|| PendingBatchDeliveries {
            remaining: Vec::new(),
            destination,
            source_id: None,
            enter_tapped: EtbTapState::Unspecified,
            exile_tracking: ZoneDeliveryExileTracking::None,
            completion: None,
        })
}

/// CR 603.10a + CR 616.1: shared batch delivery loop. Runs each request through
/// `move_object`; on a pause, parks the prompt and stashes the undelivered tail
/// (rebuilt as `Effect`-cause requests to the same destination — the mill /
/// mass-bounce attribution). `attempted` is the full id set whose departed
/// subset is stamped on completion of this segment.
fn deliver_batch(
    state: &mut GameState,
    reqs: Vec<ZoneMoveRequest>,
    attempted: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    let mut queue = reqs.into_iter();
    while let Some(req) = queue.next() {
        let destination = req.to;
        match move_object(state, req, events) {
            ZoneMoveResult::Done => {}
            ZoneMoveResult::NeedsChoice(_) => {
                // CR 616.1: `move_object` already parked the surfaced prompt
                // (centralized park at its `replace_event` NeedsChoice arm);
                // stash the rest of the batch so no object strands. The paused
                // object rides in `state.pending_replacement` and is delivered
                // by the resume path.
                stash_batch_tail(state, queue.collect(), destination);
                return BatchMoveResult::NeedsChoice;
            }
            ZoneMoveResult::NeedsAuraAttachmentChoice => {
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
                // `pending_batch_deliveries.is_some()`, so the aura-attachment
                // pause finishes the parked batch the same way the replacement-
                // choice resume does. (Updated for d5a12b8c6, which added the
                // aura-resume drain; the prior note here that the tail would be
                // "silently drained by the NEXT unrelated resume" is no longer
                // accurate.)
                stash_batch_tail(state, queue.collect(), destination);
                return BatchMoveResult::NeedsChoice;
            }
        }
    }
    // CR 603.10a + CR 608.2f: every object that actually left the battlefield in
    // this segment departed together — stamp co-departure so leaves-the-
    // battlefield observers among the group see each other via last-known info.
    // For non-battlefield origins (mill) this is a no-op via the EVENT gate, not
    // the subset filter: `departed_subset` includes milled cards (their current
    // zone — graveyard — is not Battlefield), but `mark_simultaneous_departures`
    // only stamps `ZoneChanged` events with `from: Some(Zone::Battlefield)`, and
    // a library-origin move emits none.
    zones::mark_simultaneous_departures(events, &zones::departed_subset(state, attempted));
    BatchMoveResult::Done
}

/// CR 603.10a + CR 616.1: Park the undelivered batch tail so the resume path
/// can finish it. Captures the batch-uniform request context (CR 400.7
/// attribution source, CR 614.1c tap-state, exile tracking) from the first tail
/// request so the rebuilt requests are equivalent to the originals — without
/// this the re-stash collapsed every tail request to
/// `ZoneMoveRequest::effect(obj, dest, obj)`, dropping seek's `enter_tapped`
/// mod and ability-source attribution across the pause boundary.
///
/// Batch-uniform contract (mirrors the single-`destination` design): every
/// batch caller builds requests with one shared mod/attribution set, so the
/// first tail request is representative. A request whose source equals its own
/// `object_id` is the self-anchor idiom (mill) and stashes `source_id: None` so
/// the drain re-anchors each object to itself.
fn stash_batch_tail(state: &mut GameState, tail: Vec<ZoneMoveRequest>, destination: Zone) {
    let Some(first) = tail.first() else {
        return;
    };
    let source_id = first.source().filter(|&s| s != first.object_id);
    let enter_tapped = first.mods.enter_tapped;
    let exile_tracking = first.exile_links.tracking;
    state.pending_batch_deliveries = Some(PendingBatchDeliveries {
        remaining: tail.into_iter().map(|r| r.object_id).collect(),
        destination,
        source_id,
        enter_tapped,
        exile_tracking,
        // The post-loop cleanup (if any) is attached by the batch caller after
        // it observes the `NeedsChoice`; `move_objects_simultaneously` itself
        // has no completion to stash.
        completion: None,
    });
}

/// CR 603.10a + CR 616.1: Resume a parked batch-delivery tail after the
/// per-object replacement choice that paused it resolved (and its object's
/// chosen event delivered). Re-parks — leaving `state.waiting_for` set — when
/// the next object surfaces its own prompt. Rebuilds each tail request with the
/// stashed batch-uniform context (attribution source, tap-state, exile
/// tracking) so the resumed deliveries match the originals.
pub(crate) fn drain_pending_batch_deliveries(state: &mut GameState, events: &mut Vec<GameEvent>) {
    if let Some(pending) = state.pending_batch_deliveries.take() {
        let completion = pending.completion;
        let ids = pending.remaining.clone();
        let reqs: Vec<ZoneMoveRequest> = pending
            .remaining
            .into_iter()
            .map(|obj_id| {
                let mut req = ZoneMoveRequest::effect(
                    obj_id,
                    pending.destination,
                    pending.source_id.unwrap_or(obj_id),
                );
                req.mods.enter_tapped = pending.enter_tapped;
                req.exile_links.tracking = pending.exile_tracking;
                req
            })
            .collect();
        let destination = pending.destination;
        match deliver_batch(state, reqs, &ids, events) {
            BatchMoveResult::Done => {
                // CR 603.10a + CR 616.1: the whole pile has now landed. Run the
                // post-loop cleanup exactly once on true completion (it never ran
                // inline because the loop paused). `Done` here is reachable only
                // when `deliver_batch` did NOT re-park, so the completion fires at
                // most once per batch.
                if let Some(completion) = completion {
                    run_batch_completion(state, completion, events);
                }
            }
            BatchMoveResult::NeedsChoice => {
                // Re-parked on the next object's CR 616.1 choice;
                // `deliver_batch` stashed a fresh tail (or, when the re-paused
                // object was the last in the tail, stashed nothing — create an
                // empty record). Re-attach the cleanup so it survives the next
                // pause boundary and runs once the remaining tail finally drains.
                if let Some(completion) = completion {
                    ensure_batch_record(state, destination).completion = Some(completion);
                }
            }
        }
    }
}

/// Deliver an event that already passed the replacement consult. Only callable
/// with the `ApprovedZoneChange` proof token. This is the renamed
/// `deliver_replaced_zone_change`; Phase A exposes it for the Phase B bucket-A
/// migration (it is not yet called through the token by any production site).
#[allow(dead_code)]
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
    events: &mut Vec<GameEvent>,
) -> ZoneDeliveryResult {
    // CR 701.24a: To shuffle a library, randomize the cards within it so that
    // no player knows their order.
    if to == Zone::Library {
        let owner = state.objects.get(&object_id).map(|o| o.owner);
        if let Some(owner) = owner {
            shuffle_library(state, owner, events);
        }
    }
    // Track cards exiled by the source. Some linked exiles return when the
    // source leaves; others are just remembered as "exiled with" the source.
    if to == Zone::Exile {
        if let Some(source_id) = cause.or(source_id) {
            let kind = match duration {
                Some(Duration::UntilHostLeavesPlay) => {
                    ExileLinkKind::UntilSourceLeaves { return_zone: from }
                }
                _ if matches!(exile_tracking, ZoneDeliveryExileTracking::TrackBySource) => {
                    ExileLinkKind::TrackedBySource
                }
                _ => return ZoneDeliveryResult::Done,
            };
            state.exile_links.push(ExileLink {
                exiled_id: object_id,
                source_id,
                kind,
            });
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
    if state.post_replacement_continuation.is_some() {
        let waiting_for = crate::game::engine_replacement::apply_pending_post_replacement_effect(
            state,
            Some(object_id),
            None,
            Some(crate::types::replacements::ReplacementEvent::Moved),
            events,
        );
        if matches!(waiting_for, Some(WaitingFor::EffectZoneChoice { .. })) {
            return replacement_pause_delivery_result(state);
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
    let mut targets: Vec<TargetRef> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| *id != aura_id)
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
        obj.back_face = Some(original);
    }
}

/// Deliver a zone-change event that has already passed through replacement.
pub(crate) fn deliver_replaced_zone_change(
    state: &mut GameState,
    event: ProposedEvent,
    source_id: Option<ObjectId>,
    duration: Option<&Duration>,
    track_exiled_by_source: bool,
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
        ..
    } = event
    {
        let exile_tracking = if track_exiled_by_source {
            ZoneDeliveryExileTracking::TrackBySource
        } else {
            ZoneDeliveryExileTracking::None
        };

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
            && state.devour_eligible_snapshot.is_none()
            && crate::game::engine_replacement::object_has_devour_replacement(state, object_id)
        {
            state.devour_eligible_snapshot = Some(state.battlefield.iter().copied().collect());
        }

        zones::move_to_zone(state, object_id, to, events);
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
        if to == Zone::Battlefield {
            if let Some(profile) = &face_down_profile {
                apply_face_down_entry_profile(state, object_id, profile);
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
            events,
        );
    }
    ZoneDeliveryResult::Done
}

fn replacement_pause_delivery_result(state: &GameState) -> ZoneDeliveryResult {
    match &state.waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => ZoneDeliveryResult::NeedsChoice(*player),
        // CR 614.12a: a Devour as-enters sacrifice surfaced its own
        // `EffectZoneChoice`; carry its chooser so the caller's `park_waiting_for`
        // doesn't clobber the already-surfaced prompt.
        WaitingFor::EffectZoneChoice { player, .. } => ZoneDeliveryResult::NeedsChoice(*player),
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
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    let mut proposed = ProposedEvent::zone_change(obj_id, from_zone, dest_zone, Some(source_id));

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

    // CR 306.5b + CR 310.4b + CR 614.1c: Seed the intrinsic "enters with N
    // counters" replacement when a planeswalker or battle enters the
    // battlefield from any source (effect-driven entry — bounce-return,
    // reanimate, blink, etc.). Spell-cast entry is handled in stack.rs.
    if dest_zone == Zone::Battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
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
                    crate::game::printed_cards::intrinsic_face_counters(back.loyalty, back.defense)
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
                            [] => return ZoneMoveResult::Done,
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
                match deliver_replaced_zone_change(
                    state,
                    event,
                    Some(source_id),
                    duration,
                    track_exiled_by_source,
                    events,
                ) {
                    ZoneDeliveryResult::Done => {}
                    ZoneDeliveryResult::NeedsChoice(player) => {
                        return ZoneMoveResult::NeedsChoice(player);
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
                return ZoneMoveResult::NeedsAuraAttachmentChoice;
            }
            match deliver_replaced_zone_change(
                state,
                event,
                Some(source_id),
                duration,
                track_exiled_by_source,
                events,
            ) {
                ZoneDeliveryResult::Done => {}
                ZoneDeliveryResult::NeedsChoice(player) => {
                    return ZoneMoveResult::NeedsChoice(player);
                }
            }
            ZoneMoveResult::Done
        }
        ReplacementResult::Prevented => ZoneMoveResult::Done,
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
            ZoneMoveResult::NeedsChoice(player)
        }
    }
}
