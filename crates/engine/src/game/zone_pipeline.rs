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
    ExileLink, ExileLinkKind, GameState, PendingCounterPostAction, WaitingFor,
    ZoneDeliveryExileTracking,
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
/// a cross-crate consumer exists, which it does not in Phase A.)
#[allow(dead_code)]
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

    // Phase A: `placement` integration (folding the raw
    // `move_to_library_position` / `move_to_library_at_index` siblings in) is
    // Phase D. Until then a `Some` placement preserves today's behavior of those
    // siblings — a direct, replacement-bypassing library reposition — so there
    // is zero behavior change for the (currently nonexistent) Phase A callers
    // that would pass one. Cross-zone replacement-respecting library placement
    // is wired when the sibling call sites migrate.
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
        req.mods.enter_tapped.is_tapped(),
        req.mods.controller_override,
        &req.mods.enter_with_counters,
        req.mods.face_down_profile.as_ref(),
        track_exiled_by_source,
        events,
    )
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
        // card's. Mirror `manifest_card`'s sequence: snapshot the real face into
        // `back_face`, overwrite with the face-down 2/2 (+ any specified extra
        // types/subtypes), then store the snapshot so the original is restorable.
        // Done before the controller-override and ETB-counter/trigger blocks
        // below so triggers (if any later applied) see the face-down state.
        if to == Zone::Battlefield {
            if let Some(profile) = &face_down_profile {
                if let Some(obj) = state.objects.get_mut(&object_id) {
                    let original = crate::game::printed_cards::snapshot_object_face(obj);
                    crate::game::morph::apply_face_down_creature_characteristics(obj, profile);
                    obj.back_face = Some(original);
                }
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
    effect_enter_tapped: bool,
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

    // CR 614.1: Set enter_tapped on the proposed event so replacement effects preserve it.
    if effect_enter_tapped {
        if let ProposedEvent::ZoneChange {
            enter_tapped: ref mut et,
            ..
        } = proposed
        {
            *et = crate::types::proposed_event::EtbTapState::Tapped;
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
        ReplacementResult::NeedsChoice(player) => ZoneMoveResult::NeedsChoice(player),
    }
}
