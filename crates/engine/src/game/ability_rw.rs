//! CR 603.3b: the PR-6.75 read/write **conflict** profiler — a second
//! compiler-exhaustive, wildcard-free walk of a resolved ability's typed AST,
//! sibling to `ability_scan.rs`. Where `ability_scan` answers three 1-bit
//! read-axis questions, this module answers the richer question the legacy
//! trigger-ordering paths need (§1.2 of the PR-6.75 plan, PR-6.25 §3 C0(ii)):
//! *which kinds of game state does an ability READ, which does it WRITE, and at
//! what scope* — so `group_is_order_independent` can auto-order a same-event or
//! co-departure group of normalized-identical siblings only when their
//! resolution functions provably commute (CR 603.3b), and prompt otherwise.
//!
//! The gate predicate is fail-closed and **kind/scope-aware**: two identical
//! siblings conflict iff one member's WRITE lands in a location class the other
//! member's LIVE read observes (a read/write *feed*). Members are normalized-
//! identical, so a sibling's write-set equals my own.
//!
//! # Soundness scope (documented residual)
//!
//! Commutation is proven **modulo the source-actor residual** (§1.2 side
//! condition): per-source granted state (lifelink CR 702.15 / deathtouch
//! CR 702.2), the `DamagedPlayerIsEventSourceOwner` referent (a damage-cause
//! source-owner read, `types/ability.rs`), and the CR 800.4a player-loss
//! object-removal cascade modulate resolution without appearing in the normalized
//! AST or any profiled read. Both channels were auto-ordered UNCONDITIONALLY by
//! the pre-C1 short-circuit and are inherited unchanged (zero ordering-decision
//! change). Damage kinds are therefore RECIPIENT-classified, not source-bound
//! (CR 704.5a / CR 800.4a). PR-6.75 c5 note: the batch-T1 path now clears at
//! `Uniform` (controller-equal) as well as `UniformAligned` (owner-equal), so —
//! unlike the owner-aligned span gate — it no longer equalizes source OWNER across
//! members. The `DamagedPlayerIsEventSourceOwner` / lifelink / deathtouch residual
//! exposure on the batch path is thus identical to the same-event T1 path already
//! documented, and bounded by the §7.1 `old=false, new=true` zero-movement grep.
//!
//! ## The event-object read/write feed (R4 review) — CLOSED at both depths
//!
//! A same-event group that WRITES the shared triggering object and then READS its
//! LIVE characteristic ("put a +1/+1 counter on it, then transform ~ if that
//! creature's power ≥ 6" ×2) is order-observable (CR 608.2h: the read uses the event
//! object's CURRENT information). `reads_event_live` records no KindSet, so the
//! `feeds()` matrix is structurally blind to it — the feed is instead gated by
//! `reads_and_writes_event_object` (`reads_event_live && writes_event_object.any()`):
//! the same-event discriminator (`s.same_event && s.event_object_present &&
//! reads_and_writes_event_object`) PROMPTS it, and the T1 fast-path disjunct excludes
//! it. The BATCH depth is protected differently: a co-departure event object is a
//! DEPARTED, FROZEN LKI (CR 603.10a) — a write no-ops (stable read ⇒ auto correct) or
//! is a reentry hazard ⇒ the freeze-invalidation row prompts. The `event_object_present`
//! conjunct mirrors `effective_external` (a write to a non-present event object no-ops,
//! targeting.rs:951), so a Phase-mode trigger stays auto (no live object ⇒ no feed).
//!
//! ## Residuals that REMAIN — both SYMMETRIC across batch and same-event depths
//!
//! 1. The **source-actor residual** (above): per-source granted state and the
//!    CR 800.4a player-loss cascade, invisible to the normalized AST.
//! 2. A **board-wide external write × event-live read**: a `writes_external` write NOT
//!    scoped to the event object (e.g. "each creature") that happens to mutate the
//!    event object, feeding an event-live read. `reads_and_writes_event_object` keys on
//!    `writes_event_object` (event-object-SCOPED writes) only, and `feeds()` stays blind
//!    (the live read carries no KindSet) — so this is uncaught. It is SYMMETRIC: the
//!    batch T1 conjunct also keys on `writes_event_object` (not `writes_external`), so
//!    the batch depth is equally open. Closing it needs either KindSet-recording
//!    event-live reads (so `feeds()` catches board writes) or promoting any
//!    `writes_external` to an event-object write under `reads_event_live` (a large
//!    over-prompt) — out of scope; documented-open.
//!
//! Both residuals are profile-INVISIBLE (the dependency is absent from the profiled
//! read/write sets) and equal-strength across depths. The gate is therefore sound
//! MODULO these two symmetric residuals — the exact scope the `group_is_order_
//! independent` contract (triggers.rs) records.
//!
//! # M3 binding mandate (review-blocking)
//!
//! Every NON-fully-conservative arm binds ALL payload fields of its variant;
//! `{ .. }` field elision is permitted ONLY on arms whose RHS is
//! maximal-conservative (`RwProfile::conservative()`). A precise arm that elides
//! a field would classify whatever that field carries as nothing — fail-OPEN
//! (the inc1 5-hole class). Same discipline as `ability_scan.rs`.
//!
//! # Traversal closure & fail-closed defaults
//!
//! Closed under payload reachability across the same type set as `ability_scan`
//! (`Effect`, `QuantityRef`, `QuantityExpr`, `AbilityCondition`,
//! `TriggerCondition`, `TargetFilter`, `ObjectScope`, `PlayerFilter`,
//! `PlayerScope`, `ControllerRef`, `CountScope`, `StaticCondition`, `Duration`),
//! plus the choice/RNG `AbilityDefinition` sub-bodies (§2 choice-wrapper / RNG
//! union descent). A future variant must fail to compile until classified. An
//! effect KIND absent from the plan's §1.3.1-D group-reachable histogram (zero
//! printed presence, nothing to flip) may take `RwProfile::conservative()`.
//! Sub-enums the walk does not descend (`FilterProp` interiors,
//! `PermissionGrantee`, `CombineSource`, `DamageSource`) are handled like
//! `ability_scan`'s conservative subtrees; the §5.2 parity sweep (commit 2) is
//! the arbiter for any D5 tag that hides only there.
//!
//! CR annotations: CR 603.3b (gate), CR 603.10a + CR 400.7 (frozen-read kinds +
//! freeze-invalidation row), CR 603.4 (condition inclusion), CR 603.5
//! (resolution-time-choice exclusions + Mana×unless-pay guard), CR 603.7
//! (deferred-body arms), CR 707.10 / CR 707.10c (CopySpell), CR 707.2
//! (CopyTokenOf template read), CR 702.15 / CR 702.2 / CR 704.5a / CR 800.4a
//! (source-actor residual).

// Consumers landed in commit 2: `game::triggers::group_is_order_independent`
// calls `ability_rw_profile` / `trigger_condition_rw_profile` / `profiles_conflict`
// on the legacy same-event and departure-batch ordering paths (CR 603.3b).

use crate::types::ability::FilterProp;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, ContinuousModification, ControllerRef, Duration, Effect,
    GuessSubject, ModalChoice, MultiTargetSpec, ObjectScope, PlayerFilter, PlayerScope,
    QuantityExpr, QuantityRef, RepeatContinuation, ResolvedAbility, StaticCondition,
    StaticDefinition, TargetFilter, TriggerCondition, TriggerDefinition, TypeFilter, TypedFilter,
    ZoneRef,
};
use crate::types::game_state::TargetSelectionConstraint;
use crate::types::zones::Zone;
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// State-kind lattice (§ D-profile).
// ---------------------------------------------------------------------------

/// A class of mutable game state, for CR 603.3b sibling-conflict analysis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StateKind {
    /// CR 208/209: object power/toughness (live, after layers).
    ObjectPt,
    /// CR 122.1: counters on an object.
    ObjectCounters,
    /// CR 400: zone membership / control (which objects are where / whose).
    SetMembership,
    /// CR 119: player life (and energy/poison/player counters CR 122.1, folded
    /// here as monotone player resources).
    PlayerLife,
    /// CR 401/402: hand + library contents/order.
    HandLibrary,
    /// CR 119.3: per-turn life-change journal (fed by `PlayerLife` writes).
    JournalLife,
    /// CR 120.6: per-turn draw/discard journal (fed by `HandLibrary` writes).
    JournalCards,
    /// CR 601: per-turn/per-game cast journal (no in-resolution write feeds it).
    JournalCast,
    /// CR 405: the stack's shape (copies pushed, spells countered).
    StackShape,
    /// CR 301.5/302.6: tap state.
    TapState,
    /// CR 500 (turn structure) + CR 506.4c (remove from combat) + CR 614.10
    /// (skip step/turn): game-sequencing writes — extra turns/phases, skipped
    /// steps, combat removal. Idempotent/additive among identical siblings (two
    /// extra turns commute; removing an already-removed creature is a no-op) and
    /// observed by NO profiled read, so this kind only self-conflicts (a future
    /// sequencing READ would fail-closed against it) — never the `Other`
    /// catch-all, which conflicts with every read.
    TurnStructure,
    /// Unclassifiable — conflicts with everything (fail-closed).
    Other,
}

/// Explicit-bool set over `StateKind` — mirrors `Axes`' style (no bitmagic).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct KindSet {
    object_pt: bool,
    object_counters: bool,
    set_membership: bool,
    player_life: bool,
    hand_library: bool,
    journal_life: bool,
    journal_cards: bool,
    journal_cast: bool,
    stack_shape: bool,
    tap_state: bool,
    turn_structure: bool,
    other: bool,
}

impl KindSet {
    const EMPTY: KindSet = KindSet {
        object_pt: false,
        object_counters: false,
        set_membership: false,
        player_life: false,
        hand_library: false,
        journal_life: false,
        journal_cards: false,
        journal_cast: false,
        stack_shape: false,
        tap_state: false,
        turn_structure: false,
        other: false,
    };
    const ALL: KindSet = KindSet {
        object_pt: true,
        object_counters: true,
        set_membership: true,
        player_life: true,
        hand_library: true,
        journal_life: true,
        journal_cards: true,
        journal_cast: true,
        stack_shape: true,
        tap_state: true,
        turn_structure: true,
        other: true,
    };

    fn one(k: StateKind) -> KindSet {
        let mut s = KindSet::EMPTY;
        s.set(k);
        s
    }
    fn set(&mut self, k: StateKind) {
        match k {
            StateKind::ObjectPt => self.object_pt = true,
            StateKind::ObjectCounters => self.object_counters = true,
            StateKind::SetMembership => self.set_membership = true,
            StateKind::PlayerLife => self.player_life = true,
            StateKind::HandLibrary => self.hand_library = true,
            StateKind::JournalLife => self.journal_life = true,
            StateKind::JournalCards => self.journal_cards = true,
            StateKind::JournalCast => self.journal_cast = true,
            StateKind::StackShape => self.stack_shape = true,
            StateKind::TapState => self.tap_state = true,
            StateKind::TurnStructure => self.turn_structure = true,
            StateKind::Other => self.other = true,
        }
    }
    fn union(self, o: KindSet) -> KindSet {
        KindSet {
            object_pt: self.object_pt || o.object_pt,
            object_counters: self.object_counters || o.object_counters,
            set_membership: self.set_membership || o.set_membership,
            player_life: self.player_life || o.player_life,
            hand_library: self.hand_library || o.hand_library,
            journal_life: self.journal_life || o.journal_life,
            journal_cards: self.journal_cards || o.journal_cards,
            journal_cast: self.journal_cast || o.journal_cast,
            stack_shape: self.stack_shape || o.stack_shape,
            tap_state: self.tap_state || o.tap_state,
            turn_structure: self.turn_structure || o.turn_structure,
            other: self.other || o.other,
        }
    }
    fn minus(self, o: KindSet) -> KindSet {
        KindSet {
            object_pt: self.object_pt && !o.object_pt,
            object_counters: self.object_counters && !o.object_counters,
            set_membership: self.set_membership && !o.set_membership,
            player_life: self.player_life && !o.player_life,
            hand_library: self.hand_library && !o.hand_library,
            journal_life: self.journal_life && !o.journal_life,
            journal_cards: self.journal_cards && !o.journal_cards,
            journal_cast: self.journal_cast && !o.journal_cast,
            stack_shape: self.stack_shape && !o.stack_shape,
            tap_state: self.tap_state && !o.tap_state,
            turn_structure: self.turn_structure && !o.turn_structure,
            other: self.other && !o.other,
        }
    }
    fn is_empty(self) -> bool {
        self == KindSet::EMPTY
    }
    fn any(self) -> bool {
        !self.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Census (§2 census-overlap refinement of the SetMembership same-kind row).
// ---------------------------------------------------------------------------

/// Extractable type-tag requirements (core types + subtypes + token-ness),
/// lowercased into a common tag space.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Census {
    /// No object moved (absent/no-op write) — overlaps nothing.
    None,
    /// Unextractable / unbounded — assume overlap (fail-closed).
    Any,
    /// A concrete positive tag set.
    Tags(BTreeSet<String>),
}

impl Census {
    fn merge(&mut self, o: Census) {
        let taken = std::mem::replace(self, Census::None);
        *self = match (taken, o) {
            (Census::None, x) | (x, Census::None) => x,
            (Census::Any, _) | (_, Census::Any) => Census::Any,
            (Census::Tags(mut a), Census::Tags(b)) => {
                a.extend(b);
                Census::Tags(a)
            }
        };
    }
}

/// CR 205 tag-set overlap: two censuses can name a common object iff they share
/// a tag; `None` overlaps nothing, `Any` overlaps every non-`None`.
fn census_overlap(a: &Census, b: &Census) -> bool {
    match (a, b) {
        (Census::None, _) | (_, Census::None) => false,
        (Census::Any, _) | (_, Census::Any) => true,
        (Census::Tags(x), Census::Tags(y)) => x.intersection(y).next().is_some(),
    }
}

/// The group's live source objects' type census. Read once at the
/// `begin_trigger_ordering` chokepoint. A missing source ⇒ `None` ⇒ overlap
/// assumed (fail-closed).
#[derive(Clone, Debug, Default)]
pub(crate) struct SourceCensus {
    tags: Option<BTreeSet<String>>,
}

impl SourceCensus {
    pub(crate) fn from_tags<I: IntoIterator<Item = String>>(tags: I) -> Self {
        SourceCensus {
            tags: Some(tags.into_iter().map(|t| t.to_lowercase()).collect()),
        }
    }
    pub(crate) fn unknown() -> Self {
        SourceCensus { tags: None }
    }
    fn as_census(&self) -> Census {
        match &self.tags {
            None => Census::Any,
            Some(t) => Census::Tags(t.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// ZoneSpan (§2 census-overlap refinement — the ZONE axis of the SetMembership
// same-kind row; CR 400.1 a zone is where objects live).
// ---------------------------------------------------------------------------

/// CR 400.1: the zone(s) a `SetMembership` read observes / a membership write
/// touches. A whole-zone or `InZone`-free read is `Any` (fail-closed); a
/// creation write touches only its DESTINATION (battlefield for a token); a move
/// is recorded fail-closed `Any` (both endpoints matter but are not tracked
/// precisely). The membership feed row (§2) requires zone overlap IN ADDITION to
/// type-census overlap, so a battlefield-destination token creation cannot feed
/// a graveyard-count read (Tombstone Stairwell). `merge` is fail-closed — `Any`
/// swallows any precise set — so a mix of a precise write and an unrefined `Any`
/// write yields `Any` (never fewer conflicts than a single unrefined write).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum ZoneSpan {
    /// No membership read/write on this side — overlaps nothing.
    None,
    /// Unextractable / untracked — assume overlap (fail-closed).
    Any,
    /// A concrete zone set.
    Zones(std::collections::HashSet<Zone>),
}

impl ZoneSpan {
    fn one(z: Zone) -> ZoneSpan {
        ZoneSpan::Zones(std::iter::once(z).collect())
    }
    fn merge(&mut self, o: ZoneSpan) {
        let taken = std::mem::replace(self, ZoneSpan::None);
        *self = match (taken, o) {
            (ZoneSpan::None, x) | (x, ZoneSpan::None) => x,
            (ZoneSpan::Any, _) | (_, ZoneSpan::Any) => ZoneSpan::Any,
            (ZoneSpan::Zones(mut a), ZoneSpan::Zones(b)) => {
                a.extend(b);
                ZoneSpan::Zones(a)
            }
        };
    }
}

/// CR 400.1 zone overlap: two spans can name a common zone iff they share one;
/// `None` overlaps nothing, `Any` overlaps every non-`None` (mirrors
/// `census_overlap`).
fn zone_overlap(a: &ZoneSpan, b: &ZoneSpan) -> bool {
    match (a, b) {
        (ZoneSpan::None, _) | (_, ZoneSpan::None) => false,
        (ZoneSpan::Any, _) | (_, ZoneSpan::Any) => true,
        (ZoneSpan::Zones(x), ZoneSpan::Zones(y)) => x.intersection(y).next().is_some(),
    }
}

/// CR 400.1: the zones a read filter observes — its explicit `InZone`/`InAnyZone`
/// constraints (`TargetFilter::extract_zones`), or `Any` when it declares none (a
/// bare board read defaults to the battlefield, but we stay fail-closed rather
/// than assume it, so an `InZone`-free read still conflicts with every membership
/// write as before; only a filter with an EXPLICIT zone gets precise treatment).
fn zones_of_filter(f: &TargetFilter) -> ZoneSpan {
    let zones = f.extract_zones();
    if zones.is_empty() {
        ZoneSpan::Any
    } else {
        ZoneSpan::Zones(zones.into_iter().collect())
    }
}

// ---------------------------------------------------------------------------
// PlayerSpan (PR-6.75): gate-scoped relative player-identity span for the
// player-resource and membership-controller feed rows. Mirrors Census/ZoneSpan.
// ---------------------------------------------------------------------------

/// CR 102.2 + CR 109.5: a relative player-identity span for scope-gated feed
/// refinement. Meaningful ONLY under `ControllerUniformity::UniformAligned` (all
/// members' relative sets `You`/`Opponents` then denote the SAME concrete players
/// — `You == {c0}`, `Opponents == all − {c0}`, disjoint at any player count). Dead
/// otherwise: `profiles_conflict` never consults a span when the gate is off
/// (not `UniformAligned`), so every value is byte-inert in the ungated path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PlayerSpan {
    /// No player-keyed read/write on this side — overlaps nothing. Also the
    /// top-level "unscoped" pscope sentinel (no enclosing per-player iteration).
    None,
    /// The controller `c0` (CR 109.5 "you/your").
    You,
    /// Every opponent of `c0` (CR 102.2/102.3).
    Opponents,
    /// Unextractable / mixed / unrefined — assume overlap (fail-closed).
    Any,
}

impl PlayerSpan {
    /// Mixing `You`+`Opponents` or anything+`Any` ⇒ `Any`; `None` is identity.
    fn merge(self, o: PlayerSpan) -> PlayerSpan {
        match (self, o) {
            (PlayerSpan::None, x) | (x, PlayerSpan::None) => x,
            (PlayerSpan::Any, _) | (_, PlayerSpan::Any) => PlayerSpan::Any,
            (PlayerSpan::You, PlayerSpan::You) => PlayerSpan::You,
            (PlayerSpan::Opponents, PlayerSpan::Opponents) => PlayerSpan::Opponents,
            (PlayerSpan::You, PlayerSpan::Opponents) | (PlayerSpan::Opponents, PlayerSpan::You) => {
                PlayerSpan::Any
            }
        }
    }
}

/// CR 102.2 + CR 109.5 relative-player overlap: two spans can name a common
/// player iff — under `UniformAligned` — their relative sets intersect. `None`
/// overlaps nothing; `Any` overlaps every non-`None`; `You`×`Opponents` is the
/// only disjoint concrete pair (mirrors `census_overlap`).
fn player_span_overlap(a: PlayerSpan, b: PlayerSpan) -> bool {
    match (a, b) {
        (PlayerSpan::None, _) | (_, PlayerSpan::None) => false,
        (PlayerSpan::Any, _) | (_, PlayerSpan::Any) => true,
        (PlayerSpan::You, PlayerSpan::You) | (PlayerSpan::Opponents, PlayerSpan::Opponents) => true,
        (PlayerSpan::You, PlayerSpan::Opponents) | (PlayerSpan::Opponents, PlayerSpan::You) => {
            false
        }
    }
}

/// Effective-Any rule (§4.1 fail-closed): a span consulted while its kind-bit is
/// present in the effective set but whose value was never refined (`None`) reads
/// as `Any`. This is what lets the ~40 `ext_write`/`life_writes` and other
/// unrefined callers stay conservative WITHOUT edits — an unrefined player /
/// membership read or write overlaps every player.
fn effective_span(span: PlayerSpan, kind_present: bool) -> PlayerSpan {
    if kind_present && span == PlayerSpan::None {
        PlayerSpan::Any
    } else {
        span
    }
}

/// CR 108.3 + CR 110.2: the controller-field span of a membership write set —
/// external membership ctrl when an external membership write is present, merged
/// with `You` when a self membership write is in scope (the chokepoint guarantees
/// `obj.controller == c0`). Mirrors `membership_census_of`.
fn membership_ctrl_of(
    write_kinds: KindSet,
    external_ctrl: PlayerSpan,
    self_in_scope: bool,
) -> PlayerSpan {
    let mut c = PlayerSpan::None;
    if write_kinds.set_membership {
        c = c.merge(external_ctrl);
    }
    if self_in_scope {
        c = c.merge(PlayerSpan::You);
    }
    c
}

/// CR 109.5 / CR 102.2: relative player span of a `PlayerScope` (HandSize/player
/// reads). `ScopedPlayer` is a documented ceiling — not threaded into the quantity
/// walk — and every non-{Controller,Opponent} scope is a multi-player / unrefined
/// population ⇒ `Any` (fail-closed).
fn player_span_of_scope(ps: &PlayerScope) -> PlayerSpan {
    match ps {
        PlayerScope::Controller => PlayerSpan::You,
        PlayerScope::Opponent { .. } => PlayerSpan::Opponents,
        _ => PlayerSpan::Any,
    }
}

/// CR 109.5 / CR 102.2: relative player span of a `ControllerRef` (membership
/// read/write controller key). `You`⇒You, `Opponent`⇒Opponents, everything else
/// (ScopedPlayer/Target/Chosen/… — unrefined or multi-player) ⇒ `Any`.
fn player_span_of_ctrl_ref(cr: &ControllerRef) -> PlayerSpan {
    match cr {
        ControllerRef::You => PlayerSpan::You,
        ControllerRef::Opponent => PlayerSpan::Opponents,
        _ => PlayerSpan::Any,
    }
}

/// CR 108.3 / CR 110.2: relative player span of a filter's controller key. Only a
/// bare `Typed` filter carries an unambiguous controller; composite / broad
/// filters ⇒ `Any` (fail-closed).
fn ctrl_span_of_filter(f: &TargetFilter) -> PlayerSpan {
    match f {
        TargetFilter::Typed(tf) => tf
            .controller
            .as_ref()
            .map_or(PlayerSpan::Any, player_span_of_ctrl_ref),
        _ => PlayerSpan::Any,
    }
}

/// CR 109.4 / CR 109.5: relative player span of an `AbilityDefinition`/
/// `ResolvedAbility` `player_scope` (the per-player iteration context threaded
/// through the walk). `Controller`⇒You, `Opponent`⇒Opponents; every other filter
/// (`All`/`DefendingPlayer`/`AllExcept`/… — may include or vary across players)
/// ⇒ `Any` (fail-closed).
fn player_span_of_filter(pf: &PlayerFilter) -> PlayerSpan {
    match pf {
        PlayerFilter::Controller => PlayerSpan::You,
        PlayerFilter::Opponent => PlayerSpan::Opponents,
        _ => PlayerSpan::Any,
    }
}

/// CR 400.3 (+ engine owner-default battlefield entry, `change_zone.rs:79`): a
/// `SearchLibrary` of the controller's OWN library (no `target_player`, or a
/// `Controller`-form one) selects only cards whose owner is the controller, so a
/// chained battlefield entry enters under `You`. Emitted as the chain move-owner
/// fact (threaded like `chain_root`) so the ChangeZone consumer can claim a `You`
/// membership-controller span. A search of another player's library (`target_player
/// Some(other)`) yields no fact ⇒ the consumer stays `Any`.
///
/// §10.4 ENGINE COUPLING: this fact plus the `ChangeZone` consumer below are the
/// TWO annotated ends of the entry-default coupling — if `change_zone.rs:79`'s
/// "None keeps the default (owner's control)" ever changes to the CR 110.2a
/// instructing-player default, this span rule must be revisited.
fn effect_move_owner(x: &Effect) -> Option<PlayerSpan> {
    match x {
        Effect::SearchLibrary { target_player, .. }
            if target_player.is_none()
                || matches!(target_player, Some(TargetFilter::Controller)) =>
        {
            Some(PlayerSpan::You)
        }
        _ => None,
    }
}

/// The opaque forwarded move-target class (`Any`/`ParentTarget`) whose concrete
/// cards are supplied by an earlier chain link (e.g. the search selection), so its
/// controller span is carried by the inherited chain move-owner fact rather than
/// the filter itself.
fn is_opaque_forwarded_target(f: &TargetFilter) -> bool {
    matches!(
        f,
        TargetFilter::Any | TargetFilter::ParentTarget | TargetFilter::ParentTargetSlot { .. }
    )
}

// ---------------------------------------------------------------------------
// RwProfile (§2 D-profile).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) struct RwProfile {
    /// Source-scoped reads ONLY (live unless the structure freezes them, CR
    /// 603.10a). Recipient reads are NOT recorded — a Recipient read is the
    /// write's own modify-input (read-modify-write); identical members' composed
    /// per-object totals are symmetric (Canopy Gargantuan proof §1.3.1-G).
    reads_src: KindSet,
    /// Board-/graveyard-/stack-top-scoped mutable reads. Aggregates carry
    /// `SetMembership` alongside their value kind, so membership writes feed them.
    reads_board: KindSet,
    /// Life totals / hand size / journals — sibling-mutable player state.
    reads_player: KindSet,
    /// CR 603.10a look-back class: `HadCounters`, source cast-time facts. Never
    /// conflict WHILE THE FREEZE IS VALID (see `writes_reentry_hazard`).
    reads_frozen: KindSet,
    /// An event-context read: a LIVE event-object characteristic (`EventSource`/
    /// `EventTarget`) OR a frozen event-context amount (`EventContextAmount`/
    /// `EventOutcomeWon`/… via `legacy_ref`) — the bit does not distinguish them.
    /// Consulted by T1's fast path, the freeze-invalidation row, and the same-event
    /// event-object discriminator (which thus conservatively over-prompts the frozen
    /// sub-case — a write can't feed a frozen amount, so it stays order-invariant).
    reads_event_live: bool,
    /// D5: one of the 12 retained-prompt event-context refs is present.
    /// Consulted ONLY by the batch branch (commit 2).
    legacy_batch_prompt: bool,
    /// CR 603.10a (PR-6.75 c5): the resolution consumes a binding resolved per
    /// member instance — per-source tracked/chosen storage, an attachment, or an
    /// event/replacement-context referent outside the legacy-12 /
    /// `writes_event_object` carriers — so members' resolution functions are NOT one
    /// shared `f`. Refutes batch-T1 (`!reads_member_bound` conjunct). Walk-computed
    /// single-bit read fact (precedent: `legacy_batch_prompt`); `drop_writes` keeps
    /// it (a read).
    reads_member_bound: bool,
    /// Writes scoped to the member's own Source/Recipient object.
    writes_self: KindSet,
    /// Board-/player-/stack-scoped writes (INCLUDES creation, event-object, and
    /// LastCreated writes — everything not self). Board reads see all of these.
    writes_external: KindSet,
    /// Sub-portion of `writes_external` targeting the EVENT object
    /// (`TriggeringSource`-class + parentless `ParentTarget`, §2 rule 1/2).
    writes_event_object: KindSet,
    /// Sub-portion of `writes_external` from fresh-id creation / `LastCreated`
    /// (§2 rule 1). Fresh ObjectIds cannot be a sibling's source.
    writes_created: KindSet,
    /// CR 603.10a/B1: an EXTERNAL move of EXISTING objects whose destination is
    /// the battlefield or whose origin is exile — re-enters/overwrites a
    /// departed member's LKI. Feeds the freeze-invalidation row.
    writes_reentry_hazard: bool,
    /// A `SetMembership` write whose census IS the source's typeline, resolved
    /// against `source_census` at conflict time: either a self-scoped move
    /// (`ChangeZone{SelfRef}`) or a `CopyTokenOf{SelfRef}` created copy (CR 707.2,
    /// §1.3.1-F). `membership_census_of` merges `source_census` when this is set.
    writes_membership_self: bool,
    /// CR 400.1: the zone(s) a self-scoped `SetMembership` write touches. A self
    /// MOVE (`ChangeZone{SelfRef}`) records its actual origin+destination; a self
    /// CREATION (`CopyTokenOf{SelfRef}`, CR 111.1) touches only the battlefield.
    /// Previously every self membership write forced `Any` (fail-closed), so a
    /// battlefield self-copy falsely fed a graveyard/departure read (Compy Swarm ×
    /// "a creature died this turn"). `Any` stays the default when a self write's
    /// endpoints are unrecorded, and a self-sacrifice (bf→graveyard) still overlaps
    /// a graveyard read (Chorale of the Void).
    writes_membership_self_zones: ZoneSpan,
    /// Census of external/creation/event-object `SetMembership` writes.
    writes_membership_external_census: Census,
    /// CR 400.1: zones the external/creation/event-object `SetMembership` writes
    /// touch (ZONE axis of the membership feed row — a battlefield creation vs a
    /// graveyard read is zone-disjoint, Tombstone Stairwell).
    writes_membership_external_zones: ZoneSpan,
    /// Census requirements of all `SetMembership` reads.
    reads_membership_census: Census,
    /// CR 400.1: zones all `SetMembership` reads observe (from their filter's
    /// `InZone`; `Any` when unextractable — fail-closed).
    reads_membership_zones: ZoneSpan,
    /// CR 122.1: census of EXTERNAL `ObjectCounters` writes' target filters —
    /// object-scope disjointness for the source-scoped counter read (§2; a quest
    /// read on an enchantment source × a +1/+1 write on creatures is
    /// object-disjoint, Earthbender Ascension). `Any` when unrefined (fail-closed).
    writes_external_counter_census: Census,
    /// A resolution-time payment (`unless_pay` / `PayCost`) is present (CR
    /// 603.5). With `writes_pool`, trips the Mana×unless-pay guard.
    has_pay_or_unless: bool,
    /// The ability writes the mana pool (`Effect::Mana`).
    writes_pool: bool,
    // --- PR-6.75 gate-scoped player-identity spans (consulted ONLY under
    // ControllerUniformity::UniformAligned; every value is byte-inert otherwise). ---
    /// CR 109.5/102.2: relative-player span of the player-resource READS
    /// (`HandSize`; life reads stay unrefined ⇒ effective-Any). A single merged
    /// span across kinds — mixing kinds degrades to `Any` (ceiling documented).
    reads_player_span: PlayerSpan,
    /// CR 109.5/102.2: relative-player span of the player-resource WRITES
    /// (`Discard` scope / self hand-move ⇒ You). Merged across kinds.
    writes_player_span: PlayerSpan,
    /// CR 110.2: controller-field span of all `SetMembership` READS (from the
    /// filter's `controller`; unrefined ⇒ effective-Any).
    reads_membership_ctrl: PlayerSpan,
    /// CR 110.2: controller-field span of EXTERNAL `SetMembership` writes
    /// (SearchLibrary self-library ⇒ You, ChangeZone entry ⇒ chain move-owner).
    /// Self writes contribute `You` via `membership_ctrl_of`. Unrefined external
    /// writes stay `None` here and read as `Any` (effective-Any) at conflict time.
    writes_membership_external_ctrl: PlayerSpan,
    /// §4.4 fused read-modify-write player read (`Discard{count: HandSize{
    /// ScopedPlayer}, target: Controller}` — "discards their hand"). Kept OUT of
    /// `reads_player`/`reads_player_span`: dropped under the gate (identical
    /// members' per-player fixed points compose symmetrically, Canopy Gargantuan
    /// RwProfile doc), unioned back into `reads_player` when ungated (byte-identical).
    reads_player_fused: KindSet,
}

impl RwProfile {
    fn empty() -> RwProfile {
        RwProfile {
            reads_src: KindSet::EMPTY,
            reads_board: KindSet::EMPTY,
            reads_player: KindSet::EMPTY,
            reads_frozen: KindSet::EMPTY,
            reads_event_live: false,
            legacy_batch_prompt: false,
            reads_member_bound: false,
            writes_self: KindSet::EMPTY,
            writes_external: KindSet::EMPTY,
            writes_event_object: KindSet::EMPTY,
            writes_created: KindSet::EMPTY,
            writes_reentry_hazard: false,
            writes_membership_self: false,
            writes_membership_self_zones: ZoneSpan::None,
            writes_membership_external_census: Census::None,
            writes_membership_external_zones: ZoneSpan::None,
            reads_membership_census: Census::None,
            reads_membership_zones: ZoneSpan::None,
            writes_external_counter_census: Census::None,
            has_pay_or_unless: false,
            writes_pool: false,
            reads_player_span: PlayerSpan::None,
            writes_player_span: PlayerSpan::None,
            reads_membership_ctrl: PlayerSpan::None,
            writes_membership_external_ctrl: PlayerSpan::None,
            reads_player_fused: KindSet::EMPTY,
        }
    }

    /// Fail-closed maximal profile for untraversed / unclassified subtrees.
    fn conservative() -> RwProfile {
        let mut p = RwProfile::empty();
        p.reads_board = KindSet::ALL;
        p.writes_self = KindSet::ALL;
        p.writes_external = KindSet::ALL;
        p.writes_membership_external_census = Census::Any;
        p.writes_membership_external_zones = ZoneSpan::Any;
        p.writes_membership_self = true;
        p.writes_membership_self_zones = ZoneSpan::Any;
        p.reads_membership_census = Census::Any;
        p.reads_membership_zones = ZoneSpan::Any;
        p.writes_external_counter_census = Census::Any;
        // PR-6.75: an unclassified subtree may read/write any player ⇒ spans `Any`.
        p.reads_player_span = PlayerSpan::Any;
        p.writes_player_span = PlayerSpan::Any;
        p.reads_membership_ctrl = PlayerSpan::Any;
        p.writes_membership_external_ctrl = PlayerSpan::Any;
        // CR 603.10a: an unclassified subtree may consult a per-member binding ⇒
        // fail-closed (refuses batch-T1).
        p.reads_member_bound = true;
        p
    }

    pub(crate) fn merge(&mut self, o: RwProfile) {
        self.reads_src = self.reads_src.union(o.reads_src);
        self.reads_board = self.reads_board.union(o.reads_board);
        self.reads_player = self.reads_player.union(o.reads_player);
        self.reads_frozen = self.reads_frozen.union(o.reads_frozen);
        self.reads_event_live |= o.reads_event_live;
        self.legacy_batch_prompt |= o.legacy_batch_prompt;
        self.reads_member_bound |= o.reads_member_bound;
        self.writes_self = self.writes_self.union(o.writes_self);
        self.writes_external = self.writes_external.union(o.writes_external);
        self.writes_event_object = self.writes_event_object.union(o.writes_event_object);
        self.writes_created = self.writes_created.union(o.writes_created);
        self.writes_reentry_hazard |= o.writes_reentry_hazard;
        self.writes_membership_self |= o.writes_membership_self;
        self.writes_membership_self_zones
            .merge(o.writes_membership_self_zones);
        self.writes_membership_external_census
            .merge(o.writes_membership_external_census);
        self.writes_membership_external_zones
            .merge(o.writes_membership_external_zones);
        self.reads_membership_census
            .merge(o.reads_membership_census);
        self.reads_membership_zones.merge(o.reads_membership_zones);
        self.writes_external_counter_census
            .merge(o.writes_external_counter_census);
        self.has_pay_or_unless |= o.has_pay_or_unless;
        self.writes_pool |= o.writes_pool;
        self.reads_player_span = self.reads_player_span.merge(o.reads_player_span);
        self.writes_player_span = self.writes_player_span.merge(o.writes_player_span);
        self.reads_membership_ctrl = self.reads_membership_ctrl.merge(o.reads_membership_ctrl);
        self.writes_membership_external_ctrl = self
            .writes_membership_external_ctrl
            .merge(o.writes_membership_external_ctrl);
        self.reads_player_fused = self.reads_player_fused.union(o.reads_player_fused);
    }

    /// CR 603.3b T1 (§1.2): the resolution function never consults the source
    /// binding — no source read, no self write, no source-referential frozen
    /// read. Fail-closed (the walk routes source predicates into `reads_src` /
    /// `reads_frozen`).
    pub(crate) fn source_independent(&self) -> bool {
        self.reads_src.is_empty() && self.writes_self.is_empty() && self.reads_frozen.is_empty()
    }

    /// CR 603.3b + CR 608.2h: the same-event event-object read/write feed — the
    /// resolution WRITES the shared triggering object (`writes_event_object`) AND
    /// READS its live characteristic (`reads_event_live`, which records NO KindSet,
    /// so `feeds()` is structurally blind to this feed). On the same-event path every
    /// member shares ONE live event object (CR 608.2h current-information read), so a
    /// member's write is observed by a sibling's live read ⇒ order-observable.
    /// Fail-closed conjunction of two profiled facts. Consumed by
    /// `profiles_conflict`'s same-event discriminator and (as the class predicate) by
    /// the ordering-parity sweep — a single source of truth (mirrors
    /// `source_independent`).
    pub(crate) fn reads_and_writes_event_object(&self) -> bool {
        self.reads_event_live && self.writes_event_object.any()
    }

    /// D5 (CR 603.10a): true iff one of the 12 retained-prompt event-context refs
    /// is present. Consulted ONLY by the batch branch (`batch_conflict`) to keep
    /// the legacy departure-batch prompting parity (D3 zero widening).
    pub(crate) fn legacy_batch_prompt(&self) -> bool {
        self.legacy_batch_prompt
    }

    /// CR 603.10a: true iff the resolution reads per-member-bound storage
    /// (`member_bound_target_filter` carriers). Test-only read accessor for the
    /// parity sweep's same-event over-prompt CLASS classifier (mirrors the
    /// discriminator gate `profiles_conflict`: `if s.same_event && p.reads_member_bound`).
    /// Production reads the field directly in `profiles_conflict`, so the accessor
    /// exists only for the cross-module `ordering_parity_tests`.
    #[cfg(test)]
    pub(crate) fn reads_member_bound(&self) -> bool {
        self.reads_member_bound
    }

    /// Drop all writes (deferred-body descent, CR 603.7: writes happen
    /// post-window, so reads descend but writes are not counted).
    fn drop_writes(&mut self) {
        self.writes_self = KindSet::EMPTY;
        self.writes_external = KindSet::EMPTY;
        self.writes_event_object = KindSet::EMPTY;
        self.writes_created = KindSet::EMPTY;
        self.writes_reentry_hazard = false;
        self.writes_membership_self = false;
        self.writes_membership_self_zones = ZoneSpan::None;
        self.writes_membership_external_census = Census::None;
        self.writes_membership_external_zones = ZoneSpan::None;
        self.writes_external_counter_census = Census::None;
        self.writes_pool = false;
        self.writes_player_span = PlayerSpan::None;
        self.writes_membership_external_ctrl = PlayerSpan::None;
    }
}

// ---------------------------------------------------------------------------
// GroupStructure.
// ---------------------------------------------------------------------------

/// CR 603.3b + CR 110.2 + CR 108.3 + CR 805.7: the controller structure of an
/// ordering group, as one ordered refinement axis (parameterizes the former
/// `same_controller` + would-be `controllers_uniform` two-correlated-bool sibling
/// cluster). `Mixed` is the fail-closed floor; each higher level unlocks strictly
/// more refinement. Ordering is meaningful only for the two consulting sites
/// (`profiles_conflict` batch-T1 checks `!= Mixed`; the span/fused gate checks
/// `UniformAligned`) — the enum itself is not `Ord`.
///
/// `Mixed` is reachable ONLY via team-pooled trigger placement (CR 805.7):
/// `begin_trigger_ordering` partitions groups by controller (CR 109.5 triggered-
/// ability "you" = the controller when it triggered), so every non-team topology
/// is controller-uniform by construction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ControllerUniformity {
    /// Divergent pending controllers (CR 805.7 team pool). No refinement consulted
    /// ⇒ conflict decision is byte-identical to the pre-uniformity engine.
    Mixed,
    /// Every member's `pending.controller` is one shared player `c0` (CR 109.5).
    /// The relative player-identity spans still cannot be trusted (owner may
    /// diverge), but the batch-T1 identical-function fast path is sound: an f that
    /// consults no source binding, no member-bound storage, and no live event is
    /// one shared f(state, c0).
    Uniform,
    /// `Uniform` AND each live source object is both controlled and owned by `c0`
    /// (CR 108.3 owner). Only then do the relative player-identity spans
    /// (`You`/`Opponents`) denote the same concrete players across members, and
    /// owner-keyed self-write destinations (CR 400.3 hand/graveyard) become
    /// controller-resolvable — the precondition of the span/fused gate.
    UniformAligned,
}

pub(crate) struct GroupStructure {
    /// All members fired on ONE trigger event.
    pub(crate) same_event: bool,
    /// All members share one `source_id`.
    pub(crate) all_same_source: bool,
    /// Every member's trigger is a ZoneChanged-from-battlefield whose object is
    /// that member's own source (departure batch — CR 603.10a frozen reads).
    pub(crate) all_sources_self_departed: bool,
    /// The shared `valid_card` filter provably excludes every member's source
    /// (extractable `Another`/not-self, §2 rule 2).
    pub(crate) event_object_excludes_sources: bool,
    /// The group's trigger event carries an event object (ZoneChanged / a source
    /// event). FALSE for `Phase` triggers ⇒ a `TriggeringSource` / parentless
    /// `ParentTarget` write resolves to None and is a no-op (targeting.rs:951
    /// `_ => None`; bounce.rs empty-target no-op — §2 rule 1 parentless clause).
    /// Beyond the plan's five listed fields; required to mechanize the
    /// resolver-referent pin the context-free profile cannot express.
    pub(crate) event_object_present: bool,
    /// The live source census, for the membership census-overlap row (§2).
    pub(crate) source_census: SourceCensus,
    /// PR-6.75 (CR 603.3b + CR 110.2 + CR 108.3 + CR 805.7): the controller
    /// structure of this ordering group. `UniformAligned` reproduces the former
    /// `same_controller == true` exactly (span/fused gate consults it verbatim);
    /// `Uniform` additionally unlocks only the batch-T1 identical-function fast
    /// path (`!= Mixed`); `Mixed` consults no refinement ⇒ byte-identical to the
    /// pre-uniformity engine (structural zero-delta).
    pub(crate) controller_uniformity: ControllerUniformity,
}

// ---------------------------------------------------------------------------
// feeds matrix (§2).
// ---------------------------------------------------------------------------

/// PR-6.75 gate-scoped span bundle for `feeds` — the effective (already
/// effective-Any-promoted) relative-player spans of the player-resource and
/// membership rows plus the `gate` (`UniformAligned`). When `gate` is false the
/// span conjuncts collapse to `true`, so the feed matrix is byte-identical.
#[derive(Clone, Copy)]
struct SpanGate {
    gate: bool,
    read_player: PlayerSpan,
    write_player: PlayerSpan,
    read_mctrl: PlayerSpan,
    write_mctrl: PlayerSpan,
}

impl SpanGate {
    /// The ungated bundle (src-row call, §4.5): no player-kind reads route to
    /// `reads_src`, so the player/membership rows never fire here — `gate = false`
    /// keeps the conjuncts inert.
    fn ungated() -> SpanGate {
        SpanGate {
            gate: false,
            read_player: PlayerSpan::None,
            write_player: PlayerSpan::None,
            read_mctrl: PlayerSpan::None,
            write_mctrl: PlayerSpan::None,
        }
    }
}

/// CR 603.3b feed matrix: same-kind rows + the cross rows (`ObjectCounters →
/// ObjectPt`, `PlayerLife → JournalLife`, `HandLibrary → JournalCards`), with the
/// `SetMembership` same-kind row refined by census overlap.
/// `SetMembership → aggregate {ObjectPt,ObjectCounters}` needs no explicit row —
/// aggregate reads already carry a `SetMembership` tag. `Other` conflicts with
/// everything. `PlayerLife → SetMembership` is deliberately ABSENT (the CR 800.4a
/// player-loss cascade is the documented source-actor residual, §1.2).
fn feeds(
    reads: KindSet,
    writes: KindSet,
    read_census: &Census,
    write_census: &Census,
    read_zones: &ZoneSpan,
    write_zones: &ZoneSpan,
    spans: SpanGate,
) -> bool {
    // PR-6.75 (CR 102.2/109.5): under `UniformAligned`, a player-keyed read and
    // write of the SAME kind conflict only when their relative-player spans can
    // name a common player. `!gate` ⇒ byte-identical (conjunct is `true`).
    let player_ok = !spans.gate || player_span_overlap(spans.read_player, spans.write_player);
    // PR-6.75 (CR 110.2): same, for the membership same-kind row's controller key.
    let mctrl_ok = !spans.gate || player_span_overlap(spans.read_mctrl, spans.write_mctrl);
    if (writes.other && reads.any()) || (reads.other && writes.any()) {
        return true;
    }
    if (reads.object_pt && writes.object_pt)
        || (reads.object_counters && writes.object_counters)
        || (reads.journal_life && writes.journal_life)
        || (reads.journal_cards && writes.journal_cards)
        || (reads.journal_cast && writes.journal_cast)
        || (reads.stack_shape && writes.stack_shape)
        || (reads.tap_state && writes.tap_state)
        // CR 500 / CR 506.4c / CR 614.10: sequencing writes only conflict with a
        // sequencing READ. No profiled read produces `turn_structure` today, so
        // this row is dormant — but a future sequencing read fails closed here.
        || (reads.turn_structure && writes.turn_structure)
    {
        return true;
    }
    // CR 119 / CR 401/402: player-resource same-kind rows, gate-refined by the
    // relative-player span (Rekindled/Brink — opp-hand read × your-hand write).
    if (reads.player_life && writes.player_life && player_ok)
        || (reads.hand_library && writes.hand_library && player_ok)
    {
        return true;
    }
    // CR 122.1 + CR 613.4: counters change P/T.
    if reads.object_pt && writes.object_counters {
        return true;
    }
    // CR 119.3: a life write feeds a life-change journal read.
    if reads.journal_life && writes.player_life {
        return true;
    }
    // CR 120.6: a hand/library write feeds a draw/discard journal read.
    if reads.journal_cards && writes.hand_library {
        return true;
    }
    // SetMembership same-kind, census- AND zone-refined (§2; CR 205 type tags +
    // CR 400.1 zones), and PR-6.75 controller-refined (`mctrl_ok`, CR 110.2). A
    // battlefield token creation cannot feed a graveyard read even though both
    // name "creature" — their zones are disjoint (Tombstone); and an opponents'-
    // board count cannot feed a your-cards battlefield entry (Defense of the Heart).
    if reads.set_membership
        && writes.set_membership
        && census_overlap(read_census, write_census)
        && zone_overlap(read_zones, write_zones)
        && mctrl_ok
    {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// The conflict predicate (§1.2 pseudocode, implemented exactly).
// ---------------------------------------------------------------------------

/// CR 603.3b: do two normalized-identical siblings' resolution functions FAIL to
/// commute under the given group structure? Sound modulo the source-actor
/// residual (module doc). Fail-closed.
pub(crate) fn profiles_conflict(p: &RwProfile, s: &GroupStructure) -> bool {
    // Mana×unless-pay guard (D4-R1, CR 603.5): pool write + unless-pay/pay-cost
    // breaks the identical-choice-set symmetry. 0 printed co-occurrences.
    if p.writes_pool && p.has_pay_or_unless {
        return true;
    }
    // T1 identical-function fast paths (§1.2), sound modulo the source-actor
    // residual. The `source_independent` disjunct additionally requires
    // `!reads_member_bound`: a same-event group of DISTINCT sources whose identical
    // resolution reads per-source bound storage (CR 603.10a look-back — TrackedSet /
    // ExiledBySource / ChosenCard, `member_bound_target_filter`) is NOT one shared
    // f(state) — member A reads A's storage, member B reads B's — so the identical-
    // function commutation proof breaks and the group must prompt (discriminator
    // below). It ALSO requires `!reads_and_writes_event_object`: a same-event group
    // can be `source_independent` yet WRITE the shared triggering object and READ its
    // live characteristic (`writes_event_object` ∉ `writes_self`, `reads_event_live` ∉
    // `reads_src`) — an order-observable feed `feeds()` cannot see (CR 608.2h; the
    // event-object discriminator below). The event-object conjunct is gated on
    // `s.event_object_present` — mirroring `effective_external` (below), which drops
    // `writes_event_object` when the trigger carries no event object (the write
    // no-ops, targeting.rs:951): with no live event object there is no feed, so such
    // a group stays on the fast path (auto). `all_same_source` stays exempt: one
    // shared source ⇒ one shared storage AND one shared event object ⇒ f_A = f_B
    // literally (deterministic accumulation), order-independent.
    if s.same_event
        && (s.all_same_source
            || (p.source_independent()
                && !p.reads_member_bound
                && !(s.event_object_present && p.reads_and_writes_event_object())))
    {
        return false;
    }
    // CR 603.3b + CR 603.10a: the same-event member-bound discriminator (the batch
    // path's `!reads_member_bound` conjunct, mirrored for same-event depth). A
    // same-event group that reaches here with per-source bound storage has distinct
    // sources (`all_same_source` was auto-ordered above) and cannot prove
    // commutation — prompt fail-closed. Guarded by `s.same_event`; the batch path
    // handles member-bound via its own conjunct plus the freeze/feed rows, so batch
    // ordering decisions are byte-inert.
    if s.same_event && p.reads_member_bound {
        return true;
    }
    // CR 603.3b + CR 608.2h: the same-event event-object read/write feed
    // discriminator. `reads_event_live` (EventSource/EventTarget characteristic
    // reads, CR 608.2h current-information) records NO KindSet, so `feeds()` cannot
    // see a `writes_event_object` mutation feeding an event-object LIVE read. Same-
    // event members share ONE live event object ⇒ a member's event-object write is
    // observed by a sibling's live read ⇒ order-observable. DERIVED (not copied) from
    // the batch-T1 event-object conjunct (`!reads_event_live && !writes_event_object
    // .any()`): that guard is a disjunction-of-negations because refusing the batch
    // fast path only DEFERS to the feed rows, whereas this returns a PROMPT, so it
    // fires only on a real feed = BOTH endpoints (a conjunction, `reads_and_writes_
    // event_object`). Gated on `s.event_object_present` (mirrors `effective_external`
    // below): no live event object ⇒ the `writes_event_object` write no-ops
    // (targeting.rs:951) ⇒ no feed ⇒ auto (a genuine feed always has a live event
    // object, so this never drops one). `all_same_source` stays auto-ordered (returned
    // false at the fast path above — one shared source ⇒ identical f over one shared
    // event object, deterministic accumulation, order-immaterial). Guarded by
    // `s.same_event`; the batch path is byte-inert (its event-object reads are frozen
    // LKI, CR 603.10a, handled by the freeze-invalidation row below — the
    // batch/same-event asymmetry).
    if s.same_event && s.event_object_present && p.reads_and_writes_event_object() {
        return true;
    }
    if s.all_same_source && !p.reads_event_live {
        return false;
    }
    // CR 603.3b batch T1 (PR-6.75 c5): in a controller-uniform co-departure group of
    // normalized-identical members, a resolution that consults neither its source
    // binding (`source_independent`: CR 603.10a), nor per-member bound storage
    // (`reads_member_bound`), nor its firing event (`reads_event_live` / event-object
    // writes) is ONE function f(state, c0) shared by every member — any permutation
    // is f∘…∘f, so CR 603.3b ordering is unobservable. Sound modulo the source-actor
    // residual (module doc), inherited unchanged from the pre-C1 batch short-circuit.
    // Never bypasses the freeze-invalidation row below: that row requires a
    // frozen/src/event-live read, all excluded here. `Mixed` (CR 805.7 team pool)
    // fails closed — the divergent-controller cause-source channel is order-observable
    // (Dodecapod / EventSourceControlledBy).
    if !s.same_event
        && !matches!(s.controller_uniformity, ControllerUniformity::Mixed)
        && p.source_independent()
        && !p.reads_member_bound
        && !p.reads_event_live
        && !p.writes_event_object.any()
    {
        return false;
    }

    let freeze_valid = !p.writes_reentry_hazard;
    let frozen_src = s.all_sources_self_departed && freeze_valid;
    let live_src_reads = if frozen_src {
        KindSet::EMPTY
    } else {
        p.reads_src
    };

    // Event-object writes resolve to None (no-op) when the trigger carries no
    // event object (§2 rule 1 parentless clause; targeting.rs:951).
    let effective_external = if s.event_object_present {
        p.writes_external
    } else {
        p.writes_external.minus(p.writes_event_object)
    };

    // Freeze-invalidation row (B1) — a DIRECT conjunct on the batch path.
    if !s.same_event
        && !freeze_valid
        && (p.reads_frozen.any() || p.reads_src.any() || p.reads_event_live)
    {
        return true;
    }

    // SRC-read sibling writes: external existing objects, plus self only when
    // all members share one source; event-object writes excluded under
    // object-disjointness; created/LastCreated writes always excluded.
    let mut src_writes = effective_external.minus(p.writes_created);
    if s.same_event && s.event_object_excludes_sources && s.event_object_present {
        src_writes = src_writes.minus(p.writes_event_object);
    }
    // CR 122.1 object-scope disjointness (§2 Earthbender): an EXTERNAL counter
    // write feeds a SOURCE-scoped counter read only if the write filter can match
    // the source (census overlap; fail-closed — an unrefined counter write is
    // `Any`, and `None` here means no external counter write). A quest read on an
    // enchantment source × a +1/+1 write on creatures is object-disjoint ⇒ drop
    // `ObjectCounters` from the source-read feed. The same-source SELF counter
    // write is added AFTER this gate (a self write on the shared source DOES feed).
    if p.reads_src.object_counters && src_writes.object_counters {
        let ext_counter_census = if matches!(p.writes_external_counter_census, Census::None) {
            Census::Any // membership present but census unrecorded ⇒ fail-closed
        } else {
            p.writes_external_counter_census.clone()
        };
        if !census_overlap(&s.source_census.as_census(), &ext_counter_census) {
            src_writes = src_writes.minus(KindSet::one(StateKind::ObjectCounters));
        }
    }
    let src_self_membership = s.all_same_source && p.writes_membership_self;
    let src_write_census = membership_census_of(
        src_writes,
        &p.writes_membership_external_census,
        src_self_membership,
        s,
    );
    let src_write_zones = membership_zones_of(
        src_writes,
        &p.writes_membership_external_zones,
        src_self_membership,
        &p.writes_membership_self_zones,
    );
    if s.all_same_source {
        src_writes = src_writes.union(p.writes_self);
    }
    if feeds(
        live_src_reads,
        src_writes,
        &p.reads_membership_census,
        &src_write_census,
        &p.reads_membership_zones,
        &src_write_zones,
        // §4.5: no player-kind read routes to `reads_src`, so the gated rows never
        // fire here — pass the inert ungated bundle (fail-closed, documented).
        SpanGate::ungated(),
    ) {
        return true;
    }

    // BOARD/PLAYER-read sibling writes: everything (incl. self).
    let board_writes = effective_external.union(p.writes_self);
    let board_write_census = membership_census_of(
        board_writes,
        &p.writes_membership_external_census,
        p.writes_membership_self,
        s,
    );
    let board_write_zones = membership_zones_of(
        board_writes,
        &p.writes_membership_external_zones,
        p.writes_membership_self,
        &p.writes_membership_self_zones,
    );
    // §4.4 fused RMW: the "discards their hand" count read is a symmetric
    // per-player fixed-point input among identical members ⇒ dropped under the
    // gate; unioned back into `reads_player` when ungated (byte-identical to today,
    // where the count read was recorded in `reads_player`).
    let effective_reads_player = if matches!(
        s.controller_uniformity,
        ControllerUniformity::UniformAligned
    ) {
        p.reads_player
    } else {
        p.reads_player.union(p.reads_player_fused)
    };
    let board_reads = p.reads_board.union(effective_reads_player);
    // PR-6.75 effective spans (effective-Any promotes an unrefined-but-present
    // kind to `Any`). The external membership-ctrl is promoted only when an
    // EXTERNAL membership write is present (`effective_external.set_membership`);
    // a self-only membership write contributes `You` via `membership_ctrl_of`.
    let board_player_read_present =
        effective_reads_player.player_life || effective_reads_player.hand_library;
    let board_player_write_present = board_writes.player_life || board_writes.hand_library;
    let eff_external_ctrl = effective_span(
        p.writes_membership_external_ctrl,
        effective_external.set_membership,
    );
    let board_spans = SpanGate {
        gate: matches!(
            s.controller_uniformity,
            ControllerUniformity::UniformAligned
        ),
        read_player: effective_span(p.reads_player_span, board_player_read_present),
        write_player: effective_span(p.writes_player_span, board_player_write_present),
        read_mctrl: effective_span(p.reads_membership_ctrl, board_reads.set_membership),
        write_mctrl: membership_ctrl_of(board_writes, eff_external_ctrl, p.writes_membership_self),
    };
    if feeds(
        board_reads,
        board_writes,
        &p.reads_membership_census,
        &board_write_census,
        &p.reads_membership_zones,
        &board_write_zones,
        board_spans,
    ) {
        return true;
    }

    false
}

/// The membership-write census applying to an effective write set: external
/// census if any external membership write is present, unioned with the source
/// census if a self membership write is in scope.
fn membership_census_of(
    write_kinds: KindSet,
    external_census: &Census,
    self_in_scope: bool,
    s: &GroupStructure,
) -> Census {
    let mut c = Census::None;
    if write_kinds.set_membership {
        c.merge(external_census.clone());
    }
    if self_in_scope {
        c.merge(s.source_census.as_census());
    }
    c
}

/// CR 400.1: the membership-write ZONE span for an effective write set — the
/// external/creation zones if any external membership write is present, plus
/// `Any` (fail-closed) when a self membership move is in scope (its endpoints are
/// untracked). Mirrors `membership_census_of`.
fn membership_zones_of(
    write_kinds: KindSet,
    external_zones: &ZoneSpan,
    self_in_scope: bool,
    self_zones: &ZoneSpan,
) -> ZoneSpan {
    let mut z = ZoneSpan::None;
    if write_kinds.set_membership {
        z.merge(external_zones.clone());
    }
    if self_in_scope {
        // CR 400.1: the recorded self-write endpoints (fail-closed `Any` when a
        // self write left them unrecorded) — no longer a blanket `Any`, so a
        // battlefield self-copy is zone-disjoint from a graveyard read.
        z.merge(self_zones.clone());
    }
    z
}

// ---------------------------------------------------------------------------
// Public entry points.
// ---------------------------------------------------------------------------

/// Profile a resolved ability's reads/writes (CR 603.3b). A top-level
/// `ParentTarget` is parentless (§2 rule 1) ⇒ chain-root context starts empty.
pub(crate) fn ability_rw_profile(a: &ResolvedAbility) -> RwProfile {
    let mut p = RwProfile::empty();
    walk_ability(a, None, None, PlayerSpan::None, &mut p);
    // CR 603.10a + CR 603.3b: `legacy_batch_prompt` is computed AUTHORITATIVELY by
    // the decoupled `contains_legacy_event_ref` visitor, not by whichever leaf
    // sites the read/write walk happened to descend into. This overwrites (never
    // ORs) the walk's intermediate value: the visitor is a superset of the walk's
    // legacy leaf-hooking and additionally covers the effect target/count
    // positions the walk drops (the D5 fail-open holes).
    p.legacy_batch_prompt = contains_legacy_event_ref(a);
    p
}

/// Profile a bare trigger-level `condition` (CR 603.4 intervening-if — re-checked
/// at resolution, so its reads are order-relevant).
pub(crate) fn trigger_condition_rw_profile(c: &TriggerCondition) -> RwProfile {
    let mut p = rw_trigger_condition(c);
    // CR 603.10a: authoritative D5 flag for the trigger-level condition (merged
    // into the batch profile by `group_is_order_independent`).
    p.legacy_batch_prompt = legacy_trigger_condition(c);
    p
}

// ---------------------------------------------------------------------------
// Chain-root scope for anaphoric `ParentTarget` writes (§2 rule 1).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WriteScope {
    /// The member's own source (SelfRef / SourceOrPaired).
    SelfSource,
    /// An existing external object / player / board.
    External,
    /// The event object (`TriggeringSource`-class + parentless `ParentTarget`).
    EventObject,
    /// A fresh-id creation / LastCreated object.
    Created,
}

/// CR 603.4 + CR 608.2c: the write scope of an effect target. `ParentTarget`
/// resolves to the CHAIN ROOT (`nearest object-referent ancestor`,
/// filter.rs:3063-3085); a PARENTLESS `ParentTarget` resolves to the EVENT object
/// on a ZoneChanged trigger (targeting.rs:946-950) — represented as `EventObject`,
/// which `profiles_conflict` drops when the trigger carries no event object.
/// Exhaustive & wildcard-free: a future `TargetFilter` variant must be classified.
fn scope_of(target: &TargetFilter, chain_root: Option<WriteScope>) -> WriteScope {
    match target {
        // CR 608.2c: `OriginalSource` denotes the ability's own (pre-rebind) source
        // object — a write to it lands on the source, exactly like `SelfRef`.
        TargetFilter::SelfRef | TargetFilter::SourceOrPaired | TargetFilter::OriginalSource => {
            WriteScope::SelfSource
        }
        TargetFilter::TriggeringSource => WriteScope::EventObject,
        TargetFilter::ParentTarget | TargetFilter::ParentTargetSlot { .. } => {
            chain_root.unwrap_or(WriteScope::EventObject)
        }
        TargetFilter::LastCreated => WriteScope::Created,
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::Typed(..)
        | TargetFilter::Not { .. }
        | TargetFilter::Or { .. }
        | TargetFilter::And { .. }
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        // CR 201.5a: a reference to the specific existing object that granted the
        // ability — a write to it lands on an external object, exactly like
        // `SpecificObject` (its concretized form, ability_utils.rs:4090).
        | TargetFilter::GrantingObject
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastRevealed
        | TargetFilter::CostPaidObject
        | TargetFilter::ChosenCard
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSourceController
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource { .. }
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => WriteScope::External,
    }
}

/// Place a non-membership object write (`ObjectPt`/`ObjectCounters`/`TapState`/
/// `Other`) by scope.
fn place_object_write(p: &mut RwProfile, kind: StateKind, sc: WriteScope) {
    match sc {
        WriteScope::SelfSource => p.writes_self.set(kind),
        WriteScope::External => p.writes_external.set(kind),
        WriteScope::EventObject => {
            p.writes_external.set(kind);
            p.writes_event_object.set(kind);
        }
        WriteScope::Created => {
            p.writes_external.set(kind);
            p.writes_created.set(kind);
        }
    }
}

/// Place a `SetMembership` (zone/control) write with its census + reentry-hazard
/// (CR 603.10a/B1) + hand/library endpoint tagging.
fn place_membership_write(
    p: &mut RwProfile,
    sc: WriteScope,
    census: Census,
    origin: Option<Zone>,
    dest: Zone,
) {
    let hazard = matches!(sc, WriteScope::External)
        && (dest == Zone::Battlefield || origin == Some(Zone::Exile))
        && origin != Some(Zone::Library);
    // CR 400.1: a MOVE touches both endpoints (origin removal + dest addition), so
    // a graveyard-count read IS fed by a graveyard→battlefield return.
    let mut move_zones = ZoneSpan::one(dest);
    if let Some(o) = origin {
        move_zones.merge(ZoneSpan::one(o));
    }
    match sc {
        WriteScope::SelfSource => {
            p.writes_self.set(StateKind::SetMembership);
            p.writes_membership_self = true;
            // CR 400.1: record the actual self-move endpoints (Chorale of the
            // Void's self-sacrifice bf→graveyard keeps overlapping a graveyard
            // "left this turn" read).
            p.writes_membership_self_zones.merge(move_zones);
        }
        WriteScope::External => {
            p.writes_external.set(StateKind::SetMembership);
            p.writes_membership_external_census.merge(census);
            p.writes_membership_external_zones.merge(move_zones);
            p.writes_reentry_hazard |= hazard;
        }
        WriteScope::EventObject => {
            p.writes_external.set(StateKind::SetMembership);
            p.writes_event_object.set(StateKind::SetMembership);
            // Event object identity is unknown at profile time ⇒ census + zone Any
            // (the precise `move_zones` is used only by the External/Created arms).
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.writes_reentry_hazard |= hazard;
        }
        WriteScope::Created => {
            p.writes_external.set(StateKind::SetMembership);
            p.writes_created.set(StateKind::SetMembership);
            p.writes_membership_external_census.merge(census);
            p.writes_membership_external_zones.merge(move_zones);
        }
    }
    if is_hand_or_library(dest) || origin.is_some_and(is_hand_or_library) {
        p.writes_external.set(StateKind::HandLibrary);
        // §4.3.4 (CR 400.3 + owner-alignment gate): a SELF hand/library move
        // (Rekindled Flame's `Bounce{SelfRef}` → hand) touches the owner-of-self's
        // hand, which equals the controller's under `UniformAligned` ⇒ You.
        // External/event/created endpoints leave the span unrefined (`None` ⇒
        // effective-Any at conflict time).
        if matches!(sc, WriteScope::SelfSource) {
            p.writes_player_span = PlayerSpan::You;
        }
    }
}

fn is_hand_or_library(z: Zone) -> bool {
    matches!(z, Zone::Hand | Zone::Library)
}

/// CR 205: extract a type-tag census from a filter used as a read/write selector.
/// `Typed` yields its core-type + subtype + token-ness tags; `Not`/broad filters
/// ⇒ `Any` (fail-closed); `And`/`Or` union their components.
fn census_of_filter(f: &TargetFilter) -> Census {
    match f {
        TargetFilter::Typed(tf) => census_of_typed(tf),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            let mut c = Census::None;
            for x in filters {
                c.merge(census_of_filter(x));
            }
            if matches!(c, Census::None) {
                Census::Any
            } else {
                c
            }
        }
        _ => Census::Any,
    }
}

/// CR 205: collect core-type + subtype tags from a bare `TypeFilter` slice into
/// `tags`. Returns `Some(Census::Any)` on the first broad / negative / disjunctive
/// constraint (unextractable ⇒ fail-closed); `None` when every entry was a
/// concrete positive tag. Shared by `census_of_typed` and the
/// `QuantityRef::ZoneCardCount { card_types }` census (§L1).
fn collect_type_tags(type_filters: &[TypeFilter], tags: &mut BTreeSet<String>) -> Option<Census> {
    for t in type_filters {
        match t {
            TypeFilter::Creature => tags.insert("creature".into()),
            TypeFilter::Land => tags.insert("land".into()),
            TypeFilter::Artifact => tags.insert("artifact".into()),
            TypeFilter::Enchantment => tags.insert("enchantment".into()),
            TypeFilter::Instant => tags.insert("instant".into()),
            TypeFilter::Sorcery => tags.insert("sorcery".into()),
            TypeFilter::Planeswalker => tags.insert("planeswalker".into()),
            TypeFilter::Battle => tags.insert("battle".into()),
            TypeFilter::Kindred => tags.insert("kindred".into()),
            TypeFilter::Subtype(s) => tags.insert(s.to_lowercase()),
            // Broad / non-positive / disjunctive type constraints ⇒ unextractable.
            TypeFilter::Permanent | TypeFilter::Card | TypeFilter::Any => return Some(Census::Any),
            TypeFilter::Non(_) | TypeFilter::AnyOf(_) => return Some(Census::Any),
        };
    }
    None
}

fn census_of_typed(tf: &TypedFilter) -> Census {
    let mut tags: BTreeSet<String> = BTreeSet::new();
    if let Some(c) = collect_type_tags(&tf.type_filters, &mut tags) {
        return c;
    }
    for prop in &tf.properties {
        match prop {
            FilterProp::Token => {
                tags.insert("token".into());
            }
            FilterProp::NonToken => {
                tags.insert("nontoken".into());
            }
            FilterProp::RepresentedByCard => {
                tags.insert("represented-card".into());
            }
            _ => {}
        }
    }
    if tags.is_empty() {
        Census::Any
    } else {
        Census::Tags(tags)
    }
}

/// CR 604.3 + CR 205: census of a `QuantityRef::ZoneCardCount { card_types }`
/// list — the same tag extraction as `census_of_typed` but over a bare
/// `TypeFilter` slice with no properties. Empty `card_types` (all cards) ⇒ `Any`.
fn census_of_zone_card_types(card_types: &[TypeFilter]) -> Census {
    let mut tags: BTreeSet<String> = BTreeSet::new();
    if let Some(c) = collect_type_tags(card_types, &mut tags) {
        return c;
    }
    if tags.is_empty() {
        Census::Any
    } else {
        Census::Tags(tags)
    }
}

/// CR 400.1: the concrete battlefield-external zone a `ZoneRef` names, for the
/// `QuantityRef::ZoneCardCount { zone }` membership-read zone (§L1).
fn zone_of_zone_ref(z: &ZoneRef) -> Zone {
    match z {
        ZoneRef::Graveyard => Zone::Graveyard,
        ZoneRef::Exile => Zone::Exile,
        ZoneRef::Library => Zone::Library,
        ZoneRef::Hand => Zone::Hand,
    }
}

/// A read filter that provably counts only the member's own source (a `SelfRef`
/// conjunct) — routes to per-member-private `reads_src` (§2 read-carrier closure).
fn filter_is_self_scoped(f: &TargetFilter) -> bool {
    match f {
        TargetFilter::SelfRef => true,
        TargetFilter::And { filters } => filters.iter().any(filter_is_self_scoped),
        _ => false,
    }
}

/// CR 111.1: the filter's `valid_card` provably excludes every group member's
/// source (an `Another` component) — the object-disjointness signal (§2 rule 2).
/// Consumed by the parity sweep's STATIC event-object-disjointness model
/// (`triggers_ordering_parity_tests`). The production chokepoint
/// (`group_is_order_independent`) instead uses the DYNAMIC id-disjointness check
/// (the event object's id vs the members' `source_id`s) because `valid_card` is
/// not carried on `PendingTrigger`; the dynamic check is a sound, at-least-as-
/// precise witness of the same relation. `#[allow(dead_code)]` covers the
/// non-test lib build where only the sweep (a `#[cfg(test)]` consumer) calls it.
#[allow(dead_code)]
pub(crate) fn filter_excludes_source(f: &TargetFilter) -> bool {
    match f {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::Another)),
        TargetFilter::And { filters } => filters.iter().any(filter_excludes_source),
        // §L6 (CR 303.4d + CR 301.5c): the object an Aura/Equipment is ATTACHED to
        // is a DIFFERENT object than the source itself — an Aura can't enchant
        // itself (CR 303.4d) and an Equipment can't equip itself (CR 301.5c). So an
        // `AttachedTo` `valid_card` (the enchanted/equipped creature — e.g. the
        // Ordeals' "whenever enchanted creature attacks") provably excludes every
        // group member's source, dropping the event-object counter write from the
        // source-scoped counter-read feed.
        TargetFilter::AttachedTo => true,
        _ => false,
    }
}

/// CR 205: does a printed source's type census overlap a filter's type census
/// (fail-closed — either side unextractable ⇒ `Any` ⇒ overlap)? Used by the
/// parity sweep's condition-based reachability guard (§1.3.1-F): a same-event
/// 2-copy group is unreachable when the source itself matches an
/// `Another`-self-exclusion count the intervening-if requires to be zero
/// (Thopter Assembly). `#[allow(dead_code)]` covers the non-test lib build where
/// only the sweep (a `#[cfg(test)]` consumer) calls it.
#[allow(dead_code)]
pub(crate) fn source_census_overlaps_filter(s: &SourceCensus, f: &TargetFilter) -> bool {
    census_overlap(&s.as_census(), &census_of_filter(f))
}

/// D5 (CR 603.10a): the 9 `TargetFilter` carriers of the 12 retained-prompt
/// event-context refs (the other 3 — `EventContextAmount`,
/// `EventContextSourceCostX`, `ManaSpentToCast` — are `QuantityRef`s, handled by
/// the read path). The frozen serde oracle
/// (`value_contains_trigger_event_context_ref`) matched these tags ANYWHERE in
/// the serialized ability — read OR write position — so a tag as an effect WRITE
/// TARGET must also set `legacy_batch_prompt` for the batch branch to retain its
/// prompt (D3 zero-widening). Each of the 9 is a unit variant serializing to a
/// bare string, exactly what the oracle matched; the struct-variant
/// `ParentTargetSlot` is deliberately EXCLUDED (it serializes as an object key,
/// which the oracle's value-walk never matches). Composite filters are descended
/// so a nested tag is still caught (position-agnostic like the oracle).
fn target_is_legacy_ref(f: &TargetFilter) -> bool {
    match f {
        TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::StackSpell
        | TargetFilter::CostPaidObject => true,
        TargetFilter::Not { filter } => target_is_legacy_ref(filter),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(target_is_legacy_ref)
        }
        _ => false,
    }
}

/// Set `legacy_batch_prompt` when an effect write target carries a D5 event-
/// context ref (§D5, CR 603.10a) — mirrors the read-carrier path
/// (`rw_target_filter` → `legacy_ref`) for write position. Position-agnostic:
/// applied at every write-target-bearing arm so the tag anywhere ⇒ prompt.
fn flag_legacy_write_target(p: &mut RwProfile, target: &TargetFilter) {
    if target_is_legacy_ref(target) {
        p.legacy_batch_prompt = true;
    }
}

/// Set `reads_member_bound` when an effect WRITE target names a per-member-bound
/// referent (CR 603.10a, PR-6.75 c5) — the write path's counterpart to
/// `member_bound_target_filter` read-position wiring. A `ChangeZone{TrackedSet}`
/// return (Day of the Dragons) writes a per-source set ⇒ members diverge.
fn flag_member_bound_write_target(p: &mut RwProfile, target: &TargetFilter) {
    if member_bound_target_filter(target) {
        p.reads_member_bound = true;
    }
}

// ---------------------------------------------------------------------------
// D5 decoupled legacy event-context visitor (CR 603.10a + CR 603.3b).
//
// `legacy_batch_prompt` must be true whenever the resolved ability references
// ANY of the 12 frozen event-context tags in ANY position — the batch/departure
// ordering path keeps prompting them for strict D3 parity with the deleted serde
// allowlist (CR 603.10a look-back reads). The read/write PROFILE walk sets the
// flag only at the leaf detectors it happens to DESCEND into; an arm that ignores
// its target/count field (`Discard { target: _ }`, `PutAtLibraryPosition`,
// `Sacrifice`'s `EventContextAmount` count, …) silently dropped the tag and the
// batch group auto-ordered where the shipped engine prompted (a fail-open D5/D3
// widening).
//
// This visitor is the SINGLE AUTHORITY for `legacy_batch_prompt` (wired into
// `ability_rw_profile` / `trigger_condition_rw_profile`), DECOUPLED from the
// profile walk's descent decisions: it performs its own exhaustive, wildcard-free
// descent of every position where one of the 5 tag-bearing typed enums appears,
// so the flag can never again depend on whether the profiling walk descended. A
// future variant of any visited enum fails to COMPILE until classified (CR
// 603.3b ordering correctness). It intentionally reproduces the profile walk's
// legacy coverage for non-effect positions (already proven equal to the frozen
// serde oracle by the §5.2 parity sweep) and ADDS the effect target/count
// positions the walk drops.
//
// The 12 frozen tags and their carrier enums:
//   * TargetFilter: TriggeringSpellController, TriggeringSpellOwner,
//     TriggeringPlayer, TriggeringSource, ParentTarget, ParentTargetController,
//     ParentTargetOwner, StackSpell, CostPaidObject.
//   * QuantityRef: EventContextAmount, EventContextSourceCostX, ManaSpentToCast.
//   * ObjectScope: CostPaidObject.  * PlayerFilter: TriggeringPlayer.
//   * ControllerRef: TriggeringPlayer, ParentTargetController, ParentTargetOwner.
// `TargetFilter::ParentTargetSlot` is EXCLUDED — it is NOT one of the 12 (it
// serializes as an object key the frozen serde oracle's value-walk never matched).
// ---------------------------------------------------------------------------

/// D5 (CR 603.10a): does a resolved ability reference any of the 12 frozen
/// event-context tags anywhere in its typed AST? The single authority for
/// `legacy_batch_prompt` (CR 603.3b batch-ordering parity).
fn contains_legacy_event_ref(a: &ResolvedAbility) -> bool {
    legacy_effect(&a.effect)
        || a.sub_ability
            .as_deref()
            .is_some_and(contains_legacy_event_ref)
        || a.else_ability
            .as_deref()
            .is_some_and(contains_legacy_event_ref)
        || a.condition.as_ref().is_some_and(legacy_ability_condition)
        || a.duration.as_ref().is_some_and(legacy_duration)
        || a.player_scope.as_ref().is_some_and(legacy_player_filter)
        || a.starting_with.as_ref().is_some_and(legacy_controller_ref)
        || a.repeat_for.as_ref().is_some_and(legacy_quantity_expr)
        || a.multi_target.as_ref().is_some_and(legacy_multi_target)
        || a.target_constraints.iter().any(legacy_target_constraint)
        || a.target_chooser.as_ref().is_some_and(legacy_target_filter)
        || a.repeat_until
            .as_ref()
            .is_some_and(legacy_repeat_continuation)
        || a.modal.as_ref().is_some_and(legacy_modal_choice)
        || a.mode_abilities.iter().any(legacy_definition)
}

/// D5: the same descent over a nested `AbilityDefinition` body (deferred
/// triggers, choice branches, coin/die sub-effects, modal modes).
fn legacy_definition(a: &AbilityDefinition) -> bool {
    legacy_effect(&a.effect)
        || a.sub_ability.as_deref().is_some_and(legacy_definition)
        || a.else_ability.as_deref().is_some_and(legacy_definition)
        || a.condition.as_ref().is_some_and(legacy_ability_condition)
        || a.duration.as_ref().is_some_and(legacy_duration)
        || a.player_scope.as_ref().is_some_and(legacy_player_filter)
        || a.starting_with.as_ref().is_some_and(legacy_controller_ref)
        || a.repeat_for.as_ref().is_some_and(legacy_quantity_expr)
        || a.multi_target.as_ref().is_some_and(legacy_multi_target)
        || a.target_constraints.iter().any(legacy_target_constraint)
        || a.target_chooser.as_ref().is_some_and(legacy_target_filter)
        || a.repeat_until
            .as_ref()
            .is_some_and(legacy_repeat_continuation)
        || a.modal.as_ref().is_some_and(legacy_modal_choice)
        || a.mode_abilities.iter().any(legacy_definition)
}

fn legacy_multi_target(m: &MultiTargetSpec) -> bool {
    legacy_quantity_expr(&m.min) || m.max.as_ref().is_some_and(legacy_quantity_expr)
}

fn legacy_target_constraint(c: &TargetSelectionConstraint) -> bool {
    match c {
        TargetSelectionConstraint::TotalManaValue { value, .. } => legacy_quantity_expr(value),
        TargetSelectionConstraint::DifferentTargetPlayers
        | TargetSelectionConstraint::DifferentObjectControllers
        | TargetSelectionConstraint::SameZoneOwner { .. } => false,
    }
}

fn legacy_repeat_continuation(r: &RepeatContinuation) -> bool {
    match r {
        RepeatContinuation::WhileCondition { condition, .. } => legacy_ability_condition(condition),
        RepeatContinuation::ControllerChoice | RepeatContinuation::UntilStopConditions { .. } => {
            false
        }
    }
}

fn legacy_modal_choice(m: &ModalChoice) -> bool {
    legacy_player_filter(&m.chooser)
        || m.dynamic_max_choices
            .as_ref()
            .is_some_and(legacy_quantity_expr)
}

/// D5 trigger-level condition entry (CR 603.4 intervening-if — re-checked at
/// resolution, so a legacy ref there also retains the batch prompt).
fn legacy_trigger_condition(x: &TriggerCondition) -> bool {
    match x {
        TriggerCondition::QuantityComparison { lhs, rhs, .. } => {
            legacy_quantity_expr(lhs) || legacy_quantity_expr(rhs)
        }
        TriggerCondition::DuringPlayersTurn { player } => legacy_player_filter(player),
        TriggerCondition::And { conditions } | TriggerCondition::Or { conditions } => {
            conditions.iter().any(legacy_trigger_condition)
        }
        TriggerCondition::Not { condition } => legacy_trigger_condition(condition),
        TriggerCondition::GainedLife { .. }
        | TriggerCondition::LostLife
        | TriggerCondition::LostLifeLastTurn
        | TriggerCondition::DealtDamageBySourceThisTurn
        | TriggerCondition::DealtDamageThisTurnBySource { .. }
        | TriggerCondition::LifeTotalGE { .. }
        | TriggerCondition::ControlsType { .. }
        | TriggerCondition::ControlCount { .. }
        | TriggerCondition::ControlsNone { .. }
        | TriggerCondition::DefendingPlayerControlsNone { .. }
        | TriggerCondition::HadCounters { .. }
        | TriggerCondition::HasCounters { .. }
        | TriggerCondition::CounterAddedThisTurn
        | TriggerCondition::SourceIsTapped
        | TriggerCondition::SourceMatchesFilter { .. }
        | TriggerCondition::NoSpellsCastLastTurn
        | TriggerCondition::TwoOrMoreSpellsCastLastTurn
        | TriggerCondition::CastSpellThisTurn { .. }
        | TriggerCondition::SpellCastWithVariantThisTurn { .. }
        | TriggerCondition::SourceEnteredThisTurn
        | TriggerCondition::SourceIsHarnessed
        | TriggerCondition::SourceIsAttacking
        | TriggerCondition::SourceIsTransformed
        | TriggerCondition::SourceIsFaceUp
        | TriggerCondition::SourceIsFaceDown
        | TriggerCondition::SourceInZone { .. }
        | TriggerCondition::IsRenowned { .. }
        | TriggerCondition::WasStartingPlayer { .. }
        | TriggerCondition::ZoneChangeObjectMatchesFilter { .. }
        | TriggerCondition::ZoneChangeObjectIsTapped
        | TriggerCondition::EventDamageSourceMatchesFilter { .. }
        | TriggerCondition::EventObjectMatchesFilter { .. }
        | TriggerCondition::DamagedPlayerIsEventSourceOwner
        | TriggerCondition::TriggeringSpellTargetsFilter { .. }
        // CR 601.2a + CR 603.4: the "match-event-subject" sibling of
        // `TriggeringSpellTargetsFilter` — an event predicate, not a frozen-12 tag.
        | TriggerCondition::TriggeringSpellMatchesFilter { .. }
        | TriggerCondition::ManaColorSpent { .. }
        | TriggerCondition::ManaSpentCondition { .. }
        | TriggerCondition::AttackersDeclaredCount { .. }
        | TriggerCondition::Descended
        | TriggerCondition::EchoDue
        | TriggerCondition::MinCoAttackers { .. }
        | TriggerCondition::SolveConditionMet
        | TriggerCondition::ClassLevelGE { .. }
        | TriggerCondition::AttractionVisitRoll { .. }
        | TriggerCondition::WasCast { .. }
        | TriggerCondition::WasPlayed
        | TriggerCondition::AdditionalCostPaid { .. }
        | TriggerCondition::CastVariantPaid { .. }
        | TriggerCondition::CastVariantPaidPersistent { .. }
        | TriggerCondition::ActivatedAbilityIsNonMana
        | TriggerCondition::FirstTimeObjectTappedThisTurn
        // Both first-time siblings are terminal here: neither carries a legacy
        // player-filter/quantity ref, so the D5 legacy-batch-prompt flag is
        // identical either way (the read-vs-no-read divergence is only in
        // `rw_trigger_condition` above).
        | TriggerCondition::FirstTimeObjectCountersAddedThisTurn
        | TriggerCondition::WasType { .. }
        | TriggerCondition::AttackedThisTurn
        | TriggerCondition::FirstCombatPhaseOfTurn
        | TriggerCondition::HasMaxSpeed
        | TriggerCondition::IsMonarch
        | TriggerCondition::IsInitiative
        | TriggerCondition::NoMonarch
        | TriggerCondition::HasCityBlessing
        | TriggerCondition::CompletedDungeon { .. }
        | TriggerCondition::TributeNotPaid
        | TriggerCondition::CastDuringPhase { .. }
        | TriggerCondition::CastTimingPermission { .. }
        | TriggerCondition::ControlsCommander { .. }
        | TriggerCondition::ChosenLabelIs { .. }
        | TriggerCondition::ExceptFirstDrawInDrawStep
        | TriggerCondition::PlacedByAbilitySource => false,
    }
}

fn legacy_ability_condition(x: &AbilityCondition) -> bool {
    match x {
        AbilityCondition::QuantityCheck { lhs, rhs, .. } => {
            legacy_quantity_expr(lhs) || legacy_quantity_expr(rhs)
        }
        AbilityCondition::PreviousEffectAmount { rhs, .. } => legacy_quantity_expr(rhs),
        AbilityCondition::ScopedPlayerMatches { filter } => legacy_player_filter(filter),
        AbilityCondition::ConditionInstead { inner }
        | AbilityCondition::Not { condition: inner } => legacy_ability_condition(inner),
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            conditions.iter().any(legacy_ability_condition)
        }
        AbilityCondition::ObjectsShareQuality { .. }
        | AbilityCondition::TargetMatchesFilter { .. }
        | AbilityCondition::SourceMatchesFilter { .. }
        | AbilityCondition::SourceIsTapped
        | AbilityCondition::ControllerControlsMatching { .. }
        | AbilityCondition::TriggeringSpellTargetsFilter { .. }
        | AbilityCondition::ZoneChangeObjectMatchesFilter { .. }
        | AbilityCondition::ZoneChangedThisWay { .. }
        | AbilityCondition::CostPaidObjectMatchesFilter { .. }
        | AbilityCondition::EventOutcomeWon
        | AbilityCondition::CoinFlipOutcome { .. }
        | AbilityCondition::SpellCastWithVariantThisTurn { .. }
        | AbilityCondition::NthResolutionThisTurn { .. }
        | AbilityCondition::RevealedHasCardType { .. }
        | AbilityCondition::SourceEnteredThisTurn
        | AbilityCondition::AdditionalCostPaid { .. }
        | AbilityCondition::CastVariantPaid { .. }
        | AbilityCondition::SourceAttachedToCreature
        | AbilityCondition::ControllerControlledMatchingAsCast { .. }
        | AbilityCondition::SourceLacksKeyword { .. }
        | AbilityCondition::WasStartingPlayer { .. }
        | AbilityCondition::AdditionalCostPaidInstead
        | AbilityCondition::AlternativeManaCostPaid
        | AbilityCondition::EffectOutcome { .. }
        | AbilityCondition::WhenYouDo
        | AbilityCondition::WasCast { .. }
        | AbilityCondition::CastDuringPhase { .. }
        | AbilityCondition::CurrentPhaseIs { .. }
        | AbilityCondition::CastTimingPermission { .. }
        | AbilityCondition::ManaColorSpent { .. }
        | AbilityCondition::TargetSharesNameWithOtherExiledThisWay { .. }
        | AbilityCondition::CastVariantPaidInstead { .. }
        | AbilityCondition::HasMaxSpeed
        | AbilityCondition::IsMonarch
        // CR 309.7: controller-state predicate; reads no object, writes nothing.
        | AbilityCondition::CompletedDungeon { .. }
        | AbilityCondition::IsInitiative
        | AbilityCondition::HasCityBlessing
        | AbilityCondition::IsRingBearer
        | AbilityCondition::TargetHasKeywordInstead { .. }
        | AbilityCondition::HasObjectTarget
        | AbilityCondition::IsYourTurn
        | AbilityCondition::FirstCombatPhaseOfTurn
        | AbilityCondition::FirstEndStepOfTurn
        | AbilityCondition::DayNightIsNeither
        | AbilityCondition::DayNightIs { .. } => false,
    }
}

fn legacy_static_condition(x: &StaticCondition) -> bool {
    match x {
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            legacy_quantity_expr(lhs) || legacy_quantity_expr(rhs)
        }
        StaticCondition::IsTapped { scope, .. } => legacy_object_scope(scope),
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            conditions.iter().any(legacy_static_condition)
        }
        StaticCondition::Not { condition } => legacy_static_condition(condition),
        StaticCondition::DevotionGE { .. }
        | StaticCondition::SharesColorWithMostCommonColorAmongPermanents
        | StaticCondition::IsPresent { .. }
        | StaticCondition::DefendingPlayerControls { .. }
        | StaticCondition::HasCounters { .. }
        | StaticCondition::SourceIsTapped
        | StaticCondition::OpponentPoisonAtLeast { .. }
        | StaticCondition::SpellCastWithVariantThisTurn { .. }
        | StaticCondition::SourceMatchesFilter { .. }
        | StaticCondition::TopOfLibraryMatches { .. }
        | StaticCondition::UnlessPay { .. }
        | StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsAttacking
        | StaticCondition::SourceIsBlocking
        | StaticCondition::SourceIsBlocked
        | StaticCondition::SourceEnteredThisTurn
        | StaticCondition::SourceHasDealtDamage
        | StaticCondition::SourceIsSaddled
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsEnchanted
        | StaticCondition::SourceIsMonstrous
        | StaticCondition::SourceIsHarnessed
        | StaticCondition::SourceAttachedToCreature
        | StaticCondition::SourceIsPaired
        | StaticCondition::SourceInZone { .. }
        | StaticCondition::WasStartingPlayer { .. }
        | StaticCondition::RecipientHasCounters { .. }
        | StaticCondition::RecipientMatchesFilter { .. }
        | StaticCondition::RecipientAttackingOwnerTarget { .. }
        | StaticCondition::ChosenColorIs { .. }
        | StaticCondition::ChosenLabelIs { .. }
        | StaticCondition::HasMaxSpeed
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::DayNightIs { .. }
        | StaticCondition::CastVariantPaid { .. }
        | StaticCondition::ClassLevelGE { .. }
        | StaticCondition::IsMonarch
        | StaticCondition::IsInitiative
        | StaticCondition::NoMonarch
        | StaticCondition::HasCityBlessing
        | StaticCondition::CompletedADungeon
        | StaticCondition::Unrecognized { .. }
        | StaticCondition::DuringYourTurn
        | StaticCondition::WasCast { .. }
        | StaticCondition::IsRingBearer
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::ControlsCommander { .. }
        | StaticCondition::SourceControllerEquals { .. }
        | StaticCondition::EnchantedIsFaceDown
        | StaticCondition::SourceIsFaceUp
        | StaticCondition::AdditionalCostPaid
        | StaticCondition::CastingAsVariant { .. }
        | StaticCondition::None => false,
    }
}

fn legacy_duration(x: &Duration) -> bool {
    match x {
        Duration::ForAsLongAs { condition } => legacy_static_condition(condition),
        Duration::UntilEndOfTurn
        | Duration::UntilEndOfCombat
        | Duration::UntilHostLeavesPlay
        | Duration::UntilSourceExilesAnotherCard
        | Duration::Permanent
        | Duration::UntilNextTurnOf { .. }
        | Duration::UntilEndOfNextTurnOf { .. }
        | Duration::UntilNextStepOf { .. } => false,
    }
}

fn legacy_quantity_expr(x: &QuantityExpr) -> bool {
    match x {
        QuantityExpr::Ref { qty } => legacy_quantity_ref(qty),
        QuantityExpr::Fixed { value: _ } => false,
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::UpTo { max: inner } => legacy_quantity_expr(inner),
        QuantityExpr::Power { exponent, base: _ } => legacy_quantity_expr(exponent),
        QuantityExpr::Difference { left, right } => {
            legacy_quantity_expr(left) || legacy_quantity_expr(right)
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().any(legacy_quantity_expr)
        }
    }
}

fn legacy_quantity_ref(x: &QuantityRef) -> bool {
    match x {
        // 3 of the 12 frozen tags are QuantityRefs.
        QuantityRef::EventContextAmount
        | QuantityRef::EventContextSourceCostX
        | QuantityRef::ManaSpentToCast { .. } => true,
        // Object-scope carriers: `ObjectScope::CostPaidObject` is a 12th tag.
        QuantityRef::CountersOn { scope, .. }
        | QuantityRef::Intensity { scope, .. }
        | QuantityRef::Power { scope, .. }
        | QuantityRef::Toughness { scope, .. }
        | QuantityRef::ObjectManaValue { scope, .. }
        | QuantityRef::ObjectColorCount { scope, .. }
        | QuantityRef::ObjectNameWordCount { scope, .. }
        | QuantityRef::ObjectTypelineComponentCount { scope, .. }
        | QuantityRef::ManaSymbolsInManaCost { scope, .. } => legacy_object_scope(scope),
        QuantityRef::HandSize { .. }
        | QuantityRef::LifeTotal { .. }
        | QuantityRef::LifeAboveStarting
        | QuantityRef::StartingLifeTotal
        | QuantityRef::TriggeringDiscoverValue
        | QuantityRef::GraveyardSize { .. }
        | QuantityRef::ObjectCount { .. }
        | QuantityRef::ObjectCountDistinct { .. }
        | QuantityRef::ObjectCountBySharedQuality { .. }
        | QuantityRef::ControlledByEachPlayer { .. }
        | QuantityRef::DistinctColorsAmongPermanents { .. }
        | QuantityRef::CountersOnObjects { .. }
        | QuantityRef::DistinctCounterKindsAmong { .. }
        | QuantityRef::Aggregate { .. }
        | QuantityRef::PlayerCount { .. }
        | QuantityRef::TargetObjectManaValue { .. }
        | QuantityRef::PlayerCounter { .. }
        | QuantityRef::TargetControllerCounter { .. }
        | QuantityRef::Variable { .. }
        | QuantityRef::SelfManaValue
        | QuantityRef::TargetZoneCardCount { .. }
        | QuantityRef::Devotion { .. }
        | QuantityRef::DistinctCardTypes { .. }
        | QuantityRef::DistinctSubtypes { .. }
        | QuantityRef::BasicLandTypeCount { .. }
        | QuantityRef::PartySize { .. }
        | QuantityRef::CardsExiledBySource
        | QuantityRef::ExiledCardPower { .. }
        | QuantityRef::TrackedSetSize
        | QuantityRef::FilteredTrackedSetSize { .. }
        | QuantityRef::TrackedSetAggregate { .. }
        | QuantityRef::ExiledFromHandThisResolution
        | QuantityRef::PreviousEffectAmount { .. }
        | QuantityRef::TurnsTaken
        | QuantityRef::CrimesCommittedThisTurn
        | QuantityRef::ChosenNumber
        | QuantityRef::AttackedThisTurn { .. }
        | QuantityRef::DescendedThisTurn
        // CR 701.65b/701.66b/701.67c: controller-scoped per-turn bend accumulator
        // (Avatar Aang), same class as CrimesCommittedThisTurn — not a frozen tag.
        | QuantityRef::BendTypesThisTurn
        | QuantityRef::LandsPlayedThisTurn { .. }
        | QuantityRef::DungeonsCompleted
        | QuantityRef::CostXPaid
        | QuantityRef::KickerCount
        | QuantityRef::AdditionalCostPaymentCount
        | QuantityRef::AdditionalCostPaymentCountFor { .. }
        | QuantityRef::ConvokedCreatureCount
        | QuantityRef::ColorsInCommandersColorIdentity
        | QuantityRef::CommanderCastFromCommandZoneCount
        | QuantityRef::CommanderManaValue { .. }
        | QuantityRef::Speed { .. }
        | QuantityRef::VoteCount { .. }
        | QuantityRef::ZoneCardCount { .. }
        | QuantityRef::EnteredThisTurn { .. }
        | QuantityRef::SacrificedThisTurn { .. }
        | QuantityRef::BattlefieldEntriesThisTurn { .. }
        | QuantityRef::ZoneChangeCountThisTurn { .. }
        | QuantityRef::ZoneChangeAggregateThisTurn { .. }
        | QuantityRef::TokensCreatedThisTurn { .. }
        | QuantityRef::CounterAddedThisTurn { .. }
        | QuantityRef::LifeLostThisTurn { .. }
        | QuantityRef::LifeGainedThisTurn { .. }
        | QuantityRef::DamageDealtThisTurn { .. }
        | QuantityRef::CardsDrawnThisTurn { .. }
        | QuantityRef::CardsDiscardedThisTurn { .. }
        | QuantityRef::SpellsCastThisTurn { .. }
        | QuantityRef::SpellsCastLastTurn
        | QuantityRef::SpellsCastThisGame { .. }
        | QuantityRef::LoyaltyAbilitiesActivatedThisTurn { .. }
        | QuantityRef::PlayerActionsThisTurn { .. }
        | QuantityRef::UnspentMana { .. }
        | QuantityRef::AttachmentsOnLeavingObject { .. }
        | QuantityRef::TimesCostPaidThisResolution => false,
    }
}

fn legacy_object_scope(s: &ObjectScope) -> bool {
    match s {
        ObjectScope::CostPaidObject => true,
        ObjectScope::Source
        | ObjectScope::Recipient
        | ObjectScope::Target
        | ObjectScope::Anaphoric
        | ObjectScope::Demonstrative
        | ObjectScope::AmassedArmy
        | ObjectScope::EventSource
        // Per-resolution local surfaced by THIS ability's own reveal — not one of
        // the 12 retained legacy refs (mirrors the LastRevealed precedent).
        | ObjectScope::OtherRevealedCard
        | ObjectScope::EventTarget => false,
    }
}

fn legacy_player_filter(x: &PlayerFilter) -> bool {
    match x {
        PlayerFilter::TriggeringPlayer => true,
        PlayerFilter::ControlsCount { count, .. } => legacy_quantity_expr(count),
        PlayerFilter::PlayerAttribute { attr, value, .. } => {
            legacy_quantity_ref(attr) || legacy_quantity_expr(value)
        }
        PlayerFilter::AllExcept { exclude } => legacy_player_filter(exclude),
        PlayerFilter::OpponentLostLife
        | PlayerFilter::OpponentGainedLife
        | PlayerFilter::OpponentDealtDamage { .. }
        | PlayerFilter::OpponentOtherThanTriggering
        | PlayerFilter::OpponentOfTriggeringPlayer
        | PlayerFilter::OpponentOfTriggeringPlayerNotAttacked
        | PlayerFilter::ParentObjectTargetController
        | PlayerFilter::ParentObjectTargetOwner
        | PlayerFilter::Controller
        | PlayerFilter::Opponent
        | PlayerFilter::DefendingPlayer
        | PlayerFilter::HasLostTheGame
        | PlayerFilter::OpponentAttacked { .. }
        | PlayerFilter::OpponentAttackingEnchantedPlayer
        | PlayerFilter::All
        | PlayerFilter::HighestSpeed
        | PlayerFilter::ZoneChangedThisWay
        | PlayerFilter::PerformedActionThisWay { .. }
        | PlayerFilter::OwnersOfCardsExiledBySource
        | PlayerFilter::VotedFor { .. }
        | PlayerFilter::ChosenPlayer { .. } => false,
    }
}

fn legacy_controller_ref(x: &ControllerRef) -> bool {
    match x {
        ControllerRef::ParentTargetController
        | ControllerRef::ParentTargetOwner
        | ControllerRef::TriggeringPlayer => true,
        ControllerRef::You
        | ControllerRef::Opponent
        | ControllerRef::ScopedPlayer
        | ControllerRef::TargetPlayer
        // CR 109.4 + CR 102.2/102.3: runtime-read-identical to `TargetPlayer` (first
        // `TargetRef::Player`); not a frozen event-context tag.
        | ControllerRef::TargetOpponent
        | ControllerRef::DefendingPlayer
        | ControllerRef::ChosenPlayer { .. }
        | ControllerRef::SourceChosenPlayer
        // CR 102.1: the active player is a game-defined role read live, not a
        // frozen event-context tag.
        | ControllerRef::ActivePlayer
        | ControllerRef::EnchantedPlayer => false,
    }
}

/// The 9 `TargetFilter` carriers of the 12 tags, position-agnostic: a nested tag
/// inside `Not`/`And`/`Or`/`TrackedSetFiltered` is caught (mirrors the frozen
/// serde oracle's whole-value walk). `ParentTargetSlot` is deliberately excluded.
fn legacy_target_filter(f: &TargetFilter) -> bool {
    match f {
        TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::StackSpell
        | TargetFilter::CostPaidObject => true,
        TargetFilter::Not { filter } | TargetFilter::TrackedSetFiltered { filter, .. } => {
            legacy_target_filter(filter)
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(legacy_target_filter)
        }
        // A `Typed` filter carries a `controller` (`ControllerRef`) and
        // `properties` (`FilterProp`), each of which can nest a frozen tag
        // (e.g. `creature <ParentTargetController> controls`, or a
        // `SharesQuality`/`PtComparison` prop referencing a `TriggeringSource`
        // filter / `CostPaidObject` scope). The serde oracle walked these; we
        // must too. `type_filters` (`TypeFilter`) carry no tag.
        TargetFilter::Typed(tf) => {
            tf.controller.as_ref().is_some_and(legacy_controller_ref)
                || tf.properties.iter().any(legacy_filter_prop)
        }
        TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSourceController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::ChosenDamageSource { .. }
        | TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        // CR 201.5a: the granting object is not one of the 12 frozen event-context
        // tags (mirrors `SpecificObject`, its concretized form).
        | TargetFilter::GrantingObject
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::ChosenCard
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        // CR 201.5a: `OriginalSource` is not one of the 12 frozen event-context
        // tags — it is concretized to `SpecificObject` at resolution (mirrors
        // `SpecificObject`, its concretized form).
        | TargetFilter::OriginalSource
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => false,
    }
}

/// A `FilterProp` interior can nest each of the tag-bearing enums: a
/// `TargetFilter` (`SharesQuality`/`CanEnchant`/`Targets`/…), a `QuantityExpr`
/// (`Counters`/`Cmc`/`PtComparison` values → `EventContext*` / `CostPaidObject`
/// scope), a `ControllerRef` (`Owned`/`Attacking`/…), or a nested `FilterProp`
/// (`AnyOf`/`Not`). Exhaustive & wildcard-free so a future prop must be
/// classified (a `_ =>` here would silently re-open the D5 hole class).
fn legacy_filter_prop(p: &FilterProp) -> bool {
    match p {
        FilterProp::CanEnchant { target } => legacy_target_filter(target),
        FilterProp::DifferentNameFrom { filter }
        | FilterProp::TargetsOnly { filter }
        | FilterProp::Targets { filter } => legacy_target_filter(filter),
        FilterProp::DistinctFrom { reference } => legacy_target_filter(reference),
        FilterProp::SharesQuality { reference, .. } => {
            reference.as_deref().is_some_and(legacy_target_filter)
        }
        FilterProp::Counters { count, .. } => legacy_quantity_expr(count),
        FilterProp::Cmc { value, .. } | FilterProp::PtComparison { value, .. } => {
            legacy_quantity_expr(value)
        }
        FilterProp::ProtectorMatches { controller }
        | FilterProp::Owned { controller }
        | FilterProp::MostPrevalentCreatureTypeIn {
            scope: controller, ..
        } => legacy_controller_ref(controller),
        FilterProp::Attacking { defender: c }
        | FilterProp::AttackedThisTurn { defender: c }
        | FilterProp::HasAttachment { controller: c, .. }
        | FilterProp::HasAnyAttachmentOf { controller: c, .. }
        | FilterProp::NameMatchesAnyPermanent { controller: c } => {
            c.as_ref().is_some_and(legacy_controller_ref)
        }
        FilterProp::AnyOf { props } => props.iter().any(legacy_filter_prop),
        FilterProp::Not { prop } => legacy_filter_prop(prop),
        // CR 607.2d / CR 607.2m (by analogy): player-anchor labels are live
        // per-player state, not one of the frozen-12 event-context refs.
        FilterProp::ControllerChoseLabel { .. } => false,
        // CR 608.2i: the controller look-back predicate reads live turn-history
        // (damage ledger, life-lost tallies) via its inner PlayerFilter — not one
        // of the frozen-12 event-context refs. Mirrors ControllerChoseLabel.
        FilterProp::ControllerMatches { .. } => false,
        // Resolution-chain tracked-set membership (leaf; only a `TrackedSetId`) —
        // not one of the frozen-12 event-context refs. Member-boundness is handled
        // in `member_bound_filter_prop`.
        FilterProp::InTrackedSet { .. } => false,
        FilterProp::Token
        | FilterProp::NonToken
        | FilterProp::RepresentedByCard
        | FilterProp::WasPlayed
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::CombatRelation { .. }
        | FilterProp::Unblocked
        | FilterProp::AttackingAlone
        | FilterProp::BlockingAlone
        | FilterProp::Tapped
        | FilterProp::Untapped
        | FilterProp::IsSaddled
        | FilterProp::SaddledSource
        | FilterProp::ConvokedSource
        | FilterProp::HasHasteOrControlledSinceTurnBegan
        | FilterProp::WithKeyword { .. }
        | FilterProp::HasKeywordKind { .. }
        | FilterProp::WithoutKeyword { .. }
        | FilterProp::WithoutKeywordKind { .. }
        | FilterProp::ManaValueParity { .. }
        | FilterProp::ManaCostIn { .. }
        | FilterProp::InZone { .. }
        | FilterProp::Foretold
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::Another
        | FilterProp::Unpaired
        | FilterProp::OtherThanTriggerObject
        | FilterProp::HasColor { .. }
        | FilterProp::PowerGTSource
        | FilterProp::ColorCount { .. }
        | FilterProp::ManaSymbolCount { .. }
        | FilterProp::HasSupertype { .. }
        | FilterProp::IsChosenCreatureType
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::MatchesLastChosenCardPredicate
        | FilterProp::HasSingleTarget
        | FilterProp::Modal
        | FilterProp::NotColor { .. }
        | FilterProp::NotSupertype { .. }
        | FilterProp::Suspected
        | FilterProp::Renowned
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::InAnyZone { .. }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::DealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ControlledContinuouslySinceTurnBegan
        | FilterProp::ZoneChangedThisTurn { .. }
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::FaceDown
        | FilterProp::Transformed
        | FilterProp::HasXInManaCost
        | FilterProp::HasXInActivationCost
        | FilterProp::WasKicked
        | FilterProp::HasManaAbility
        | FilterProp::HasNoAbilities
        | FilterProp::Named { .. }
        | FilterProp::SameName
        | FilterProp::SameNameAsParentTarget
        | FilterProp::IsCommander
        // CR 205.3m: a unit variant with no nested TargetFilter/QuantityExpr/
        // ControllerRef interior — nothing to descend, so no legacy referent.
        | FilterProp::SharesCreatureTypeWithCommander
        | FilterProp::Modified
        | FilterProp::Historic
        | FilterProp::NotHistoric
        | FilterProp::CouldBeTargetedByTriggeringSpell
        | FilterProp::Other { .. } => false,
    }
}

// ---------------------------------------------------------------------------
// Member-bound referent detection (CR 603.10a, PR-6.75 c5).
//
// A `TargetFilter`/`ControllerRef`/`FilterProp` is MEMBER-BOUND when the object
// or player it resolves to is bound per member instance — per-source tracked or
// chosen storage, an attachment, or an event/replacement-context referent — so
// two normalized-identical members do NOT share one resolution function `f`.
// Position-agnostic and exhaustive (wildcard-free) so a future variant must be
// classified — mirrors `legacy_target_filter`'s descent structure exactly.
//
// Each FALSE arm's soundness rests on ONE of:
//   * source carrier — `SelfRef`/`SourceOrPaired` set `writes_self`/`reads_src`
//     ⇒ `source_independent()` already false.
//   * event-object carrier — `TriggeringSource` / parentless `ParentTarget` set
//     `writes_event_object` ⇒ T1's `!writes_event_object.any()` conjunct fires.
//   * legacy-12 carrier — the 9 frozen `TargetFilter` tags set
//     `legacy_batch_prompt` (OR-ed OUTSIDE `profiles_conflict`).
//   * resolution-local — `ScopedPlayer`/`LastRevealed`/`LastCreated`: bound
//     within the single resolution, no cross-member storage.
//   * member-invariant under uniformity — `Controller`/`Player`/`AllPlayers`/
//     `DefendingPlayer`/`Named`/`Specific*`: one shared `c0` (or a constant) ⇒
//     identical for every member.
//   * `Owner`: owner-partition-commutative among identical members — owner is NOT
//     controller-invariant, but an owner-keyed partition of identical members
//     composes commutatively (each member's owner-slice is disjoint and the
//     aggregate is order-free), so it needs no member-bound refusal.
// ---------------------------------------------------------------------------

/// CR 603.10a: does `f` resolve to a per-member-bound object/player? Mirrors
/// `legacy_target_filter`'s descent (composites + `Typed.controller`/`properties`)
/// but classifies for member-boundness, not the frozen-12 tags.
fn member_bound_target_filter(f: &TargetFilter) -> bool {
    match f {
        // Per-source tracked/exiled/chosen storage, attachments, event- and
        // replacement-context referents (fail-closed: `ParentTargetSlot`/
        // `StackAbility` are parent/stack-context referents outside the legacy-12
        // and `writes_event_object` carriers).
        TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::ChosenCard
        | TargetFilter::HasChosenName
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::ChosenDamageSource { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::Neighbor { .. }
        | TargetFilter::OriginalController
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSourceController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::StackAbility { .. }
        // CR 201.5a (PR-6.75 c5, R3 axis): two normalized-identical granted bodies
        // whose granters DIFFER each read their OWN granter ⇒ per-member-divergent
        // (TrackedSet/ExiledBySource shape). REACHABILITY: grant-clone concretizes
        // `GrantingObject` → `SpecificObject{granter}` (ability_utils.rs:4090) and an
        // un-concretized survivor degrades to the source (targeting.rs:871), so a
        // bare `GrantingObject` never reaches this runtime walk — the arm is inert.
        // Classified fail-closed (maximal-conservative) on the member-bound axis: an
        // elided member-bound read is fail-OPEN (a false auto-order, CR 603.3b), so
        // an unreachable/degrade-to-source referent takes `true`, never `false`.
        | TargetFilter::GrantingObject => true,
        TargetFilter::Not { filter } => member_bound_target_filter(filter),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(member_bound_target_filter)
        }
        TargetFilter::Typed(tf) => {
            tf.controller
                .as_ref()
                .is_some_and(member_bound_controller_ref)
                || tf.properties.iter().any(member_bound_filter_prop)
        }
        // Source carriers (`writes_self`/`reads_src`), event-object carriers, the
        // legacy-12 tags (`legacy_batch_prompt`), resolution-local refs, and
        // uniformity-/owner-partition-invariant refs — all documented above.
        TargetFilter::SelfRef
        // CR 608.2c: source carrier (writes_self/reads_src) — source-invariant,
        // not per-member-bound; concretized to SpecificObject before this walk.
        | TargetFilter::OriginalSource
        | TargetFilter::SourceOrPaired
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::StackSpell
        | TargetFilter::CostPaidObject
        | TargetFilter::ScopedPlayer
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::DefendingPlayer
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => false,
    }
}

/// CR 603.10a: a `ControllerRef` is member-bound when it names a per-source chosen
/// player or an attachment-relative player. `You`/`Opponent`/`ScopedPlayer`/
/// `DefendingPlayer` are member-invariant under uniformity; `TargetPlayer` is
/// closed by the no-ordering-input target gate; the three legacy-12 refs ride
/// `legacy_batch_prompt`.
fn member_bound_controller_ref(x: &ControllerRef) -> bool {
    match x {
        ControllerRef::ChosenPlayer { .. }
        | ControllerRef::SourceChosenPlayer
        | ControllerRef::EnchantedPlayer => true,
        ControllerRef::ParentTargetController
        | ControllerRef::ParentTargetOwner
        | ControllerRef::TriggeringPlayer
        | ControllerRef::You
        | ControllerRef::Opponent
        | ControllerRef::ScopedPlayer
        | ControllerRef::TargetPlayer
        // CR 109.4: runtime-read-identical to `TargetPlayer` — closed by the
        // no-ordering-input target gate (the target player is a declared target,
        // member-invariant under uniformity, not per-source storage).
        | ControllerRef::TargetOpponent
        // CR 102.1: the active player is a game-defined role read live from
        // `state.active_player`, not per-source member-bound storage.
        | ControllerRef::ActivePlayer
        | ControllerRef::DefendingPlayer => false,
    }
}

/// CR 603.10a: a `FilterProp` interior nests a member-bound referent when its
/// inner `TargetFilter` / `ControllerRef` / `QuantityExpr` does. Mirrors
/// `legacy_filter_prop`'s exhaustive descent; quantity nesting reuses the
/// authoritative `rw_quantity_expr` member-bound bit.
fn member_bound_filter_prop(p: &FilterProp) -> bool {
    match p {
        FilterProp::CanEnchant { target } => member_bound_target_filter(target),
        FilterProp::DifferentNameFrom { filter }
        | FilterProp::TargetsOnly { filter }
        | FilterProp::Targets { filter } => member_bound_target_filter(filter),
        FilterProp::DistinctFrom { reference } => member_bound_target_filter(reference),
        FilterProp::SharesQuality { reference, .. } => {
            reference.as_deref().is_some_and(member_bound_target_filter)
        }
        FilterProp::Counters { count, .. } => rw_quantity_expr(count).reads_member_bound,
        FilterProp::Cmc { value, .. } | FilterProp::PtComparison { value, .. } => {
            rw_quantity_expr(value).reads_member_bound
        }
        FilterProp::ProtectorMatches { controller }
        | FilterProp::Owned { controller }
        | FilterProp::MostPrevalentCreatureTypeIn {
            scope: controller, ..
        } => member_bound_controller_ref(controller),
        FilterProp::Attacking { defender: c }
        | FilterProp::AttackedThisTurn { defender: c }
        | FilterProp::HasAttachment { controller: c, .. }
        | FilterProp::HasAnyAttachmentOf { controller: c, .. }
        | FilterProp::NameMatchesAnyPermanent { controller: c } => {
            c.as_ref().is_some_and(member_bound_controller_ref)
        }
        FilterProp::AnyOf { props } => props.iter().any(member_bound_filter_prop),
        FilterProp::Not { prop } => member_bound_filter_prop(prop),
        // CR 607.2d / CR 607.2m (by analogy): this reads durable per-player anchor
        // state keyed by controller, not per-source member-bound storage.
        FilterProp::ControllerChoseLabel { .. } => false,
        // CR 608.2i: reads live per-turn history keyed by the object's controller,
        // not per-source member-bound storage. Mirrors ControllerChoseLabel.
        FilterProp::ControllerMatches { .. } => false,
        // CR 603.10a (PR-6.75 c5): membership in the active resolution-chain tracked
        // set — the property form of the member-bound `TargetFilter::TrackedSet`
        // selector (chain-first via `chain_tracked_set_id`). Per-source published
        // storage ⇒ member-bound.
        FilterProp::InTrackedSet { .. } => true,
        FilterProp::Token
        | FilterProp::NonToken
        | FilterProp::RepresentedByCard
        | FilterProp::WasPlayed
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::CombatRelation { .. }
        | FilterProp::Unblocked
        | FilterProp::AttackingAlone
        | FilterProp::BlockingAlone
        | FilterProp::Tapped
        | FilterProp::Untapped
        | FilterProp::IsSaddled
        | FilterProp::SaddledSource
        | FilterProp::ConvokedSource
        | FilterProp::HasHasteOrControlledSinceTurnBegan
        | FilterProp::WithKeyword { .. }
        | FilterProp::HasKeywordKind { .. }
        | FilterProp::WithoutKeyword { .. }
        | FilterProp::WithoutKeywordKind { .. }
        | FilterProp::ManaValueParity { .. }
        | FilterProp::ManaCostIn { .. }
        | FilterProp::InZone { .. }
        | FilterProp::Foretold
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::Another
        | FilterProp::Unpaired
        | FilterProp::OtherThanTriggerObject
        | FilterProp::HasColor { .. }
        | FilterProp::PowerGTSource
        | FilterProp::ColorCount { .. }
        | FilterProp::ManaSymbolCount { .. }
        | FilterProp::HasSupertype { .. }
        | FilterProp::IsChosenCreatureType
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::MatchesLastChosenCardPredicate
        | FilterProp::HasSingleTarget
        | FilterProp::Modal
        | FilterProp::NotColor { .. }
        | FilterProp::NotSupertype { .. }
        | FilterProp::Suspected
        | FilterProp::Renowned
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::InAnyZone { .. }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::DealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ControlledContinuouslySinceTurnBegan
        | FilterProp::ZoneChangedThisTurn { .. }
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::FaceDown
        | FilterProp::Transformed
        | FilterProp::HasXInManaCost
        | FilterProp::HasXInActivationCost
        | FilterProp::WasKicked
        | FilterProp::HasManaAbility
        | FilterProp::HasNoAbilities
        | FilterProp::Named { .. }
        | FilterProp::SameName
        | FilterProp::SameNameAsParentTarget
        | FilterProp::IsCommander
        // CR 205.3m: no nested interior to carry a member-bound referent.
        | FilterProp::SharesCreatureTypeWithCommander
        | FilterProp::Modified
        | FilterProp::Historic
        | FilterProp::NotHistoric
        | FilterProp::CouldBeTargetedByTriggeringSpell
        | FilterProp::Other { .. } => false,
    }
}

/// A static ability granted by an effect (`Token`/`GenericEffect`/`CreateEmblem`)
/// can nest a frozen tag in its affected filter, its `as long as` gate, or a
/// granted modification (e.g. a `GrantTrigger` whose body references
/// `ParentTarget` — Rekindling Phoenix's token upkeep trigger).
fn legacy_static_definition(s: &StaticDefinition) -> bool {
    s.affected.as_ref().is_some_and(legacy_target_filter)
        || s.condition.as_ref().is_some_and(legacy_static_condition)
        || s.modifications.iter().any(legacy_continuous_modification)
}

/// A layer-6 grant (`GrantAbility`/`GrantTrigger`/`GrantStaticAbility`), an
/// all-of-source grant filter, or a dynamic P/T/counter value can each nest a
/// frozen tag. Exhaustive & wildcard-free so a future modification is classified.
fn legacy_continuous_modification(m: &ContinuousModification) -> bool {
    match m {
        ContinuousModification::GrantAbility { definition } => legacy_definition(definition),
        ContinuousModification::GrantStaticAbility { definition } => {
            legacy_static_definition(definition)
        }
        ContinuousModification::GrantTrigger { trigger } => legacy_trigger_definition(trigger),
        ContinuousModification::GrantAllActivatedAbilitiesOf { source, .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { source } => {
            legacy_target_filter(source)
        }
        ContinuousModification::SetDynamicPower { value }
        | ContinuousModification::SetDynamicToughness { value }
        | ContinuousModification::SetPowerDynamic { value }
        | ContinuousModification::SetToughnessDynamic { value }
        | ContinuousModification::AddDynamicPower { value }
        | ContinuousModification::AddDynamicToughness { value }
        | ContinuousModification::AddDynamicKeyword { value, .. }
        | ContinuousModification::AddCounterOnEnter { count: value, .. } => {
            legacy_quantity_expr(value)
        }
        ContinuousModification::CopyValues { .. }
        | ContinuousModification::SetName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::AddKeyword { .. }
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor { .. }
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        // A granted rule-modification mode (`AddStaticMode`) carries no frozen tag
        // in the corpus; the §5.2 sweep is the arbiter if one ever hides there.
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        // CR 612.8 + 613.1c: Layer-3 name-set from source's chosen name (Psychic
        // Paper); a granted continuous mod, no frozen event-context tag.
        | ContinuousModification::SetChosenName
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => false,
    }
}

/// A granted / emblem `TriggerDefinition` can carry a frozen tag in its firing
/// filters (`valid_card`/`valid_source`), its intervening-if condition, or its
/// execute body.
fn legacy_trigger_definition(td: &TriggerDefinition) -> bool {
    td.execute.as_deref().is_some_and(legacy_definition)
        || td.condition.as_ref().is_some_and(legacy_trigger_condition)
        || td.valid_card.as_ref().is_some_and(legacy_target_filter)
        || td.valid_source.as_ref().is_some_and(legacy_target_filter)
}

/// D5: every effect position where a frozen tag can appear. `{ .. }` elides
/// fields that carry no tag-bearing enum; the match is exhaustive at the VARIANT
/// level (a new `Effect` variant fails to compile). Effect TARGET/COUNT positions
/// are checked here even where the read/write profile walk drops them — this is
/// the class of 50 D5 holes this visitor closes (CR 603.10a).
fn legacy_effect(x: &Effect) -> bool {
    // Small helpers for optional carriers.
    let otf = |o: &Option<TargetFilter>| o.as_ref().is_some_and(legacy_target_filter);
    let oqe = |o: &Option<QuantityExpr>| o.as_ref().is_some_and(legacy_quantity_expr);
    let ocr = |o: &Option<ControllerRef>| o.as_ref().is_some_and(legacy_controller_ref);
    let odur = |o: &Option<Duration>| o.as_ref().is_some_and(legacy_duration);
    let odef = |o: &Option<Box<AbilityDefinition>>| o.as_deref().is_some_and(legacy_definition);
    match x {
        // ---- Single `TargetFilter` target (only tag-bearing field) ----
        Effect::Pump { target, .. }
        | Effect::PairWith { target }
        | Effect::Destroy { target, .. }
        | Effect::Regenerate { target }
        | Effect::RemoveAllDamage { target }
        | Effect::Counter { target, .. }
        | Effect::CounterAll { target }
        | Effect::SetTapState { target, .. }
        | Effect::MultiplyCounter { target, .. }
        | Effect::DoublePT { target, .. }
        | Effect::DoublePTAll { target, .. }
        | Effect::PumpAll { target, .. }
        | Effect::GainControl { target }
        | Effect::GainControlAll { target }
        | Effect::ControlNextTurn { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::DestroyAll { target, .. }
        | Effect::SwitchPT { target }
        | Effect::ExileHaunting { target }
        | Effect::HideawayConceal { target }
        | Effect::ChooseCard { target, .. }
        | Effect::Transform { target }
        | Effect::Shuffle { target }
        | Effect::Reveal { target }
        | Effect::TargetOnly { target }
        | Effect::Suspect { target, .. }
        | Effect::Unsuspect { target, .. }
        | Effect::PhaseOut { target }
        | Effect::PhaseIn { target }
        | Effect::ForceBlock { target }
        | Effect::BecomePrepared { target }
        | Effect::BecomeUnprepared { target }
        | Effect::BecomeSaddled { target }
        | Effect::ProliferateTarget { target }
        | Effect::Exploit { target }
        | Effect::LoseAllPlayerCounters { target }
        | Effect::Heist { target, .. }
        | Effect::PutOnTopOrBottom { target }
        | Effect::Goad { target }
        | Effect::GoadAll { target }
        | Effect::Detain { target }
        | Effect::SetRoomDoorLock { target, .. }
        | Effect::RemoveFromCombat { target }
        | Effect::BecomeBlocked { target }
        | Effect::ApplyPerpetual { target, .. }
        | Effect::TurnFaceUp { target }
        | Effect::TurnFaceDown { target, .. }
        | Effect::ExtraTurn { target }
        | Effect::Double { target, .. }
        | Effect::CrankContraptions { target }
        | Effect::ReassembleContraption { target, .. }
        | Effect::AssembleContraptionOnSprocket { target, .. }
        | Effect::ReassembleContraptionOnSprocket { target, .. }
        | Effect::ApplySticker { target, .. }
        | Effect::RememberCard { target }
        | Effect::GrantCastingPermission { target, .. }
        | Effect::AddTargetReplacement { target, .. }
        | Effect::DiscardCard { target, .. }
        | Effect::Animate { target, .. } => legacy_target_filter(target),

        Effect::GainActivatedAbilitiesOfTarget {
            target,
            recipient,
            duration,
            // CR 602.1: `GrantedAbilityScope` (ActivatedOnly / AllOther) is a leaf
            // grant-scope enum carrying no TargetFilter / QuantityExpr / ControllerRef
            // ⇒ no frozen event-context tag.
            scope: _,
        } => legacy_target_filter(target) || legacy_target_filter(recipient) || odur(duration),

        // ---- Single filter-typed field under a different name ----
        Effect::ExploreAll { filter: target, .. }
        // CR 701.4a: behold's `filter` is the revealed/chosen quality — walked for a
        // nested frozen event-context tag like every single-filter effect.
        | Effect::Behold { filter: target }
        | Effect::FreeCastFromZones { filter: target, .. }
        | Effect::ChooseDamageSource {
            source_filter: target,
        }
        | Effect::ReturnAsAura {
            enchant_filter: target,
            ..
        }
        | Effect::RevealTop { player: target, .. }
        | Effect::BlightEffect { player: target, .. }
        | Effect::ExileFromTopUntil { player: target, .. }
        | Effect::ExchangeLifeWithStat { player: target, .. } => legacy_target_filter(target),

        // `host` is `Box<TargetFilter>` — deref-coerced in the call.
        Effect::CombineHost { host, .. } => legacy_target_filter(host),

        // ---- `count`/`amount` (QuantityExpr) + `target` ----
        Effect::Draw { count, target }
        | Effect::Mill { count, target, .. }
        | Effect::Scry { count, target }
        | Effect::Surveil { count, target }
        | Effect::RemoveCounter { count, target, .. }
        | Effect::Sacrifice { target, count, .. }
        | Effect::PutCounter { count, target, .. }
        | Effect::PutCounterAll { count, target, .. }
        | Effect::Connive { target, count }
        | Effect::GivePlayerCounter { count, target, .. }
        | Effect::PutAtLibraryPosition { target, count, .. }
        | Effect::SkipNextTurn { target, count }
        | Effect::SkipNextStep { target, count, .. }
        | Effect::AdditionalPhase { target, count, .. }
        | Effect::GrantExtraLoyaltyActivations {
            amount: count,
            target,
        } => legacy_quantity_expr(count) || legacy_target_filter(target),
        // CR 701.58a: `object_source` (Some) names already-chosen objects to cloak —
        // an `Option<TargetFilter>` that can nest a frozen event-context tag, so it is
        // walked here (the shared `{ target, count }` group above cannot).
        Effect::Cloak {
            target,
            count,
            object_source,
        } => legacy_quantity_expr(count) || legacy_target_filter(target) || otf(object_source),
        Effect::SetLifeTotal { target, amount } | Effect::DealDamage { amount, target, .. } => {
            legacy_quantity_expr(amount) || legacy_target_filter(target)
        }
        Effect::Endure {
            amount: count,
            subject: target,
        }
        | Effect::Discover {
            mana_value_limit: count,
            player: target,
        }
        | Effect::ExileTop {
            count,
            player: target,
            ..
        } => legacy_quantity_expr(count) || legacy_target_filter(target),

        // ---- `count`-only (QuantityExpr) ----
        Effect::Monstrosity { count }
        | Effect::Incubate { count }
        | Effect::Amass { count, .. }
        | Effect::Renown { count }
        | Effect::Bolster { count }
        | Effect::Adapt { count }
        | Effect::AssembleContraptions { count }
        | Effect::AddPendingETBCounters { count, .. } => legacy_quantity_expr(count),
        // Deferred continuous-modification carrier — no `count`; descend the mods
        // for a nested frozen tag (AddType/AddSubtype return `false`).
        Effect::AddPendingEntersModifications { modifications } => {
            modifications.iter().any(legacy_continuous_modification)
        }
        Effect::GainEnergy { amount } | Effect::Intensify { amount, .. } => {
            legacy_quantity_expr(amount)
        }

        // ---- Pairs of filters / mixed shapes ----
        Effect::Fight { target, subject } => {
            legacy_target_filter(target) || legacy_target_filter(subject)
        }
        Effect::EachDealsDamageEqualToPower {
            sources,
            recipient,
            extra_source,
        } => {
            legacy_target_filter(sources)
                || legacy_target_filter(recipient)
                || extra_source.as_ref().is_some_and(legacy_target_filter)
        }
        // CR 120: each source deals damage; `recipient` (`Shared`) can be a context
        // anaphor (ParentTarget/TriggeringSource) ⇒ descend all tag-bearing fields.
        Effect::EachSourceDealsDamage {
            sources,
            amount,
            recipient,
        } => {
            legacy_target_filter(sources)
                || legacy_quantity_expr(amount)
                || match recipient {
                    crate::types::ability::EachDamageRecipient::Shared(f) => {
                        legacy_target_filter(f)
                    }
                    crate::types::ability::EachDamageRecipient::EachController => false,
                }
        }
        Effect::ChooseCounterKind { target } => legacy_target_filter(target),
        Effect::PutChosenCounter { target, count } => {
            legacy_quantity_expr(count) || legacy_target_filter(target)
        }
        Effect::ChooseCounterAdjustment { count, .. } => legacy_quantity_expr(count),
        Effect::CreatePlaneswalkReplacement { replacement_effect } => {
            legacy_effect(replacement_effect)
        }
        Effect::OpponentGuess { guesser, subject } => {
            legacy_controller_ref(guesser) || legacy_guess_subject(subject)
        }
        // Payload-less keyword action (planar chaos, CR 311.7) — no tag-bearing field.
        Effect::ChaosEnsues => false,
        // Payload-less self-gathering effect (redistribute life totals, CR 119.7 + CR 119.8)
        // — no tag-bearing field.
        Effect::RedistributeLifeTotals => false,
        // Payload-less keyword action (reverse turn order, CR 103.1) — no
        // tag-bearing field and reads no per-object state.
        Effect::ReverseTurnOrder => false,
        Effect::SwapChosenLabels {
            first: _,
            second: _,
        } => false,
        Effect::Attach { attachment, target } | Effect::UnattachAll { attachment, target } => {
            legacy_target_filter(attachment) || legacy_target_filter(target)
        }
        Effect::ExchangeControl { target_a, target_b }
        | Effect::ExchangeLifeTotals {
            player_a: target_a,
            player_b: target_b,
        } => legacy_target_filter(target_a) || legacy_target_filter(target_b),
        Effect::GiveControl { target, recipient }
        | Effect::CopyTokenBlockingAttacker {
            source_filter: target,
            owner: recipient,
        }
        | Effect::ChooseObjectsIntoTrackedSet {
            chooser: target,
            filter: recipient,
            ..
        } => legacy_target_filter(target) || legacy_target_filter(recipient),
        Effect::ChooseAugmentAndCombineWithHost { filter, host, .. } => {
            legacy_target_filter(filter) || legacy_target_filter(host)
        }
        Effect::GainLife { amount, player } => {
            legacy_quantity_expr(amount) || legacy_target_filter(player)
        }
        Effect::LoseLife { amount, target } => legacy_quantity_expr(amount) || otf(target),
        Effect::DamageAll {
            amount,
            target,
            player_filter,
            ..
        } => {
            legacy_quantity_expr(amount)
                || legacy_target_filter(target)
                || player_filter.as_ref().is_some_and(legacy_player_filter)
        }
        Effect::DamageEachPlayer {
            amount,
            player_filter,
        } => legacy_quantity_expr(amount) || legacy_player_filter(player_filter),
        Effect::Discard {
            count,
            target,
            unless_filter,
            filter,
            ..
        } => {
            legacy_quantity_expr(count)
                || legacy_target_filter(target)
                || otf(unless_filter)
                || otf(filter)
        }
        Effect::Dig {
            player,
            count,
            filter,
            ..
        } => {
            legacy_target_filter(player)
                || legacy_quantity_expr(count)
                || legacy_target_filter(filter)
        }
        Effect::Seek { filter, count, .. } | Effect::SearchOutsideGame { filter, count, .. } => {
            legacy_target_filter(filter) || legacy_quantity_expr(count)
        }
        Effect::SearchLibrary {
            filter,
            count,
            target_player,
            ..
        } => legacy_target_filter(filter) || legacy_quantity_expr(count) || otf(target_player),
        Effect::ChooseAndSacrificeRest {
            choose_filter,
            sacrifice_filter,
            total_power_cap,
            ..
        } => {
            legacy_target_filter(choose_filter)
                || legacy_target_filter(sacrifice_filter)
                || oqe(total_power_cap)
        }
        Effect::EachPlayerCopyChosen {
            choose_filter,
            min: _,
            max: _,
            copy_modifications,
            scale: _,
            choose_scope: _,
        } => {
            legacy_target_filter(choose_filter)
                || copy_modifications
                    .iter()
                    .any(legacy_continuous_modification)
        }
        Effect::ChangeSpeed {
            player_scope,
            amount,
            ..
        } => legacy_player_filter(player_scope) || legacy_quantity_expr(amount),
        Effect::StartYourEngines { player_scope } => legacy_player_filter(player_scope),
        Effect::ChooseOneOf { chooser, branches } => {
            legacy_player_filter(chooser) || branches.iter().any(legacy_definition)
        }
        Effect::PutSticker {
            target,
            count,
            max_ticket_cost,
            ..
        } => legacy_target_filter(target) || legacy_quantity_expr(count) || oqe(max_ticket_cost),
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count,
            life_payment,
            player,
        } => {
            legacy_quantity_expr(count)
                || legacy_quantity_expr(life_payment)
                || legacy_target_filter(player)
        }

        // ---- Options-only carriers ----
        Effect::Mana { target, .. }
        | Effect::LoseTheGame { target }
        | Effect::WinTheGame { target } => otf(target),
        Effect::ChooseFromZone { filter, .. } => otf(filter),
        Effect::ReduceNextSpellCost { spell_filter, .. }
        | Effect::GrantNextSpellAbility { spell_filter, .. } => otf(spell_filter),
        Effect::PreventDamage {
            amount_dynamic,
            target,
            ..
        } => oqe(amount_dynamic) || legacy_target_filter(target),
        Effect::PayCost { scale, payer, .. } => oqe(scale) || legacy_target_filter(payer),
        Effect::ChangeTargets {
            target, forced_to, ..
        } => legacy_target_filter(target) || otf(forced_to),
        Effect::CreateDamageReplacement {
            source_filter,
            redirect_object_filter,
            recipient_object_filter,
            ..
        } => otf(source_filter) || otf(redirect_object_filter) || otf(recipient_object_filter),

        // ---- ControllerRef / Duration riders on a target ----
        Effect::Manifest {
            target,
            count,
            enters_under,
            ..
        } => legacy_target_filter(target) || legacy_quantity_expr(count) || ocr(enters_under),
        Effect::RevealUntil {
            player,
            filter,
            count,
            enters_under,
            ..
        } => {
            legacy_target_filter(player)
                || legacy_target_filter(filter)
                || legacy_quantity_expr(count)
                || ocr(enters_under)
        }
        Effect::BecomeCopy {
            target, duration, ..
        }
        | Effect::CastFromZone {
            target, duration, ..
        } => legacy_target_filter(target) || odur(duration),
        Effect::GenericEffect {
            duration,
            target,
            static_abilities,
        } => odur(duration) || otf(target) || static_abilities.iter().any(legacy_static_definition),
        Effect::CreateEmblem { statics, triggers } => {
            statics.iter().any(legacy_static_definition)
                || triggers.iter().any(legacy_trigger_definition)
        }
        Effect::ForceAttack {
            target,
            required_player,
            duration,
        } => {
            legacy_target_filter(target)
                || legacy_target_filter(required_player)
                || legacy_duration(duration)
        }

        // ---- Token creation / counters with enter-with-counters ----
        Effect::Token {
            count,
            owner,
            attach_to,
            enter_with_counters,
            static_abilities,
            ..
        } => {
            legacy_quantity_expr(count)
                || legacy_target_filter(owner)
                || otf(attach_to)
                || enter_with_counters
                    .iter()
                    .any(|(_, q)| legacy_quantity_expr(q))
                || static_abilities.iter().any(legacy_static_definition)
        }
        Effect::CopyTokenOf {
            target,
            owner,
            source_filter,
            count,
            ..
        } => {
            legacy_target_filter(target)
                || legacy_target_filter(owner)
                || otf(source_filter)
                || legacy_quantity_expr(count)
        }
        Effect::CreateTokenCopyFromPool {
            owner,
            type_filter,
            mv_bound,
            count,
            ..
        } => {
            legacy_target_filter(owner)
                || legacy_target_filter(type_filter)
                || legacy_quantity_expr(mv_bound)
                || legacy_quantity_expr(count)
        }
        Effect::ChangeZone {
            target,
            enters_under,
            enter_with_counters,
            conditional_enter_with_counters,
            enters_modified_if,
            ..
        } => {
            legacy_target_filter(target)
                || ocr(enters_under)
                || enter_with_counters
                    .iter()
                    .any(|(_, q)| legacy_quantity_expr(q))
                || conditional_enter_with_counters
                    .iter()
                    .any(|(f, _, q)| legacy_target_filter(f) || legacy_quantity_expr(q))
                || otf(enters_modified_if)
        }
        Effect::ChangeZoneAll {
            target,
            enters_under,
            enter_with_counters,
            ..
        } => {
            legacy_target_filter(target)
                || ocr(enters_under)
                || enter_with_counters
                    .iter()
                    .any(|(_, q)| legacy_quantity_expr(q))
        }
        Effect::MoveCounters {
            source,
            count,
            target,
            ..
        } => legacy_target_filter(source) || oqe(count) || legacy_target_filter(target),
        Effect::BounceAll { target, count, .. } | Effect::CastCopyOfCard { target, count, .. } => {
            legacy_target_filter(target) || oqe(count)
        }
        Effect::RevealHand {
            target,
            card_filter,
            count,
            ..
        } => legacy_target_filter(target) || legacy_target_filter(card_filter) || oqe(count),

        // ---- Nested ability/effect bodies ----
        Effect::Vote {
            per_choice_effect,
            starting_with,
            ..
        } => {
            legacy_controller_ref(starting_with)
                || per_choice_effect.iter().any(|d| legacy_definition(d))
        }
        Effect::SeparateIntoPiles {
            object_filter,
            chosen_pile_effect,
            unchosen_pile_effect,
            ..
        } => {
            legacy_target_filter(object_filter)
                || legacy_definition(chosen_pile_effect)
                || unchosen_pile_effect
                    .as_ref()
                    .is_some_and(|d| legacy_definition(d))
        }
        Effect::EpicCopy { spell } => contains_legacy_event_ref(spell),
        Effect::CreateDelayedTrigger { effect, .. } => legacy_definition(effect),
        Effect::CreateDrawReplacement { replacement_effect } => legacy_effect(replacement_effect),
        Effect::RollDie { count, results, .. } => {
            legacy_quantity_expr(count) || results.iter().any(|r| legacy_definition(&r.effect))
        }
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            flipper,
        } => odef(win_effect) || odef(lose_effect) || legacy_target_filter(flipper),
        Effect::FlipCoins {
            count,
            win_effect,
            lose_effect,
            flipper,
        } => {
            legacy_quantity_expr(count)
                || odef(win_effect)
                || odef(lose_effect)
                || legacy_target_filter(flipper)
        }
        Effect::FlipCoinUntilLose { win_effect } => legacy_definition(win_effect),
        Effect::RevealFromHand {
            filter, on_decline, ..
        } => legacy_target_filter(filter) || odef(on_decline),
        Effect::CopySpell { target, copier, .. } => legacy_target_filter(target) || ocr(copier),

        // ---- No tag-bearing field (info / terminal / plumbing / undescended
        // sub-structures the profile walk also does not descend for legacy —
        // §5.2 sweep is the arbiter). ----
        Effect::Explore
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::Populate
        | Effect::Clash
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Myriad
        | Effect::Encore
        | Effect::Meld { .. }
        | Effect::RegisterBending { .. }
        | Effect::Cleanup { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Specialize
        | Effect::SolveCase
        | Effect::SetClassLevel { .. }
        | Effect::AddRestriction { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::Planeswalk
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::ProcessRadCounters
        | Effect::ForEachCategory { .. }
        | Effect::GiftDelivery { .. }
        | Effect::SetDayNight { .. }
        | Effect::Conjure { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::HeistExile
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::ManifestDread
        | Effect::Choose { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::Unimplemented { .. } => false,
    }
}

fn legacy_guess_subject(subject: &GuessSubject) -> bool {
    match subject {
        GuessSubject::CommittedChoice { choice_type } => legacy_choice_type(choice_type),
        GuessSubject::Proposition {
            lhs,
            comparator: _,
            rhs,
        } => legacy_quantity_expr(lhs) || legacy_quantity_expr(rhs),
    }
}

fn legacy_choice_type(choice_type: &crate::types::ability::ChoiceType) -> bool {
    match choice_type {
        crate::types::ability::ChoiceType::Opponent { restriction } => {
            restriction.as_deref().is_some_and(legacy_player_filter)
        }
        crate::types::ability::ChoiceType::CreatureType { .. }
        | crate::types::ability::ChoiceType::Color { .. }
        | crate::types::ability::ChoiceType::OddOrEven
        | crate::types::ability::ChoiceType::BasicLandType
        | crate::types::ability::ChoiceType::CardType { .. }
        | crate::types::ability::ChoiceType::CardName
        | crate::types::ability::ChoiceType::NumberRange { .. }
        | crate::types::ability::ChoiceType::Labeled { .. }
        | crate::types::ability::ChoiceType::LandType
        | crate::types::ability::ChoiceType::CardPredicate { .. }
        | crate::types::ability::ChoiceType::CardPredicateGuess { .. }
        | crate::types::ability::ChoiceType::Player
        | crate::types::ability::ChoiceType::TwoColors
        | crate::types::ability::ChoiceType::Word
        | crate::types::ability::ChoiceType::Artist
        | crate::types::ability::ChoiceType::Keyword { .. }
        | crate::types::ability::ChoiceType::CounterKind { .. } => false,
    }
}

// ---------------------------------------------------------------------------
// Read builders.
// ---------------------------------------------------------------------------

fn reads_board_of(k: StateKind) -> RwProfile {
    let mut p = RwProfile::empty();
    p.reads_board = KindSet::one(k);
    p
}
/// CR 400.1 + CR 205: a whole-zone / unfiltered `SetMembership` board read
/// (GraveyardSize, Devotion, …) over a zone's contents; the census is a
/// card-type tag-set (CR 205). The filter is fully unextractable, so its census is `Census::Any`
/// (§2: unextractable ⇒ overlap assumed, fail-CLOSED). Must NOT use
/// `reads_board_of(SetMembership)`, which leaves `Census::None` (the write-side
/// "no object moved" sentinel) and would make the membership feed row never fire.
fn reads_zone_membership() -> RwProfile {
    let mut p = reads_board_of(StateKind::SetMembership);
    p.reads_membership_census = Census::Any;
    // CR 400.1: a whole-zone read's zone is not extractable here (GraveyardSize
    // = graveyard, Devotion = battlefield, … — one helper, many zones) ⇒ `Any`,
    // fail-closed (conflicts with every membership write, as before).
    p.reads_membership_zones = ZoneSpan::Any;
    p
}
fn reads_player_of(k: StateKind) -> RwProfile {
    let mut p = RwProfile::empty();
    p.reads_player = KindSet::one(k);
    p
}
fn reads_src_of(k: StateKind) -> RwProfile {
    let mut p = RwProfile::empty();
    p.reads_src = KindSet::one(k);
    p
}
/// CR 603.10a: a source-referential look-back / cast-time fact — frozen, never
/// sibling-fed, but marks source-dependence (`source_independent` false).
fn frozen_source_read() -> RwProfile {
    let mut p = RwProfile::empty();
    p.reads_frozen = KindSet::one(StateKind::SetMembership);
    p
}
fn reads_frozen_of(k: StateKind) -> RwProfile {
    let mut p = RwProfile::empty();
    p.reads_frozen = KindSet::one(k);
    p
}
fn reads_event_live() -> RwProfile {
    let mut p = RwProfile::empty();
    p.reads_event_live = true;
    p
}
fn legacy_ref() -> RwProfile {
    let mut p = reads_event_live();
    p.legacy_batch_prompt = true;
    p
}
/// CR 603.10a (PR-6.75 c5): a standalone per-source look-back player referent
/// (source-chosen / attachment-relative) at a scope/anchor position — carries the
/// member-bound channel `member_bound_controller_ref` classifies for `Typed`
/// filters, fail-closed at the scope positions that never route through a filter.
fn member_bound_read() -> RwProfile {
    let mut p = RwProfile::empty();
    p.reads_member_bound = true;
    p
}
fn writes_pool_profile() -> RwProfile {
    let mut p = RwProfile::empty();
    p.writes_pool = true;
    p
}
fn ext_write(k: StateKind) -> RwProfile {
    let mut p = RwProfile::empty();
    p.writes_external.set(k);
    p
}
fn self_write(k: StateKind) -> RwProfile {
    let mut p = RwProfile::empty();
    p.writes_self.set(k);
    p
}

/// A board aggregate read over `filter`: `reads_board{SetMembership}` (with
/// census) unless the filter is provably self-scoped ⇒ per-member-private
/// `reads_src` (§2).
fn board_membership_read(filter: &TargetFilter) -> RwProfile {
    let mut p = RwProfile::empty();
    if filter_is_self_scoped(filter) {
        p.reads_src = KindSet::one(StateKind::SetMembership);
    } else {
        p.reads_board = KindSet::one(StateKind::SetMembership);
    }
    p.reads_membership_census = census_of_filter(filter);
    // CR 400.1: the zone(s) the read counts, from the filter's explicit `InZone`
    // (Tombstone Stairwell's Zombie count reads the GRAVEYARD, not the battlefield
    // its own tokens enter). No `InZone` ⇒ `Any` (fail-closed, unchanged behavior).
    p.reads_membership_zones = zones_of_filter(filter);
    // §4.3.7 (CR 110.2): the controller-field span the read observes (Defense of
    // the Heart's "creatures an opponent controls" ⇒ Opponents). Unrefined /
    // composite filters ⇒ Any.
    p.reads_membership_ctrl = ctrl_span_of_filter(filter);
    // CR 603.10a (PR-6.75 c5): a read whose filter names a per-member-bound set /
    // attachment / chosen referent is member-distinct ⇒ refuses batch-T1.
    p.reads_member_bound |= member_bound_target_filter(filter);
    p
}

/// §L2 (CR 400.7 + CR 400.1): a zone-change per-turn journal read, keyed to its
/// DESTINATION zone when known. A `None` destination stays fail-closed (`Any`
/// zones, via `board_membership_read`). Never overrides a self-scoped read (whose
/// filter routes to `reads_src`, not the membership feed).
fn journal_zone_change_read(filter: &TargetFilter, to: Option<&Zone>) -> RwProfile {
    let mut p = board_membership_read(filter);
    if let (Some(z), false) = (to, filter_is_self_scoped(filter)) {
        p.reads_membership_zones = ZoneSpan::one(*z);
    }
    p
}

/// A board VALUE aggregate (power/counter aggregate) over `filter`: records the
/// value kind AND `SetMembership` (a membership write changes the aggregate, §2).
fn board_value_aggregate_read(filter: &TargetFilter, value: StateKind) -> RwProfile {
    let mut p = board_membership_read(filter);
    if filter_is_self_scoped(filter) {
        p.reads_src.set(value);
    } else {
        p.reads_board.set(value);
    }
    p
}

/// Read an object characteristic at a given scope (§2 read-carrier closure):
/// Source ⇒ `reads_src`; Recipient ⇒ nothing (read-modify-write); event objects
/// ⇒ `reads_event_live`; other object scopes ⇒ `reads_board`.
fn read_object_scope(scope: &ObjectScope, kind: StateKind) -> RwProfile {
    match scope {
        ObjectScope::Source => reads_src_of(kind),
        ObjectScope::Recipient => RwProfile::empty(),
        ObjectScope::Target | ObjectScope::Anaphoric | ObjectScope::Demonstrative => {
            reads_board_of(kind)
        }
        ObjectScope::AmassedArmy => member_bound_read(),
        ObjectScope::EventSource | ObjectScope::EventTarget => reads_event_live(),
        // §L7 precedent (CR 608.2c): a per-resolution local surfaced by THIS
        // ability's own reveal within the same resolution — observed by no
        // sibling write, and its read is the card's intrinsic printed MV (not a
        // mutable board characteristic a sibling reorders). Mirrors
        // `ObjectScope::Recipient` and the `LastRevealed => empty` classification;
        // contributes no observable `reads_board`/`reads_src`.
        ObjectScope::OtherRevealedCard => RwProfile::empty(),
        // D5 carrier: `CostPaidObject` is one of the 12 retained refs.
        ObjectScope::CostPaidObject => legacy_ref(),
    }
}

/// §L7 (CR 608.2c + CR 603.3b): classify one `ObjectsShareQuality` operand by
/// scope. `LastRevealed` is the card the member's OWN reveal surfaced this
/// resolution — a per-resolution local (like `ObjectScope::Recipient`), observed
/// by no sibling write (mirrors `RevealedHasCardType => EMPTY`). A source operand
/// reads the source's own FIXED printed type — not a mutable board characteristic
/// a sibling reorders — so it too contributes NO observable read (crucially it
/// must NOT set `reads_src`, which would flip an otherwise source-INDEPENDENT
/// ability off the sound T1 fast path and re-expose an unrelated board-read×write
/// as a false prompt — Plane-Merge Elf's PumpAll, Sensation Gorger's HandSize
/// discard). Every OTHER operand (a live board object) keeps the fail-closed
/// board `ObjectPt` characteristic read.
fn share_quality_operand_read(f: &TargetFilter) -> RwProfile {
    match f {
        TargetFilter::LastRevealed | TargetFilter::SelfRef | TargetFilter::SourceOrPaired => {
            RwProfile::empty()
        }
        // Fail-closed: any other reference is a live board characteristic read.
        _ => reads_board_of(StateKind::ObjectPt),
    }
}

/// (player_recipient, object_recipient) for a damage/target filter — recipient
/// classification (CR 704.5a / CR 800.4a source-actor residual documented in the
/// module doc; damage kinds are recipient-classified, not source-bound).
fn target_recipient(f: &TargetFilter) -> (bool, bool) {
    match f {
        TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::Owner
        | TargetFilter::AllPlayers
        | TargetFilter::DefendingPlayer
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringSourceController
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTargetOwner => (true, false),
        // "any target" and player-or-object filters ⇒ both recipients.
        TargetFilter::Any | TargetFilter::Or { .. } => (true, true),
        // Everything else is object-scoped for damage purposes.
        _ => (false, true),
    }
}

// ---------------------------------------------------------------------------
// Ability walk (mirrors `resolved_ability_axes`, with chain-root threading).
// ---------------------------------------------------------------------------

fn walk_ability(
    a: &ResolvedAbility,
    chain_root: Option<WriteScope>,
    chain_move_owner: Option<PlayerSpan>,
    pscope_in: PlayerSpan,
    acc: &mut RwProfile,
) {
    let ResolvedAbility {
        effect,
        sub_ability,
        else_ability,
        condition,
        duration,
        player_scope,
        starting_with,
        repeat_for,
        announced_x,
        multi_target,
        target_constraints,
        unless_pay,
        target_chooser,
        repeat_until,
        modal,
        mode_abilities,
        targets: _,
        source_id: _,
        source_incarnation: _,
        source_card_id: _, // latched card identity token (AllCopies yield), no read/write effect
        controller: _,
        original_controller: _,
        scoped_player: _,
        kind: _,
        context: _,
        optional_targeting: _,
        optional: _,
        optional_for: _,
        target_choice_timing: _,
        description: _,
        selected_mode_labels: _, // display snapshots, no game-state read/write
        min_x_value: _,          // u32, no read
        cant_be_copied: _,
        copy_count_status: _,
        forward_result: _,
        distribution: _,
        chosen_x: _,
        cost_paid_object: _,
        effect_context_object: _,
        amassed_army_object: _,
        ability_index: _,
        may_trigger_origin: _,
        target_selection_mode: _,
        chosen_players: _,
        sub_link: _,
        replacement_applied: _,
        parent_target_missing_reason: _,
    } = a;

    // §4.3.2: a definition's own `player_scope` overrides the inherited scope for
    // its effect and descendants (matches runtime scoped iteration).
    let pscope = player_scope
        .as_ref()
        .map_or(pscope_in, player_span_of_filter);
    let (eff, own_scope) = rw_effect(effect, chain_root, pscope, chain_move_owner);
    acc.merge(eff);
    let child_root = own_scope.or(chain_root);
    let child_move_owner = effect_move_owner(effect).or(chain_move_owner);

    if let Some(sub) = sub_ability {
        walk_ability(sub, child_root, child_move_owner, pscope, acc);
    }
    if let Some(els) = else_ability {
        walk_ability(els, chain_root, chain_move_owner, pscope, acc);
    }
    if let Some(c) = condition {
        acc.merge(rw_ability_condition(c));
    }
    if let Some(d) = duration {
        acc.merge(rw_duration(d));
    }
    if let Some(ps) = player_scope {
        acc.merge(rw_player_filter(ps));
    }
    if let Some(sw) = starting_with {
        acc.merge(rw_controller_ref(sw));
    }
    if let Some(rf) = repeat_for {
        acc.merge(rw_quantity_expr(rf));
    }
    // CR 601.2b: the announce-time-locked X definition is a live board read, merely
    // read at announcement rather than at resolution. Same read-kind as any other
    // quantity slot.
    if let Some(ax) = announced_x {
        acc.merge(rw_quantity_expr(ax));
    }
    if let Some(MultiTargetSpec { min, max }) = multi_target {
        acc.merge(rw_quantity_expr(min));
        if let Some(max) = max {
            acc.merge(rw_quantity_expr(max));
        }
    }
    for c in target_constraints {
        acc.merge(rw_target_constraint(c));
    }
    // CR 603.5: unless-pay is a resolution-time choice — no read-kind, but arms
    // the Mana×unless-pay guard.
    if unless_pay.is_some() {
        acc.has_pay_or_unless = true;
    }
    if let Some(tc) = target_chooser {
        acc.merge(rw_target_filter(tc));
    }
    if let Some(ru) = repeat_until {
        acc.merge(rw_repeat_continuation(ru));
    }
    if let Some(m) = modal {
        acc.merge(rw_modal_choice(m));
    }
    // §voltstorm (CR 700.2): a modal resolves exactly ONE mode. Descend every
    // mode as a UNION (mirrors `Effect::ChooseOneOf` branch descent): if the union
    // of all modes' reads/writes has no feed, then no single choice — and no
    // cross-choice sibling pair — is order-observable. Independent reflexive-modal
    // choices commute (Voltstorm Angel). Fail-closed: the union over-approximates.
    for m in mode_abilities {
        walk_definition(m, child_root, child_move_owner, pscope, acc);
    }
}

/// Descend a choice/RNG sub-body (`AbilityDefinition`). `..`-free so a future
/// field forces a decision (§2 choice-wrapper / RNG union descent).
fn walk_definition(
    a: &AbilityDefinition,
    chain_root: Option<WriteScope>,
    chain_move_owner: Option<PlayerSpan>,
    pscope_in: PlayerSpan,
    acc: &mut RwProfile,
) {
    let AbilityDefinition {
        effect,
        sub_ability,
        else_ability,
        condition,
        duration,
        player_scope,
        starting_with,
        repeat_for,
        announced_x,
        multi_target,
        target_constraints,
        unless_pay,
        modal,
        mode_abilities,
        target_chooser,
        repeat_until,
        kind: _,
        cost: _,
        description: _,
        target_prompt: _,
        activation_restrictions: _,
        activator_filter: _,
        activation_zone: _,
        ability_tag: _,
        optional_targeting: _,
        optional: _,
        optional_for: _,
        target_choice_timing: _,
        distribute: _,
        min_x_value: _,
        cant_be_copied: _,
        cost_reduction: _,
        forward_result: _,
        target_selection_mode: _,
        sub_link: _,
        iteration_kind_binding: _,
    } = a;

    // §4.3.2: own `player_scope` overrides the inherited scope (Brink's Discard
    // sub-ability carries `player_scope: Opponent`).
    let pscope = player_scope
        .as_ref()
        .map_or(pscope_in, player_span_of_filter);
    let (eff, own_scope) = rw_effect(effect, chain_root, pscope, chain_move_owner);
    acc.merge(eff);
    let child_root = own_scope.or(chain_root);
    let child_move_owner = effect_move_owner(effect).or(chain_move_owner);

    if let Some(sub) = sub_ability {
        walk_definition(sub, child_root, child_move_owner, pscope, acc);
    }
    if let Some(els) = else_ability {
        walk_definition(els, chain_root, chain_move_owner, pscope, acc);
    }
    if let Some(c) = condition {
        acc.merge(rw_ability_condition(c));
    }
    if let Some(d) = duration {
        acc.merge(rw_duration(d));
    }
    if let Some(ps) = player_scope {
        acc.merge(rw_player_filter(ps));
    }
    if let Some(sw) = starting_with {
        acc.merge(rw_controller_ref(sw));
    }
    if let Some(rf) = repeat_for {
        acc.merge(rw_quantity_expr(rf));
    }
    // CR 601.2b: the announce-time-locked X definition is a live board read, merely
    // read at announcement rather than at resolution. Same read-kind as any other
    // quantity slot.
    if let Some(ax) = announced_x {
        acc.merge(rw_quantity_expr(ax));
    }
    if let Some(MultiTargetSpec { min, max }) = multi_target {
        acc.merge(rw_quantity_expr(min));
        if let Some(max) = max {
            acc.merge(rw_quantity_expr(max));
        }
    }
    for c in target_constraints {
        acc.merge(rw_target_constraint(c));
    }
    if unless_pay.is_some() {
        acc.has_pay_or_unless = true;
    }
    if let Some(tc) = target_chooser {
        acc.merge(rw_target_filter(tc));
    }
    if let Some(ru) = repeat_until {
        acc.merge(rw_repeat_continuation(ru));
    }
    if let Some(m) = modal {
        acc.merge(rw_modal_choice(m));
    }
    // §voltstorm (CR 700.2): descend each mode as a union (see `walk_ability`).
    for m in mode_abilities {
        walk_definition(m, child_root, child_move_owner, pscope, acc);
    }
}

fn rw_modal_choice(m: &ModalChoice) -> RwProfile {
    let ModalChoice {
        dynamic_max_choices,
        chooser,
        min_choices: _,
        max_choices: _,
        mode_count: _,
        mode_descriptions: _,
        allow_repeat_modes: _,
        constraints: _,
        mode_costs: _,
        mode_pawprints: _,
        entwine_cost: _,
        selection: _,
    } = m;
    let mut p = rw_player_filter(chooser);
    if let Some(q) = dynamic_max_choices {
        p.merge(rw_quantity_expr(q));
    }
    p
}

fn rw_repeat_continuation(r: &RepeatContinuation) -> RwProfile {
    match r {
        RepeatContinuation::ControllerChoice => RwProfile::empty(),
        RepeatContinuation::UntilStopConditions {
            stop_on_put_to_hand: _,
            stop_on_duplicate_exiled_names: _,
        } => RwProfile::empty(),
        RepeatContinuation::WhileCondition {
            condition,
            max_iterations: _,
        } => rw_ability_condition(condition),
    }
}

fn rw_target_constraint(c: &TargetSelectionConstraint) -> RwProfile {
    match c {
        TargetSelectionConstraint::DifferentTargetPlayers => RwProfile::empty(),
        TargetSelectionConstraint::DifferentObjectControllers => RwProfile::empty(),
        TargetSelectionConstraint::SameZoneOwner { zone: _ } => RwProfile::empty(),
        TargetSelectionConstraint::TotalManaValue {
            value,
            comparator: _,
        } => rw_quantity_expr(value),
    }
}

fn rw_duration(x: &Duration) -> RwProfile {
    match x {
        Duration::UntilEndOfTurn
        | Duration::UntilEndOfCombat
        | Duration::UntilHostLeavesPlay
        | Duration::UntilSourceExilesAnotherCard
        | Duration::Permanent => RwProfile::empty(),
        Duration::UntilNextTurnOf { player, .. }
        | Duration::UntilEndOfNextTurnOf { player, .. }
        | Duration::UntilNextStepOf { player, .. } => rw_player_scope(player),
        Duration::ForAsLongAs { condition } => rw_static_condition(condition),
    }
}

/// CR 119.3: a player-life write plus its life-change journal (CR 119.3).
fn life_writes() -> RwProfile {
    let mut p = RwProfile::empty();
    p.writes_external.set(StateKind::PlayerLife);
    p.writes_external.set(StateKind::JournalLife);
    p
}

/// CR 120 damage: recipient-classified writes (source-actor residual documented
/// in the module doc — CR 702.15 / CR 702.2 / CR 704.5a / CR 800.4a).
fn damage_writes(target: &TargetFilter) -> RwProfile {
    let (player, object) = target_recipient(target);
    let mut p = RwProfile::empty();
    if player {
        p.merge(life_writes());
    }
    if object {
        p.writes_external.set(StateKind::SetMembership);
        p.writes_membership_external_census
            .merge(census_of_filter(target));
        // CR 120.3e + CR 704.5g: lethal damage moves a creature battlefield →
        // graveyard as an SBA ⇒ both zones (fail-closed Any).
        p.writes_membership_external_zones.merge(ZoneSpan::Any);
    }
    flag_legacy_write_target(&mut p, target);
    p
}

// ---------------------------------------------------------------------------
// Effect classification (mirrors `scan_effect`; write-kind per §2 categories).
// Returns (profile, primary object-write scope for chain-root propagation).
// ---------------------------------------------------------------------------

fn rw_effect(
    x: &Effect,
    chain_root: Option<WriteScope>,
    pscope: PlayerSpan,
    chain_move_owner: Option<PlayerSpan>,
) -> (RwProfile, Option<WriteScope>) {
    // Object write of `kind` targeting `target`, placed by scope.
    let obj = |kind: StateKind, target: &TargetFilter| -> (RwProfile, Option<WriteScope>) {
        let sc = scope_of(target, chain_root);
        let mut p = RwProfile::empty();
        place_object_write(&mut p, kind, sc);
        // CR 122.1 object-scope disjointness (§2): record the census of an EXTERNAL
        // counter write's target filter, so a source-scoped counter read only
        // conflicts when the write filter can match the source (Earthbender: a
        // `+1/+1` write on creatures can't reach an enchantment source's quest
        // counter). Self/created writes are handled by their own scoping.
        if kind == StateKind::ObjectCounters
            && matches!(sc, WriteScope::External | WriteScope::EventObject)
        {
            p.writes_external_counter_census
                .merge(census_of_filter(target));
        }
        flag_legacy_write_target(&mut p, target);
        flag_member_bound_write_target(&mut p, target);
        (p, Some(sc))
    };
    // Membership move targeting `target` with the given zone endpoints.
    let mem = |target: &TargetFilter,
               origin: Option<Zone>,
               dest: Zone|
     -> (RwProfile, Option<WriteScope>) {
        let sc = scope_of(target, chain_root);
        let mut p = RwProfile::empty();
        place_membership_write(&mut p, sc, census_of_filter(target), origin, dest);
        flag_legacy_write_target(&mut p, target);
        flag_member_bound_write_target(&mut p, target);
        (p, Some(sc))
    };
    // Deferred body (CR 603.7): descend reads, drop writes. Resolved in a future
    // scope context ⇒ pscope resets to unscoped (`None`; reads stay conservative).
    let deferred = |def: &AbilityDefinition| -> RwProfile {
        let mut p = RwProfile::empty();
        walk_definition(def, None, None, PlayerSpan::None, &mut p);
        p.drop_writes();
        p
    };

    match x {
        // ---- Damage (recipient-classified, CR 120) ----
        Effect::DealDamage {
            amount,
            target,
            damage_source: _,
            excess,
        } => {
            let mut p = damage_writes(target);
            p.merge(rw_quantity_expr(amount));
            // CR 120.4a: the excess-redirect rider deals overkill to the damaged
            // permanent's controller — a player-life write not captured by
            // object-recipient damage_writes.
            if excess.is_some() {
                p.merge(life_writes());
            }
            (p, None)
        }
        Effect::DamageAll {
            amount,
            target,
            player_filter,
            damage_source: _,
        } => {
            let mut p = RwProfile::empty();
            p.writes_external.set(StateKind::SetMembership);
            p.writes_membership_external_census
                .merge(census_of_filter(target));
            // CR 704.5g: SBA deaths move battlefield → graveyard (fail-closed Any).
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            flag_legacy_write_target(&mut p, target);
            if player_filter.is_some() {
                p.merge(life_writes());
            }
            p.merge(rw_quantity_expr(amount));
            (p, None)
        }
        Effect::DamageEachPlayer {
            amount,
            player_filter: _,
        } => {
            let mut p = life_writes();
            p.merge(rw_quantity_expr(amount));
            (p, None)
        }
        Effect::Fight { target, subject } => {
            let mut p = damage_writes(target);
            p.merge(damage_writes(subject));
            (p, None)
        }
        // CR 120.1 + CR 608.2c: each object matching `sources` (enumerated on the
        // battlefield at resolution ⇒ a board membership read) deals `amount` damage.
        // `Shared` ⇒ target damage_writes; `EachController` ⇒ each source's controller
        // takes life loss (CR 120.3a).
        Effect::EachSourceDealsDamage {
            sources,
            amount,
            recipient,
        } => {
            let mut p = board_membership_read(sources);
            p.merge(match recipient {
                crate::types::ability::EachDamageRecipient::Shared(filter) => damage_writes(filter),
                crate::types::ability::EachDamageRecipient::EachController => life_writes(),
            });
            p.merge(rw_quantity_expr(amount));
            (p, None)
        }
        // CR 122 + CR 603.10a (PR-6.75 c5): inspects the distinct counter kinds on
        // `target` (an ObjectCounters board read) and persists the pick as
        // ChosenAttribute::Counter on the SOURCE — a per-source binding a later
        // PutChosenCounter consumes (member-bound; mirrors Effect::Choose{persist}).
        // No board WRITE: the placement is the separate PutChosenCounter.
        Effect::ChooseCounterKind { target: _ } => {
            let mut p = reads_board_of(StateKind::ObjectCounters);
            p.reads_member_bound = true;
            (p, None)
        }
        // CR 122.1 + CR 122.6 + CR 603.10a (PR-6.75 c5): adds `count` counters of the
        // source's persisted ChosenAttribute::Counter kind to `target`. An
        // ObjectCounters write (like PutCounter) that CONSUMES the per-source
        // chosen-kind binding ⇒ member-bound read.
        Effect::PutChosenCounter { target, count } => {
            let (mut p, sc) = obj(StateKind::ObjectCounters, target);
            p.reads_member_bound = true;
            p.merge(rw_quantity_expr(count));
            (p, sc)
        }
        // CR 122.1 + CR 608.2d: slot-less counter adjustment reads/writes the
        // propagated parent target at resolution, then delegates through a
        // runtime-built ChooseOneOf. Until this profiler can recover that parent
        // target scope, model it conservatively.
        Effect::ChooseCounterAdjustment { count, .. } => {
            let mut p = RwProfile::conservative();
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::OpponentGuess { guesser, subject } => {
            let mut p = rw_controller_ref(guesser);
            p.merge(rw_guess_subject(subject));
            (p, None)
        }
        // CR 614.1a + CR 611.2c + CR 603.7 (PR-6.75): a floating planeswalk
        // replacement is a deferred body — descend reads, drop writes (resolves in a
        // future scope). Mirrors CreateDrawReplacement.
        Effect::CreatePlaneswalkReplacement { replacement_effect } => {
            let (mut b, _) = rw_effect(replacement_effect, None, pscope, chain_move_owner);
            b.drop_writes();
            (b, None)
        }
        // CR 311.7 + CR 901.9b: fire the active plane's chaos trigger (mirrors
        // Planeswalk / VentureIntoDungeon).
        Effect::ChaosEnsues => (ext_write(StateKind::Other), None),
        // CR 103.1: reverse turn order writes global turn-direction state.
        Effect::ReverseTurnOrder => (ext_write(StateKind::Other), None),

        // ---- Hand / library ----
        Effect::Draw { count, target: _ } => {
            let mut p = ext_write(StateKind::HandLibrary);
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::Discard {
            count,
            target,
            unless_filter: _,
            filter: _,
            selection: _,
        } => {
            let mut p = ext_write(StateKind::HandLibrary);
            // CR 401/402 (+ CR 400.3 owner-keyed hand): the discarding player's
            // hand. From the scoped-player iteration context when present (Brink:
            // each opponent ⇒ Opponents), else the effect target (`Controller` ⇒
            // You), else `Any` (fail-closed).
            p.writes_player_span = if pscope != PlayerSpan::None {
                pscope
            } else if matches!(target, TargetFilter::Controller) {
                PlayerSpan::You
            } else {
                PlayerSpan::Any
            };
            // §4.4 fused read-modify-write: `Discard{count: HandSize{ScopedPlayer},
            // target: Controller}` — "discards their hand" reads the very hand this
            // instruction empties. Its per-player fixed point composes symmetrically
            // among identical members (Canopy Gargantuan RwProfile doc), so record
            // it in `reads_player_fused` (dropped under the gate, unioned back into
            // `reads_player` when ungated ⇒ byte-identical). Otherwise the count is a
            // genuine player read.
            let fused = matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer
                    }
                }
            ) && matches!(target, TargetFilter::Controller);
            if fused {
                p.reads_player_fused.set(StateKind::HandLibrary);
            } else {
                p.merge(rw_quantity_expr(count));
            }
            (p, None)
        }
        Effect::DiscardCard {
            target: _,
            count: _,
        } => (ext_write(StateKind::HandLibrary), None),
        Effect::Scry { count, target: _ } => {
            let mut p = ext_write(StateKind::HandLibrary);
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::Surveil { count, target: _ } => {
            let mut p = ext_write(StateKind::HandLibrary);
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::Shuffle { target: _ } => (ext_write(StateKind::HandLibrary), None),
        Effect::PutAtLibraryPosition {
            target: _,
            count,
            position: _,
        } => {
            let mut p = ext_write(StateKind::HandLibrary);
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::Learn => (ext_write(StateKind::HandLibrary), None),
        Effect::DraftFromSpellbook { .. } => (ext_write(StateKind::HandLibrary), None),
        Effect::Clash => (ext_write(StateKind::HandLibrary), None),
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count,
            life_payment,
            player: _,
        } => {
            let mut p = ext_write(StateKind::HandLibrary);
            p.merge(life_writes());
            p.merge(rw_quantity_expr(count));
            p.merge(rw_quantity_expr(life_payment));
            (p, None)
        }

        // ---- Life ----
        Effect::GainLife { amount, player } => {
            let mut p = life_writes();
            flag_legacy_write_target(&mut p, player);
            p.merge(rw_quantity_expr(amount));
            (p, None)
        }
        Effect::LoseLife { amount, target } => {
            let mut p = life_writes();
            if let Some(t) = target {
                flag_legacy_write_target(&mut p, t);
            }
            p.merge(rw_quantity_expr(amount));
            (p, None)
        }
        Effect::SetLifeTotal { target, amount } => {
            let mut p = life_writes();
            flag_legacy_write_target(&mut p, target);
            p.merge(rw_quantity_expr(amount));
            (p, None)
        }
        Effect::GainEnergy { amount } => {
            let mut p = ext_write(StateKind::PlayerLife);
            p.merge(rw_quantity_expr(amount));
            (p, None)
        }
        Effect::GivePlayerCounter {
            count,
            target,
            counter_kind: _,
        } => {
            let mut p = ext_write(StateKind::PlayerLife);
            flag_legacy_write_target(&mut p, target);
            p.merge(rw_quantity_expr(count));
            (p, None)
        }

        // ---- Counters (ObjectCounters) ----
        Effect::PutCounter {
            count,
            target,
            counter_type: _,
        } => {
            let (mut p, sc) = obj(StateKind::ObjectCounters, target);
            p.merge(rw_quantity_expr(count));
            (p, sc)
        }
        Effect::PutCounterAll {
            count,
            target,
            counter_type: _,
        } => {
            let (mut p, sc) = obj(StateKind::ObjectCounters, target);
            p.merge(rw_quantity_expr(count));
            (p, sc)
        }
        Effect::RemoveCounter {
            counter_type: _,
            count,
            target,
        } => {
            let (mut p, sc) = obj(StateKind::ObjectCounters, target);
            p.merge(rw_quantity_expr(count));
            (p, sc)
        }
        Effect::MultiplyCounter {
            target,
            counter_type: _,
            multiplier: _,
        } => obj(StateKind::ObjectCounters, target),
        Effect::MoveCounters {
            source,
            count,
            target,
            counter_type: _,
            mode: _,
            selection: _,
        } => {
            let (mut p, sc) = obj(StateKind::ObjectCounters, target);
            let source_sc = scope_of(source, chain_root);
            place_object_write(&mut p, StateKind::ObjectCounters, source_sc);
            // CR 122.1 object-scope (§2): the donor is also an external counter write.
            if matches!(source_sc, WriteScope::External | WriteScope::EventObject) {
                p.writes_external_counter_census
                    .merge(census_of_filter(source));
            }
            flag_legacy_write_target(&mut p, source);
            if let Some(c) = count {
                p.merge(rw_quantity_expr(c));
            }
            (p, sc)
        }
        Effect::Bolster { count } => {
            let mut p = ext_write(StateKind::ObjectCounters);
            // Untargeted external counter write ⇒ census Any (fail-closed, §2).
            p.writes_external_counter_census.merge(Census::Any);
            p.merge(rw_quantity_expr(count));
            (p, Some(WriteScope::External))
        }
        Effect::Endure { amount, subject } => {
            let (mut p, sc) = obj(StateKind::ObjectCounters, subject);
            p.writes_external.set(StateKind::SetMembership);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.merge(rw_quantity_expr(amount));
            (p, sc)
        }
        Effect::AddPendingETBCounters {
            count,
            counter_type: _,
        } => {
            let mut p = self_write(StateKind::ObjectCounters);
            p.merge(rw_quantity_expr(count));
            (p, Some(WriteScope::SelfSource))
        }
        // CR 205.1b + CR 613.1d: the rider grants a Layer-4 type-line change to
        // the FUTURE cast object; profiled as a StateKind::SetMembership write —
        // the SAME kind Animate uses for its type change (Endure:4064). Self-
        // channeled DEFERRED ETB metadata keyed to a future self-cast (mirrors
        // AddPendingETBCounters above, WriteScope::SelfSource): it reads no
        // event-live state (reads_event_live = false), no member-bound state
        // (reads_member_bound = false), and writes no event object
        // (writes_event_object = EMPTY) — all inherited from self_write →
        // RwProfile::empty. CR 400.1: SetMembership is the membership state kind.
        // SOUNDNESS (CR 603.3b fail-open axis): AddSubtype/AddType carry NO
        // QuantityExpr, and no sibling descends modification QuantityExprs, so the
        // arm collapses to a pure SetMembership self-write. If a QuantityExpr-
        // bearing ContinuousModification is ever admitted into this rider, the
        // parser extension MUST merge each mod's QuantityExpr here via
        // rw_quantity_expr.
        Effect::AddPendingEntersModifications { modifications: _ } => {
            (self_write(StateKind::SetMembership), Some(WriteScope::SelfSource))
        }
        Effect::Proliferate => {
            let mut p = ext_write(StateKind::ObjectCounters);
            p.writes_external.set(StateKind::PlayerLife);
            // Any counter on any permanent/player with a counter ⇒ census Any.
            p.writes_external_counter_census.merge(Census::Any);
            (p, Some(WriteScope::External))
        }
        Effect::Amass { count, subtype: _ } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::ObjectCounters);
            p.writes_external_counter_census.merge(Census::Any);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.merge(rw_quantity_expr(count));
            (p, Some(WriteScope::External))
        }
        Effect::Intensify { amount, scope: _ } => {
            let mut p = ext_write(StateKind::ObjectCounters);
            p.writes_external_counter_census.merge(Census::Any);
            p.merge(rw_quantity_expr(amount));
            (p, Some(WriteScope::External))
        }
        Effect::Double {
            target,
            target_kind: _,
        } => {
            let (mut p, sc) = obj(StateKind::ObjectCounters, target);
            p.writes_external.set(StateKind::PlayerLife);
            (p, sc)
        }

        // ---- Zone moves (SetMembership) ----
        Effect::Destroy {
            target,
            cant_regenerate: _,
        } => mem(target, Some(Zone::Battlefield), Zone::Graveyard),
        Effect::DestroyAll {
            target,
            cant_regenerate: _,
        } => mem(target, Some(Zone::Battlefield), Zone::Graveyard),
        Effect::Sacrifice {
            target,
            count,
            min_count: _,
        } => {
            let (mut p, sc) = mem(target, Some(Zone::Battlefield), Zone::Graveyard);
            p.merge(rw_quantity_expr(count));
            (p, sc)
        }
        Effect::Bounce {
            target,
            destination,
            selection: _,
        } => mem(
            target,
            Some(Zone::Battlefield),
            destination.unwrap_or(Zone::Hand),
        ),
        Effect::BounceAll {
            target,
            count,
            destination,
        } => {
            let (mut p, sc) = mem(
                target,
                Some(Zone::Battlefield),
                destination.unwrap_or(Zone::Hand),
            );
            if let Some(c) = count {
                p.merge(rw_quantity_expr(c));
            }
            (p, sc)
        }
        Effect::ChangeZone {
            origin,
            destination,
            target,
            owner_library: _,
            enter_transformed: _,
            enters_under,
            enter_tapped: _,
            enters_attacking: _,
            up_to: _,
            enter_with_counters,
            conditional_enter_with_counters,
            face_down_profile: _,
            enters_modified_if: _,
        } => {
            let (mut p, sc) = mem(target, *origin, *destination);
            // §4.3.6 / §10.4 ENGINE COUPLING (CR 110.2 + CR 110.2a; the entering
            // controller default is the moved card's OWNER, `change_zone.rs:79`
            // "None keeps the default (owner's control)" — NOT the CR 110.2a
            // instructing-player default). The membership-controller span of an
            // EXTERNAL battlefield entry:
            //   * `enters_under Some(cref)` ⇒ the ControllerRef's span (CR 110.2a).
            //   * no `enters_under`, opaque forwarded set (`Any`/`ParentTarget`)
            //     from `Library` ⇒ the inherited chain move-owner fact (CR 400.3: a
            //     library holds only its owner's cards; a search of the controller's
            //     own library ⇒ owner == controller ⇒ You — Defense of the Heart).
            //   * otherwise `Any` (fail-closed). If `change_zone.rs:79`'s default
            //     ever changes, revisit this arm and `effect_move_owner`.
            if *destination == Zone::Battlefield && matches!(sc, Some(WriteScope::External)) {
                p.writes_membership_external_ctrl = match enters_under {
                    Some(cref) => player_span_of_ctrl_ref(cref),
                    None => {
                        if *origin == Some(Zone::Library) && is_opaque_forwarded_target(target) {
                            chain_move_owner.unwrap_or(PlayerSpan::Any)
                        } else {
                            PlayerSpan::Any
                        }
                    }
                };
            }
            for (_ct, q) in enter_with_counters {
                p.merge(rw_quantity_expr(q));
            }
            for (_f, _ct, q) in conditional_enter_with_counters {
                p.merge(rw_quantity_expr(q));
            }
            (p, sc)
        }
        Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            enter_with_counters,
            enters_under: _,
            enter_tapped: _,
            face_down_profile: _,
            library_position: _,
            random_order: _,
        } => {
            let (mut p, sc) = mem(target, *origin, *destination);
            for (_ct, q) in enter_with_counters {
                p.merge(rw_quantity_expr(q));
            }
            (p, sc)
        }
        Effect::Mill {
            count,
            target: _,
            destination: _,
        } => {
            let mut p = RwProfile::empty();
            place_membership_write(
                &mut p,
                WriteScope::External,
                Census::Any,
                Some(Zone::Library),
                Zone::Graveyard,
            );
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::ExileTop {
            player: _,
            count,
            face_down: _,
        } => {
            let mut p = RwProfile::empty();
            place_membership_write(
                &mut p,
                WriteScope::External,
                Census::Any,
                Some(Zone::Library),
                Zone::Exile,
            );
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::ExileFromTopUntil {
            player: _,
            until: _,
        } => {
            let mut p = RwProfile::empty();
            place_membership_write(
                &mut p,
                WriteScope::External,
                Census::Any,
                Some(Zone::Library),
                Zone::Exile,
            );
            (p, None)
        }
        Effect::Dig {
            player: _,
            count,
            filter: _,
            // CR 701.20e + CR 608.2c: dynamic keep count ("put X cards from among
            // them", Stargaze) — a `QuantityExpr` resolved against game state, so it
            // is profiled like `count`. `None` = the fixed-count path (no read).
            keep_count_expr,
            destination: _,
            keep_count: _,
            up_to: _,
            rest_destination: _,
            reveal: _,
            enter_tapped: _,
            source: _,
        } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.merge(rw_quantity_expr(count));
            if let Some(kc) = keep_count_expr {
                p.merge(rw_quantity_expr(kc));
            }
            (p, None)
        }
        Effect::Seek {
            filter: _,
            count,
            from_top: _,
            destination: _,
            enter_tapped: _,
        } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::SearchLibrary {
            source_zones: _,
            filter: _,
            count,
            reveal: _,
            target_player,
            selection_constraint: _,
            split: _,
        } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            // §4.3.5 (CR 400.3): searching the controller's OWN library (no
            // `target_player`, or a `Controller`-form one) touches only owner ==
            // controller cards ⇒ the phantom membership write's controller / hand
            // spans are You. A search of another player's library leaves both
            // unrefined (`None` ⇒ effective-Any). The chained ChangeZone entry
            // consumes the emitted `effect_move_owner` fact (§10.4 both ends).
            if target_player.is_none() || matches!(target_player, Some(TargetFilter::Controller)) {
                p.writes_membership_external_ctrl = PlayerSpan::You;
                p.writes_player_span = PlayerSpan::You;
            }
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::ChooseFromZone {
            filter: _,
            count: _,
            zone: _,
            additional_zones: _,
            zone_owner: _,
            chooser: _,
            up_to: _,
            selection: _,
            constraint: _,
        } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            (p, None)
        }
        Effect::Explore => {
            let mut p = self_write(StateKind::ObjectCounters);
            p.writes_external.set(StateKind::HandLibrary);
            (p, None)
        }
        Effect::Forage => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            (p, None)
        }
        Effect::Connive { target, count } => {
            let (mut p, sc) = obj(StateKind::ObjectCounters, target);
            p.writes_external.set(StateKind::HandLibrary);
            p.merge(rw_quantity_expr(count));
            (p, sc)
        }
        Effect::CollectEvidence { amount: _ } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            (p, None)
        }
        Effect::Discover {
            mana_value_limit,
            player: _,
        } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_external.set(StateKind::StackShape);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.merge(rw_quantity_expr(mana_value_limit));
            (p, None)
        }
        Effect::RevealUntil {
            player: _,
            filter: _,
            count,
            enters_under: _,
            matched_disposition: _,
            kept_destination: _,
            rest_destination: _,
            enter_tapped: _,
            enters_attacking: _,
            kept_optional_to: _,
        } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::Manifest {
            target: _,
            count,
            enters_under: _,
            profile: _,
        } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::ManifestDread => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            (p, None)
        }
        Effect::Cloak {
            target: _,
            count,
            object_source,
        } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.merge(rw_quantity_expr(count));
            // CR 701.58a: `object_source` (Some) names already-chosen objects being
            // cloaked — a membership WRITE target. The membership write is already
            // maximal-conservative (Census/Zone Any) above; flag its D5 / member-bound
            // referents (mirrors the `mem`/`obj` write-target closures) so a
            // TrackedSet/ExiledBySource/legacy-tag source refuses batch-T1 auto-order.
            if let Some(src) = object_source {
                flag_legacy_write_target(&mut p, src);
                flag_member_bound_write_target(&mut p, src);
            }
            (p, None)
        }
        Effect::Meld {
            source: _,
            partner: _,
            result: _,
            source_filter,
            partner_filter,
            entry: _,
        } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.merge(rw_target_filter(source_filter));
            p.merge(rw_target_filter(partner_filter));
            (p, None)
        }
        Effect::PhaseOut { target } => obj_membership_scope(target, chain_root),
        Effect::MadnessCast { cost: _ } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_external.set(StateKind::StackShape);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            (p, None)
        }

        // ---- Creation (SetMembership, fresh ids) ----
        Effect::Token {
            name: _,
            power: _,
            toughness: _,
            types,
            colors: _,
            keywords: _,
            tapped: _,
            count,
            owner: _,
            attach_to: _,
            enters_attacking: _,
            supertypes: _,
            static_abilities: _,
            enter_with_counters,
        } => {
            let mut p = RwProfile::empty();
            p.writes_external.set(StateKind::SetMembership);
            p.writes_created.set(StateKind::SetMembership);
            p.writes_membership_external_census
                .merge(census_of_types(types));
            // CR 111.1 + CR 400.1: a token is CREATED on the battlefield — it
            // touches ONLY that zone (no origin), so it cannot feed a graveyard /
            // hand / library read (Tombstone Stairwell: battlefield Zombie tokens
            // vs a graveyard-creature count are zone-disjoint).
            p.writes_membership_external_zones
                .merge(ZoneSpan::one(Zone::Battlefield));
            for (_ct, q) in enter_with_counters {
                p.merge(rw_quantity_expr(q));
            }
            p.merge(rw_quantity_expr(count));
            (p, Some(WriteScope::Created))
        }
        Effect::CopyTokenOf {
            target,
            owner: _,
            source_filter,
            enters_attacking: _,
            tapped: _,
            count,
            extra_keywords: _,
            additional_modifications: _,
        } => {
            let mut p = RwProfile::empty();
            p.writes_external.set(StateKind::SetMembership);
            p.writes_created.set(StateKind::SetMembership);
            // CR 707.2: the created token acquires the copy SOURCE's copiable
            // values (card type / subtypes), so its membership census is the
            // source's typeline — NOT `Census::Any`.
            if matches!(target, TargetFilter::SelfRef) {
                // A SelfRef copy's census is the group's live source census,
                // resolved at `profiles_conflict` time — reproduce the SelfRef
                // membership-move representation (`writes_membership_self`), which
                // `membership_census_of` resolves against `source_census`. This
                // is what clears Scute Swarm (creature token ≠ its Lands read,
                // census-disjoint — §1.3.1-F) instead of over-conflicting on Any.
                p.writes_membership_self = true;
                // CR 111.1: a token copy is CREATED on the battlefield — its self
                // membership write touches only that zone, NOT the blanket `Any` a
                // self MOVE gets. So it is zone-disjoint from a graveyard/departure
                // read (Compy Swarm / Phoenix Fleet Airship's "a creature died /
                // you sacrificed a permanent this turn" ×2).
                p.writes_membership_self_zones
                    .merge(ZoneSpan::one(Zone::Battlefield));
                // Copiable-values read (fail-closed source dependence).
                p.reads_src.set(StateKind::ObjectPt);
            } else {
                // Non-SelfRef: census from the copy-source filter where
                // extractable (`source_filter` for the "for each" variant, else
                // the targeted `target`), else `Census::Any` (fail-closed).
                let copy_source = source_filter.as_ref().unwrap_or(target);
                p.writes_membership_external_census
                    .merge(census_of_filter(copy_source));
                // CR 111.1: the copy is created on the battlefield.
                p.writes_membership_external_zones
                    .merge(ZoneSpan::one(Zone::Battlefield));
            }
            // D5: an event-context write target (e.g. `CopyTokenOf{TriggeringSource}`)
            // retains the batch prompt (CR 603.10a).
            flag_legacy_write_target(&mut p, target);
            p.merge(rw_quantity_expr(count));
            (p, Some(WriteScope::Created))
        }
        Effect::Conjure {
            cards: _,
            destination,
            tapped: _,
            library_position: _,
            library_players: _,
        } => {
            let mut p = RwProfile::empty();
            p.writes_external.set(StateKind::SetMembership);
            p.writes_created.set(StateKind::SetMembership);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            if is_hand_or_library(*destination) {
                p.writes_external.set(StateKind::HandLibrary);
            }
            (p, Some(WriteScope::Created))
        }
        Effect::Incubate { count } => {
            let mut p = RwProfile::empty();
            p.writes_external.set(StateKind::SetMembership);
            p.writes_created.set(StateKind::SetMembership);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            p.merge(rw_quantity_expr(count));
            (p, Some(WriteScope::Created))
        }
        Effect::Populate => {
            let mut p = RwProfile::empty();
            p.writes_external.set(StateKind::SetMembership);
            p.writes_created.set(StateKind::SetMembership);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            (p, Some(WriteScope::Created))
        }
        Effect::Investigate => {
            let mut p = RwProfile::empty();
            p.writes_external.set(StateKind::SetMembership);
            p.writes_created.set(StateKind::SetMembership);
            p.writes_membership_external_census
                .merge(Census::Tags(BTreeSet::from([
                    "clue".into(),
                    "artifact".into(),
                ])));
            // CR 111.1: the Clue token is created on the battlefield.
            p.writes_membership_external_zones
                .merge(ZoneSpan::one(Zone::Battlefield));
            (p, Some(WriteScope::Created))
        }

        // ---- P/T & type (ObjectPt) ----
        Effect::Pump {
            power: _,
            toughness: _,
            target,
        } => obj(StateKind::ObjectPt, target),
        Effect::PumpAll {
            power: _,
            toughness: _,
            target,
        } => obj(StateKind::ObjectPt, target),
        Effect::DoublePT {
            target,
            mode: _,
            factor: _,
        } => obj(StateKind::ObjectPt, target),
        Effect::DoublePTAll {
            target,
            mode: _,
            factor: _,
        } => obj(StateKind::ObjectPt, target),
        Effect::SwitchPT { target } => obj(StateKind::ObjectPt, target),
        Effect::Transform { target } => obj(StateKind::ObjectPt, target),
        Effect::BecomeCopy {
            target,
            recipient,
            duration: _,
            mana_value_limit: _,
            additional_modifications: _,
        } => {
            // CR 707.2 + CR 611.2c: the RECIPIENT (copier) is mutated (ObjectPt +
            // SetMembership); the donor `target` is only read for its copiable
            // values. The single-subject case (`recipient == SelfRef`, every
            // existing copy card) keeps its EXACT prior profile — the write scoped
            // by the announced `target` slot — so the ordering-parity classification
            // of the existing copy corpus is byte-identical (no scope re-derivation
            // is smuggled into this Niko-only change). Only the NEW mass-recipient
            // path (Niko: "Shards you control") writes the recipient set and reads
            // the donor, since there the copier and the copied donor are distinct.
            let write_scope_filter = match recipient {
                TargetFilter::SelfRef => target,
                _ => recipient,
            };
            let (mut p, sc) = obj(StateKind::ObjectPt, write_scope_filter);
            place_object_write(
                &mut p,
                StateKind::SetMembership,
                scope_of(write_scope_filter, chain_root),
            );
            if !matches!(recipient, TargetFilter::SelfRef) {
                p.merge(board_value_aggregate_read(target, StateKind::ObjectPt));
            }
            (p, sc)
        }
        Effect::Animate {
            power: _,
            toughness: _,
            types: _,
            remove_types: _,
            target,
            keywords: _,
        } => {
            let (mut p, sc) = obj(StateKind::ObjectPt, target);
            place_object_write(
                &mut p,
                StateKind::SetMembership,
                scope_of(target, chain_root),
            );
            (p, sc)
        }
        Effect::TurnFaceUp { target } => {
            let (mut p, sc) = obj(StateKind::ObjectPt, target);
            place_object_write(
                &mut p,
                StateKind::SetMembership,
                scope_of(target, chain_root),
            );
            (p, sc)
        }
        Effect::TurnFaceDown { target, profile: _ } => obj(StateKind::ObjectPt, target),
        Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } => {
            // CR 611.2c: the player-chosen `target` slot names the affected object
            // when present; otherwise transient static grants (Stonehoof #5335)
            // carry `affected: TriggeringSource` on the nested static.
            let target_filters: Vec<_> = target.clone().map_or_else(
                || {
                    let nested: Vec<_> = static_abilities
                        .iter()
                        .filter_map(|s| s.affected.clone())
                        .collect();
                    if nested.is_empty() {
                        vec![TargetFilter::SelfRef]
                    } else {
                        nested
                    }
                },
                |tf| vec![tf],
            );
            let mut p = RwProfile::empty();
            let mut sc = None;
            for tf in target_filters {
                let (target_profile, target_scope) = obj(StateKind::ObjectPt, &tf);
                p.merge(target_profile);
                place_object_write(&mut p, StateKind::SetMembership, scope_of(&tf, chain_root));
                sc = match (sc, target_scope) {
                    (None, next) => next,
                    (current, None) => current,
                    (Some(current), Some(next)) if current == next => Some(current),
                    (Some(_), Some(_)) => Some(WriteScope::External),
                };
            }
            if let Some(d) = duration {
                p.merge(rw_duration(d));
            }
            (p, sc)
        }

        // ---- Control (SetMembership external) ----
        Effect::GainControl { target: _ }
        | Effect::GainControlAll { target: _ }
        | Effect::GiveControl {
            target: _,
            recipient: _,
        }
        | Effect::ExchangeControl {
            target_a: _,
            target_b: _,
        } => {
            let mut p = ext_write(StateKind::SetMembership);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            (p, Some(WriteScope::External))
        }

        // ---- Stack (StackShape) ----
        Effect::CopySpell {
            target,
            retarget: _,
            copier: _,
            additional_modifications: _,
            starting_loyalty_from_casualty_sacrifice: _,
        } => {
            // CR 707.10/707.10c: SelfRef / explicit-target copies read the
            // original by id (clean); the untargeted top-of-stack fallback reads
            // the mutable stack top (D4). §L9: `TriggeringSource` (the spell that
            // caused the trigger) and a chain-referenced `ParentTarget` also
            // address the original BY ID, not the mutable stack shape — independent
            // copies commute (Pyromancer Ascension / Ominous Lockbox / Curse of
            // Echoes / Locus of Enlightenment).
            let mut p = ext_write(StateKind::StackShape);
            if !matches!(
                target,
                TargetFilter::SelfRef
                    | TargetFilter::StackSpell
                    | TargetFilter::SpecificObject { .. }
                    | TargetFilter::TriggeringSource
                    | TargetFilter::ParentTarget
            ) {
                p.reads_board.set(StateKind::StackShape);
            }
            (p, None)
        }
        Effect::Counter {
            target: _,
            source_rider: _,
            countered_spell_zone: _,
        } => (ext_write(StateKind::StackShape), None),
        // §L9 (CR 115.7): changing the target(s) of a spell/ability on the stack is
        // a StackShape write, NOT the maximal-conservative fallback (which set a
        // board `ObjectPt`/… read that falsely conflicted with the sibling
        // CopySpell). Each per-player copy retargets its OWN copy independently, so
        // identical siblings commute (Curse of Echoes). Fail-closed batch parity
        // via the D5 write-target flag.
        Effect::ChangeTargets {
            target,
            scope: _,
            forced_to,
        } => {
            let mut p = ext_write(StateKind::StackShape);
            flag_legacy_write_target(&mut p, target);
            if let Some(ft) = forced_to {
                flag_legacy_write_target(&mut p, ft);
            }
            (p, None)
        }
        Effect::CastCopyOfCard {
            target: _,
            count,
            cost: _,
        } => {
            let mut p = ext_write(StateKind::StackShape);
            p.writes_external.set(StateKind::SetMembership);
            p.writes_external.set(StateKind::HandLibrary);
            p.writes_membership_external_census.merge(Census::Any);
            p.writes_membership_external_zones.merge(ZoneSpan::Any);
            if let Some(c) = count {
                p.merge(rw_quantity_expr(c));
            }
            (p, None)
        }
        Effect::CastFromZone {
            target: _,
            without_paying_mana_cost: _,
            mode: _,
            cast_transformed: _,
            alt_ability_cost: _,
            constraint: _,
            duration: _,
            driver: _,
            mana_spend_permission: _,
        } => {
            let mut p = ext_write(StateKind::HandLibrary);
            p.writes_external.set(StateKind::StackShape);
            (p, None)
        }
        Effect::ExileResolvingSpellInsteadOfGraveyard { on_exile: _ } => {
            (ext_write(StateKind::StackShape), None)
        }

        // ---- Pool ----
        Effect::Mana {
            produced: _,
            restrictions: _,
            grants: _,
            expiry: _,
            target: _,
        } => (writes_pool_profile(), None),

        // ---- Tap ----
        Effect::SetTapState {
            target,
            scope: _,
            state: _,
        } => obj(StateKind::TapState, target),

        // ---- Deferred bodies (CR 603.7): reads descended, writes NOT counted ----
        Effect::CreateDelayedTrigger {
            condition: _,
            effect,
            uses_tracked_set: _,
        } => (deferred(effect), None),
        Effect::CreateDrawReplacement { replacement_effect } => {
            let (mut b, _) = rw_effect(replacement_effect, None, pscope, chain_move_owner);
            b.drop_writes();
            (b, None)
        }
        Effect::PreventDamage {
            amount_dynamic,
            target: _,
            damage_source_filter: _,
            prevention_duration: _,
            amount: _,
            scope: _,
        } => {
            let mut p = RwProfile::empty();
            if let Some(q) = amount_dynamic {
                p.merge(rw_quantity_expr(q));
            }
            (p, None)
        }
        Effect::PayCost {
            cost: _,
            scale,
            payer: _,
        } => {
            let mut p = RwProfile::empty();
            p.has_pay_or_unless = true;
            if let Some(q) = scale {
                p.merge(rw_quantity_expr(q));
            }
            (p, None)
        }
        Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::CreateEmblem { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::Regenerate { .. }
        | Effect::GrantCastingPermission { .. } => (RwProfile::empty(), None),

        // ---- Choice wrappers (union descent, §2) ----
        Effect::ChooseOneOf { chooser, branches } => {
            let mut p = rw_player_filter(chooser);
            for b in branches {
                walk_definition(b, chain_root, chain_move_owner, pscope, &mut p);
            }
            (p, None)
        }
        Effect::Vote {
            choices: _,
            per_choice_effect,
            starting_with,
            voter_scope: _,
            tally_mode: _,
            // CR 701.38a/b: visibility is reveal-timing only (inert); subject's
            // Objects case is an unmodeled residual of this incomplete arm (which
            // also drops outcome_template) — not introduced by the rebase.
            subject: _,
            visibility: _,
        } => {
            let mut p = rw_controller_ref(starting_with);
            for b in per_choice_effect {
                walk_definition(b, chain_root, chain_move_owner, pscope, &mut p);
            }
            (p, None)
        }

        // ---- RNG (no read-kind; descend sub-effects, §2/D4) ----
        Effect::RollDie {
            count,
            sides: _,
            results,
            modifier: _,
        } => {
            let mut p = rw_quantity_expr(count);
            for r in results {
                walk_definition(&r.effect, chain_root, chain_move_owner, pscope, &mut p);
            }
            (p, None)
        }
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            flipper: _,
        } => {
            let mut p = RwProfile::empty();
            if let Some(w) = win_effect {
                walk_definition(w, chain_root, chain_move_owner, pscope, &mut p);
            }
            if let Some(l) = lose_effect {
                walk_definition(l, chain_root, chain_move_owner, pscope, &mut p);
            }
            (p, None)
        }
        Effect::FlipCoins {
            count,
            win_effect,
            lose_effect,
            flipper: _,
        } => {
            let mut p = rw_quantity_expr(count);
            if let Some(w) = win_effect {
                walk_definition(w, chain_root, chain_move_owner, pscope, &mut p);
            }
            if let Some(l) = lose_effect {
                walk_definition(l, chain_root, chain_move_owner, pscope, &mut p);
            }
            (p, None)
        }
        Effect::FlipCoinUntilLose { win_effect } => {
            let mut p = RwProfile::empty();
            walk_definition(win_effect, chain_root, chain_move_owner, pscope, &mut p);
            (p, None)
        }

        // ---- Status / designation tail (fail-closed writes_external{Other}).
        // M3: all fields bound (no `..` on a non-conservative RHS) so a future
        // read/write-bearing field forces reclassification. Count/min/max leaves
        // here are fixed numbers (ability_scan does not descend them). ----
        // CR 702.95 soulbond / CR 701 attach: an attachment/pairing DESIGNATION.
        // It mutates only pairing/attachment state, which EVERY reader consults
        // through a FROZEN source condition (SourceIsPaired / SourceAttachedTo-
        // Creature / SourceIsEquipped ⇒ `frozen_source_read`, never fed), so no
        // LIVE read observes it ⇒ NO observable RW kind. The plan's status-tail
        // `Other` default is parity-safe only where no co-occurring read exists,
        // but Deadeye Navigator's soulbond trigger reads `SourceMatchesFilter`
        // ObjectPt alongside the `PairWith` write, so `Other` (which conflicts with
        // any read) falsely prompts. Order-independence proof: two identical
        // soulbond triggers off ONE creature-enters event each pair their own
        // source with the event object; pairing is a symmetric designation and the
        // ObjectPt read sees only the frozen pre-write source ⇒ identical board in
        // either order (no feed — attachment/pairing state is read only frozen).
        // The write target still flags D5 batch parity (CR 603.10a).
        Effect::PairWith { target }
        | Effect::Attach {
            attachment: _,
            target,
        } => {
            let mut p = RwProfile::empty();
            flag_legacy_write_target(&mut p, target);
            (p, None)
        }
        // Target-bearing status effects: `target` is a write recipient, so a D5
        // event-context ref there retains the batch prompt (CR 603.10a).
        Effect::Goad { target }
        | Effect::GoadAll { target }
        | Effect::BecomePrepared { target }
        | Effect::ApplyPerpetual {
            target,
            modification: _,
        } => {
            let mut p = ext_write(StateKind::Other);
            flag_legacy_write_target(&mut p, target);
            (p, None)
        }
        // §L12 (CR 701.60 suspect / CR 701.60a unsuspect): suspected is an
        // IDEMPOTENT designation — suspecting an already-suspected creature is a
        // no-op, and identical siblings suspect/unsuspect the SAME (or self)
        // targets, so the write is order-invariant. Like `PairWith`/`Attach`, no
        // LIVE read observes it (the status is read only via source-referential
        // `SourceMatchesFilter{Suspected}`), so NO observable RW kind — never the
        // `Other` catch-all, which falsely conflicted with Frantic Scapegoat's
        // "if ~ is suspected" source read. The write target still flags D5 batch
        // parity (CR 603.10a).
        Effect::Suspect { target, scope: _ } | Effect::Unsuspect { target, scope: _ } => {
            let mut p = RwProfile::empty();
            flag_legacy_write_target(&mut p, target);
            (p, None)
        }
        // §L14 (CR 500 + CR 505.6): extra turns/phases are game-SEQUENCING writes
        // (two extra turns commute) — a dedicated turn-structure kind that no
        // profiled read observes, NOT the `Other` catch-all (which falsely
        // conflicted with a co-occurring source counter/life read on Lighthouse
        // Chronologist / Second Chance / Regenerations Restored / Time Bends).
        Effect::ExtraTurn { target } => {
            let mut p = ext_write(StateKind::TurnStructure);
            flag_legacy_write_target(&mut p, target);
            (p, None)
        }
        Effect::OpenAttractions { count: _ }
        | Effect::RegisterBending { kind: _ }
        | Effect::BlightEffect {
            player: _,
            count: _,
        }
        | Effect::ChooseObjectsIntoTrackedSet {
            chooser: _,
            filter: _,
            min: _,
            max: _,
        }
        | Effect::BecomeMonarch
        | Effect::RingTemptsYou
        | Effect::TimeTravel
        | Effect::Planeswalk
        | Effect::VentureIntoDungeon
        | Effect::SolveCase => (ext_write(StateKind::Other), None),
        Effect::ForceAttack {
            target,
            required_player: _,
            duration,
        } => {
            let mut p = ext_write(StateKind::Other);
            flag_legacy_write_target(&mut p, target);
            p.merge(rw_duration(duration));
            (p, None)
        }
        // §L14 (CR 500.8): an additional phase/step is a turn-structure write.
        Effect::AdditionalPhase {
            target: _,
            count,
            phase: _,
            after: _,
            followed_by: _,
            attacker_restriction,
        } => {
            let mut p = ext_write(StateKind::TurnStructure);
            p.merge(rw_quantity_expr(count));
            // CR 608.2h + 611.2c: Some(filter) snapshots the eligible-attacker set
            // at resolution (a board membership read) before installing the
            // restriction on the added combat.
            if let Some(filter) = attacker_restriction {
                p.merge(board_membership_read(filter));
            }
            (p, None)
        }
        // §L13 (CR 614.10) + §L14: skipping a step/turn is a turn-structure write.
        Effect::SkipNextStep {
            target: _,
            step: _,
            count,
            scope: _,
        }
        | Effect::SkipNextTurn { target: _, count } => {
            let mut p = ext_write(StateKind::TurnStructure);
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        // §L13 (CR 506.4c): removing a creature from combat is idempotent (a
        // no-op on an already-removed creature) and observed by no profiled read —
        // a turn/combat-structure write, NOT the maximal-conservative fallback
        // that falsely conflicted (Gustcloak Savior / Lost in the Woods / Time
        // Bends to My Will).
        Effect::RemoveFromCombat { target } => {
            let mut p = ext_write(StateKind::TurnStructure);
            flag_legacy_write_target(&mut p, target);
            (p, None)
        }
        // CR 509.1h: making an attacking creature become blocked writes the
        // combat `blocked` flag — a turn/combat-structure write, idempotent on a
        // non-attacker/already-blocked target (mirrors RemoveFromCombat above,
        // NOT the maximal-conservative fallback).
        Effect::BecomeBlocked { target } => {
            let mut p = ext_write(StateKind::TurnStructure);
            flag_legacy_write_target(&mut p, target);
            (p, None)
        }
        Effect::AssembleContraptions { count } => {
            let mut p = ext_write(StateKind::Other);
            p.merge(rw_quantity_expr(count));
            (p, None)
        }
        Effect::PutSticker {
            target: _,
            count,
            max_ticket_cost,
            kind: _,
            ticket_cost_payment: _,
        } => {
            let mut p = ext_write(StateKind::Other);
            p.merge(rw_quantity_expr(count));
            if let Some(q) = max_ticket_cost {
                p.merge(rw_quantity_expr(q));
            }
            (p, None)
        }

        // ---- No observable WRITE kind (info / terminal / plumbing). M3: bind
        // all fields. `RevealHand.count` is a dynamic `Option<QuantityExpr>` read
        // (ability_scan descends it) ⇒ surfaced below; all other leaves here are
        // concrete/announced values. ----
        Effect::RevealHand {
            count,
            target: _,
            card_filter: _,
            selection: _,
            choice_optional: _,
            reveal: _,
        } => {
            let mut p = RwProfile::empty();
            if let Some(q) = count {
                p.merge(rw_quantity_expr(q));
            }
            (p, None)
        }
        Effect::ApplyPostReplacementDamage {
            context: _,
            target: _,
            amount: _,
            is_combat: _,
        }
        | Effect::Cleanup {
            clear_remembered: _,
            clear_chosen_player: _,
            clear_chosen_color: _,
            clear_chosen_type: _,
            clear_chosen_card: _,
            clear_imprinted: _,
            clear_triggers: _,
            clear_coin_flips: _,
        }
        | Effect::RuntimeHandled { handler: _ }
        // CR 701.4a + CR 608.2d: reveal-or-choose is a pure information event — no
        // observable write and no sibling-mutable read (mirrors `Reveal`); the
        // resolution-time choice is classified in ability_scan.rs (MayPrompt,
        // `WaitingFor::BeholdChoice`), not the read/write ordering profiler.
        | Effect::Behold { filter: _ }
        | Effect::Reveal { target: _ }
        | Effect::RevealTop {
            player: _,
            count: _,
        }
        | Effect::TargetOnly { target: _ }
        | Effect::LoseTheGame { target: _ }
        | Effect::WinTheGame { target: _ }
        | Effect::RemoveAllDamage { target: _ }
        | Effect::Unimplemented {
            name: _,
            description: _,
        }
        | Effect::NoOp
        | Effect::EndTheTurn
        | Effect::EndCombatPhase => (RwProfile::empty(), None),

        // CR 603.10a (PR-6.75 c5): a PERSISTED choice writes per-source storage
        // (`chosen_attributes`) that a later resolution reads ⇒ member-bound; a
        // resolution-local choice (`persist: false`) leaves no cross-member binding
        // (slithermuse's `Choose{Opponent, persist:false}`).
        Effect::Choose { persist, .. } => {
            let mut p = RwProfile::empty();
            if *persist {
                p.reads_member_bound = true;
            }
            (p, None)
        }
        Effect::SwapChosenLabels {
            first: _,
            second: _,
        } => (ext_write(StateKind::Other), None),

        // ---- Histogram-absent ⇒ fail-closed conservative ----
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::CounterAll { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealFromHand { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::SetClassLevel { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        // CR 101.4 + CR 707.2 + CR 122.1: this APNAP walk creates token copies
        // and may add counters from a live property read. Fail closed until the
        // copy/counter sub-steps have a precise profile.
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::EpicCopy { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::UnattachAll { .. }
        | Effect::ExploreAll { .. }
        | Effect::Tribute { .. }
        | Effect::ProliferateTarget { .. }
        | Effect::Exploit { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        // CR 119.7 + CR 119.8: writes multiple players' life totals via an interactive
        // permutation — conservative alongside the life-total sibling.
        | Effect::RedistributeLifeTotals
        | Effect::SetDayNight { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Adapt { .. }
        | Effect::Harness
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::RememberCard { .. }
        | Effect::ForEachCategory { .. }
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters => (RwProfile::conservative(), None),
    }
}

/// A membership write scoped by target (used where the scope matters for chain
/// propagation, e.g. `PhaseOut`).
fn obj_membership_scope(
    target: &TargetFilter,
    chain_root: Option<WriteScope>,
) -> (RwProfile, Option<WriteScope>) {
    let sc = scope_of(target, chain_root);
    let mut p = RwProfile::empty();
    place_membership_write(
        &mut p,
        sc,
        census_of_filter(target),
        Some(Zone::Battlefield),
        Zone::Exile,
    );
    flag_legacy_write_target(&mut p, target);
    (p, Some(sc))
}

/// CR 205: extract a census from a token's type strings.
fn census_of_types(types: &[String]) -> Census {
    if types.is_empty() {
        return Census::Any;
    }
    Census::Tags(types.iter().map(|t| t.to_lowercase()).collect())
}

// ---------------------------------------------------------------------------
// Quantity reads (mirror `scan_quantity_*`).
// ---------------------------------------------------------------------------

fn rw_quantity_expr(x: &QuantityExpr) -> RwProfile {
    match x {
        QuantityExpr::Ref { qty } => rw_quantity_ref(qty),
        QuantityExpr::Fixed { value: _ } => RwProfile::empty(),
        QuantityExpr::DivideRounded {
            inner,
            divisor: _,
            rounding: _,
        }
        | QuantityExpr::Offset { inner, offset: _ }
        | QuantityExpr::ClampMin { inner, minimum: _ }
        | QuantityExpr::Multiply { inner, factor: _ }
        | QuantityExpr::UpTo { max: inner } => rw_quantity_expr(inner),
        QuantityExpr::Power { exponent, base: _ } => rw_quantity_expr(exponent),
        QuantityExpr::Difference { left, right } => {
            let mut p = rw_quantity_expr(left);
            p.merge(rw_quantity_expr(right));
            p
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            let mut p = RwProfile::empty();
            for e in exprs {
                p.merge(rw_quantity_expr(e));
            }
            p
        }
    }
}

fn rw_guess_subject(subject: &GuessSubject) -> RwProfile {
    match subject {
        GuessSubject::CommittedChoice { choice_type } => {
            let mut p = rw_choice_type(choice_type);
            // CR 608.2d + CR 603.10a: a committed-value guess consumes the source's
            // persisted choice, so same-event sibling instances are member-bound.
            p.reads_member_bound = true;
            p
        }
        GuessSubject::Proposition {
            lhs,
            comparator: _,
            rhs,
        } => {
            let mut p = rw_quantity_expr(lhs);
            p.merge(rw_quantity_expr(rhs));
            p
        }
    }
}

fn rw_choice_type(choice_type: &crate::types::ability::ChoiceType) -> RwProfile {
    match choice_type {
        crate::types::ability::ChoiceType::Opponent { restriction } => match restriction {
            Some(filter) => rw_player_filter(filter),
            None => RwProfile::empty(),
        },
        crate::types::ability::ChoiceType::CreatureType { .. }
        | crate::types::ability::ChoiceType::Color { .. }
        | crate::types::ability::ChoiceType::OddOrEven
        | crate::types::ability::ChoiceType::BasicLandType
        | crate::types::ability::ChoiceType::CardType { .. }
        | crate::types::ability::ChoiceType::CardName
        | crate::types::ability::ChoiceType::NumberRange { .. }
        | crate::types::ability::ChoiceType::Labeled { .. }
        | crate::types::ability::ChoiceType::LandType
        | crate::types::ability::ChoiceType::CardPredicate { .. }
        | crate::types::ability::ChoiceType::CardPredicateGuess { .. }
        | crate::types::ability::ChoiceType::Player
        | crate::types::ability::ChoiceType::TwoColors
        | crate::types::ability::ChoiceType::Word
        | crate::types::ability::ChoiceType::Artist
        | crate::types::ability::ChoiceType::Keyword { .. }
        | crate::types::ability::ChoiceType::CounterKind { .. } => RwProfile::empty(),
    }
}

fn rw_quantity_ref(x: &QuantityRef) -> RwProfile {
    match x {
        // §4.3.1 (CR 401/402): a hand-size read, refined by its player axis
        // (Rekindled `Opponent` ⇒ Opponents, Brink cond `Controller` ⇒ You).
        QuantityRef::HandSize { player } => {
            let mut p = reads_player_of(StateKind::HandLibrary);
            p.reads_player_span = player_span_of_scope(player);
            p
        }
        QuantityRef::LifeTotal { player: _ } | QuantityRef::LifeAboveStarting => {
            reads_player_of(StateKind::PlayerLife)
        }
        QuantityRef::StartingLifeTotal => RwProfile::empty(),
        // CR 701.57a: reads the transient last-discover scalar, written only by
        // discover resolution (never by a sibling trigger) — no ordering-relevant
        // read/write, mirroring StartingLifeTotal.
        QuantityRef::TriggeringDiscoverValue => RwProfile::empty(),
        QuantityRef::GraveyardSize { .. } => reads_zone_membership(),
        QuantityRef::ObjectCount { filter }
        | QuantityRef::ObjectCountDistinct { filter, .. }
        | QuantityRef::ObjectCountBySharedQuality { filter, .. }
        | QuantityRef::ControlledByEachPlayer { filter, .. }
        | QuantityRef::DistinctColorsAmongPermanents { filter } => board_membership_read(filter),
        QuantityRef::CountersOnObjects {
            filter,
            counter_type: _,
        }
        | QuantityRef::DistinctCounterKindsAmong { filter } => {
            board_value_aggregate_read(filter, StateKind::ObjectCounters)
        }
        QuantityRef::Aggregate {
            filter,
            function: _,
            property: _,
        } => board_value_aggregate_read(filter, StateKind::ObjectPt),
        QuantityRef::PlayerCount { filter: _ } => RwProfile::empty(),
        QuantityRef::CountersOn { scope, .. } | QuantityRef::Intensity { scope, .. } => {
            read_object_scope(scope, StateKind::ObjectCounters)
        }
        QuantityRef::Power { scope, .. }
        | QuantityRef::Toughness { scope, .. }
        | QuantityRef::ObjectManaValue { scope, .. }
        | QuantityRef::ObjectColorCount { scope, .. }
        | QuantityRef::ObjectNameWordCount { scope, .. }
        | QuantityRef::ObjectTypelineComponentCount { scope, .. }
        | QuantityRef::ManaSymbolsInManaCost { scope, .. } => {
            read_object_scope(scope, StateKind::ObjectPt)
        }
        QuantityRef::TargetObjectManaValue { filter: _ } => reads_board_of(StateKind::ObjectPt),
        QuantityRef::PlayerCounter { scope: _, kind: _ } => reads_player_of(StateKind::PlayerLife),
        // CR 122.1f: the target's controller's player-counter total (poison ==
        // "poisoned"). A target-relative player-mutable read — same proxy as the
        // `PlayerCounter` sibling above (StateKind has no dedicated player-counter
        // kind; player counters ride the `PlayerLife` sibling-mutable player-state
        // row, which `Effect::GivePlayerCounter` writes for the matching feed).
        QuantityRef::TargetControllerCounter { kind: _ } => reads_player_of(StateKind::PlayerLife),
        QuantityRef::Variable { name: _ } | QuantityRef::SelfManaValue => RwProfile::empty(),
        QuantityRef::TargetZoneCardCount { zone: _ } => reads_zone_membership(),
        QuantityRef::Devotion { .. }
        | QuantityRef::DistinctCardTypes { .. }
        | QuantityRef::DistinctSubtypes { .. }
        | QuantityRef::BasicLandTypeCount { .. }
        | QuantityRef::PartySize { .. } => reads_zone_membership(),
        // CR 603.10a (PR-6.75 c5): promoted out of the fail-closed group below to
        // its own arm so the next field addition is compiler-visible (a read-bearing
        // field must force a re-audit). `reads_member_bound = true` is the HONEST
        // classification for BOTH sources, not conservative-by-reflex:
        //   - `ChainSet` is per-instance chain-published storage (same as the group
        //     below), so it is member-bound.
        //   - `TriggeringBatch` reads `state.current_trigger_events`, which
        //     `matching_batched_trigger_events` (triggers.rs) scopes by THIS
        //     trigger's own filter — so distinct-filter same-event sibling triggers
        //     read DISTINCT batches ⇒ "NOT one shared `f`" = member-bound by the
        //     definition at `reads_member_bound`'s doc. The `EventContextAmount`
        //     frozen-context mirror (`legacy_batch_prompt`) is UNREACHABLE here:
        //     `contains_legacy_event_ref` overwrites `legacy_batch_prompt` to false
        //     for any non-legacy-12 ref (ability_rw.rs `ability_rw_profile`),
        //     collapsing that mirror to a fail-open read-live path. So fail-closed
        //     member-bound is the correct, not merely safe, verdict.
        QuantityRef::TrackedSetAggregate {
            function: _,
            property: _,
            source: _,
        } => {
            let mut p = RwProfile::empty();
            p.reads_member_bound = true;
            p
        }
        // CR 603.10a (PR-6.75 c5): per-source tracked/exiled/chosen storage and
        // per-instance cast-context memory (X paid, kicker/convoke/vote/additional-
        // cost counts) are bound per member instance — each trigger stack object
        // carries its own stored value ⇒ refuse batch-T1 (fail-closed).
        QuantityRef::CardsExiledBySource
        | QuantityRef::ExiledCardPower { .. }
        | QuantityRef::TrackedSetSize
        | QuantityRef::FilteredTrackedSetSize { .. }
        | QuantityRef::ChosenNumber
        | QuantityRef::CostXPaid
        | QuantityRef::KickerCount
        | QuantityRef::AdditionalCostPaymentCount
        | QuantityRef::AdditionalCostPaymentCountFor { .. }
        | QuantityRef::ConvokedCreatureCount
        | QuantityRef::VoteCount { .. } => {
            let mut p = RwProfile::empty();
            p.reads_member_bound = true;
            p
        }
        // Resolution-local / turn- / commander-scoped: no per-source binding
        // (member-invariant under uniformity).
        QuantityRef::ExiledFromHandThisResolution
        | QuantityRef::PreviousEffectAmount { .. }
        | QuantityRef::TurnsTaken
        | QuantityRef::CrimesCommittedThisTurn
        | QuantityRef::AttackedThisTurn { .. }
        | QuantityRef::DescendedThisTurn
        // CR 701.65b/701.66b/701.67c: controller-scoped per-turn accumulator; no
        // per-source binding, member-invariant under uniformity (Avatar Aang).
        | QuantityRef::BendTypesThisTurn
        | QuantityRef::LandsPlayedThisTurn { .. }
        | QuantityRef::DungeonsCompleted
        | QuantityRef::ColorsInCommandersColorIdentity
        | QuantityRef::CommanderCastFromCommandZoneCount
        | QuantityRef::CommanderManaValue { .. }
        | QuantityRef::Speed { .. } => RwProfile::empty(),
        QuantityRef::ZoneCardCount {
            zone,
            card_types,
            filter,
            scope: _,
        } => match filter {
            Some(f) => board_membership_read(f),
            // §L1 (CR 604.3 + CR 205 + CR 400.1): extract the census from
            // `card_types` and the read zone from the `ZoneRef`, instead of
            // discarding both to `Any`/`Any`. "creature cards in your graveyard"
            // observes only the GRAVEYARD for a CREATURE census — zone-disjoint
            // from a battlefield token write (Hallowed Spiritkeeper) and
            // census-disjoint from creature writes when counting lands (Cavalier
            // of Flame / Scouring Swarm). `scope` (controller/owner) is not
            // carried into the membership feed (fail-closed on controller).
            None => {
                let mut p = reads_board_of(StateKind::SetMembership);
                p.reads_membership_census = census_of_zone_card_types(card_types);
                p.reads_membership_zones = ZoneSpan::one(zone_of_zone_ref(zone));
                p
            }
        },
        // §L2: object-population per-turn journals. A ZONE-CHANGE journal counts a
        // SETTLED event keyed by its DESTINATION zone (CR 400.7 + CR 400.1), so a
        // battlefield token CREATION is zone-disjoint from a graveyard "died this
        // turn" read (Compy Swarm / Dripping-Tongue Zubera) while a self-sacrifice
        // bf→graveyard still overlaps (Chorale of the Void, held). ENTRY journals
        // keep the filter's zones (fail-closed) — a creation DOES feed them.
        QuantityRef::ZoneChangeCountThisTurn {
            to,
            filter,
            from: _,
        } => journal_zone_change_read(filter, to.as_ref()),
        QuantityRef::ZoneChangeAggregateThisTurn {
            to,
            filter,
            from: _,
            function: _,
            property: _,
        } => journal_zone_change_read(filter, to.as_ref()),
        // CR 701.21a: a sacrifice moves the permanent to its owner's graveyard.
        QuantityRef::SacrificedThisTurn { filter, player: _ } => {
            let mut p = board_membership_read(filter);
            p.reads_membership_zones = ZoneSpan::one(Zone::Graveyard);
            p
        }
        QuantityRef::EnteredThisTurn { filter }
        | QuantityRef::BattlefieldEntriesThisTurn { filter, .. }
        | QuantityRef::TokensCreatedThisTurn { filter, .. } => board_membership_read(filter),
        // §L2 (CR 122.3 + CR 603.3b): "a +1/+1 counter was put on a permanent this
        // turn" is a settled, MONOTONE-UP per-turn journal — counters are only
        // ADDED to it, so a sibling PutCounter keeps an intervening-if satisfied
        // and identical siblings resolve order-invariantly. Modeled as a frozen
        // look-back read (CR 603.10a — never fed while the freeze is valid) rather
        // than a LIVE counter read, so a self PutCounter does not falsely conflict
        // (Fairgrounds Trumpeter). Fail-closed on a departure reentry hazard.
        QuantityRef::CounterAddedThisTurn { .. } => reads_frozen_of(StateKind::ObjectCounters),
        // Player-resource journals.
        QuantityRef::LifeLostThisTurn { player: _ }
        | QuantityRef::LifeGainedThisTurn { player: _ }
        | QuantityRef::DamageDealtThisTurn { .. } => reads_player_of(StateKind::JournalLife),
        QuantityRef::CardsDrawnThisTurn { player: _ }
        | QuantityRef::CardsDiscardedThisTurn { .. } => reads_player_of(StateKind::JournalCards),
        QuantityRef::SpellsCastThisTurn { .. }
        | QuantityRef::SpellsCastLastTurn
        | QuantityRef::SpellsCastThisGame { .. }
        | QuantityRef::LoyaltyAbilitiesActivatedThisTurn { .. }
        | QuantityRef::PlayerActionsThisTurn { .. } => reads_player_of(StateKind::JournalCast),
        QuantityRef::UnspentMana { color: _ } => reads_player_of(StateKind::PlayerLife),
        QuantityRef::AttachmentsOnLeavingObject { .. } => reads_event_live(),
        QuantityRef::TimesCostPaidThisResolution => reads_event_live(),
        // D5 carriers.
        QuantityRef::EventContextAmount
        | QuantityRef::EventContextSourceCostX
        | QuantityRef::ManaSpentToCast { .. } => legacy_ref(),
    }
}

// ---------------------------------------------------------------------------
// Condition reads.
// ---------------------------------------------------------------------------

fn rw_ability_condition(x: &AbilityCondition) -> RwProfile {
    match x {
        AbilityCondition::QuantityCheck {
            lhs,
            rhs,
            comparator: _,
        } => {
            let mut p = rw_quantity_expr(lhs);
            p.merge(rw_quantity_expr(rhs));
            p
        }
        AbilityCondition::PreviousEffectAmount {
            rhs,
            comparator: _,
            channel: _,
        } => rw_quantity_expr(rhs),
        // §L7 (CR 608.2c + CR 603.3b): route EACH operand by scope. An operand
        // that is the card THIS resolution revealed (`LastRevealed`) is a
        // per-resolution local — no sibling write observes it (mirror
        // `RevealedHasCardType => EMPTY`); a source operand is a source-scoped
        // read. Only a LIVE board operand keeps the board `ObjectPt` read. Clears
        // "if the revealed card shares a creature type with ~" (Winnower Patrol /
        // Kithkin Zephyrnaut / Mudbutton Clanger / Waterspout Weavers).
        AbilityCondition::ObjectsShareQuality {
            subject,
            reference,
            quality: _,
        } => {
            let mut p = share_quality_operand_read(subject);
            p.merge(share_quality_operand_read(reference));
            p
        }
        // §L3 (CR 400.7 + CR 603.10a): "if that creature WAS a [type]" reads the
        // target's LAST-KNOWN information — a frozen read of the departed/event
        // object, settled at the event and never altered by a sibling board write.
        // Present-tense (`use_lki: false`) stays a LIVE board `ObjectPt` read
        // (fail-closed). Clears "if that [died/entered] creature was X, put a
        // counter on ~" (Taborax / Uglúk / Venom / Locus Cobra / Overgrowth
        // Elemental / Magnanimous Magistrate / Prowling Geistcatcher).
        AbilityCondition::TargetMatchesFilter {
            filter: _,
            use_lki,
            subject_slot,
        } => {
            let mut p = if *use_lki {
                frozen_source_read()
            } else {
                reads_board_of(StateKind::ObjectPt)
            };
            // CR 608.2c (PR-6.75 c5): Some(n) tests a specific DECLARED chain slot
            // resolved per member instance — the same per-member binding
            // member_bound_target_filter flags for TargetFilter::ParentTargetSlot.
            // Fail-closed: refuse batch-T1 (a member-bound read).
            if subject_slot.is_some() {
                p.reads_member_bound = true;
            }
            p
        }
        // PR-6.75 residual: two context-free-unclassifiable singletons stay safe
        // conservative over-prompts here, with no recognizer. Paroxysm's present-
        // tense `TargetMatchesFilter { use_lki: false }` fires on a card the member
        // itself revealed this resolution (a per-resolution local, not a live board
        // read), but that provenance is not visible on the AST at this leaf, so the
        // fail-closed live `ObjectPt` read is retained. Flamewake Phoenix's
        // begin-combat return is gated on controlling a creature with power >= 4 (a
        // P/T-constrained control census) — proving disjointness needs the runtime
        // source P/T the profile cannot see, so its control read stays fail-closed.
        AbilityCondition::SourceMatchesFilter { filter: _ } => reads_src_of(StateKind::ObjectPt),
        AbilityCondition::SourceIsTapped => reads_src_of(StateKind::TapState),
        AbilityCondition::ControllerControlsMatching { filter } => board_membership_read(filter),
        AbilityCondition::ScopedPlayerMatches { filter } => rw_player_filter(filter),
        AbilityCondition::TriggeringSpellTargetsFilter { filter: _ }
        | AbilityCondition::ZoneChangeObjectMatchesFilter { .. }
        | AbilityCondition::ZoneChangedThisWay { filter: _ }
        | AbilityCondition::CostPaidObjectMatchesFilter { filter: _ } => reads_event_live(),
        AbilityCondition::EventOutcomeWon => reads_event_live(),
        // CR 705.2: reads resolution-local `state.resolution_coin_flip` — a live
        // in-resolution signal, same read-bucket as `EventOutcomeWon`.
        AbilityCondition::CoinFlipOutcome { result: _ } => reads_event_live(),
        AbilityCondition::SpellCastWithVariantThisTurn { variant: _ }
        | AbilityCondition::NthResolutionThisTurn { n: _ } => {
            reads_player_of(StateKind::JournalCast)
        }
        // CR 701.20 + CR 603.3b: "if a card revealed THIS WAY has card type T" —
        // a read of the card the member's OWN parent reveal surfaced (a per-
        // resolution local, like an `ObjectScope::Recipient` read-modify-write:
        // §2 read-carrier closure). No sibling write can change the TYPE of the
        // card MY reveal surfaces; a sibling that reorders/moves the library only
        // changes WHICH card each identical member reveals, and identical
        // top-consuming functions compose order-independently (CR 603.3b T1:
        // f∘f = f∘f). Order-independence proof: two Delvers of Secrets off one
        // upkeep each look at the top card (a non-mutating Dig) and Transform{Self}
        // iff it is instant/sorcery — both read the SAME frozen top card and each
        // transforms its OWN source ⇒ identical board in either order; two Lurking
        // Predators off one spell cast each reveal-and-route the then-current top,
        // so the top-N cards are each routed by their own type regardless of which
        // copy processed which ⇒ identical library/battlefield in either order (no
        // feed — the read is the write's own reveal output). So `conservative()`
        // (which the coarse fallback assigned) falsely conflicts.
        AbilityCondition::RevealedHasCardType { .. } => RwProfile::empty(),
        AbilityCondition::SourceEnteredThisTurn
        | AbilityCondition::AdditionalCostPaid { .. }
        | AbilityCondition::CastVariantPaid { .. }
        | AbilityCondition::SourceAttachedToCreature
        | AbilityCondition::ControllerControlledMatchingAsCast { .. }
        | AbilityCondition::SourceLacksKeyword { .. }
        | AbilityCondition::WasStartingPlayer { .. } => frozen_source_read(),
        AbilityCondition::ConditionInstead { inner }
        | AbilityCondition::Not { condition: inner } => rw_ability_condition(inner),
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            let mut p = RwProfile::empty();
            for c in conditions {
                p.merge(rw_ability_condition(c));
            }
            p
        }
        AbilityCondition::AdditionalCostPaidInstead
        | AbilityCondition::AlternativeManaCostPaid
        | AbilityCondition::EffectOutcome { .. }
        | AbilityCondition::WhenYouDo
        | AbilityCondition::WasCast { .. }
        | AbilityCondition::CastDuringPhase { .. }
        | AbilityCondition::CurrentPhaseIs { .. }
        | AbilityCondition::CastTimingPermission { .. }
        | AbilityCondition::ManaColorSpent { .. }
        | AbilityCondition::TargetSharesNameWithOtherExiledThisWay { .. }
        | AbilityCondition::CastVariantPaidInstead { .. }
        | AbilityCondition::HasMaxSpeed
        | AbilityCondition::IsMonarch
        // CR 309.7: controller-state predicate; reads no object, writes nothing.
        | AbilityCondition::CompletedDungeon { .. }
        | AbilityCondition::IsInitiative
        | AbilityCondition::HasCityBlessing
        | AbilityCondition::IsRingBearer
        | AbilityCondition::TargetHasKeywordInstead { .. }
        | AbilityCondition::HasObjectTarget
        | AbilityCondition::IsYourTurn
        | AbilityCondition::FirstCombatPhaseOfTurn
        | AbilityCondition::FirstEndStepOfTurn
        | AbilityCondition::DayNightIsNeither
        | AbilityCondition::DayNightIs { .. } => RwProfile::empty(),
    }
}

fn rw_trigger_condition(x: &TriggerCondition) -> RwProfile {
    match x {
        TriggerCondition::GainedLife { minimum: _ }
        | TriggerCondition::LostLife
        | TriggerCondition::LostLifeLastTurn => reads_player_of(StateKind::JournalLife),
        // CR 120.3e + CR 603.3b: "dealt damage this turn" is combat/marked-damage
        // history (CR 120), NOT a life-total change (CR 119) — a frozen per-turn
        // fact about the damaged object, settled at damage time. A sibling
        // GainLife/LoseLife (a `PlayerLife`/`JournalLife` write) cannot alter it, so
        // it must NOT ride the life-journal row (the coarse conflation that flipped
        // Abattoir Ghoul). Order-independence proof: two Abattoir Ghouls off ONE
        // creature's death both read the SAME frozen "dealt damage this turn" flag
        // and gain that creature's (LKI-frozen) toughness ⇒ identical life in either
        // order (no feed: a life write doesn't change a damage-history fact).
        // `frozen_source_read` never feeds while the freeze is valid (marks
        // source/history dependence; fail-closed on a reentry hazard).
        TriggerCondition::DealtDamageBySourceThisTurn
        | TriggerCondition::DealtDamageThisTurnBySource { source: _ } => frozen_source_read(),
        TriggerCondition::LifeTotalGE { minimum: _ } => reads_player_of(StateKind::PlayerLife),
        TriggerCondition::ControlsType { filter }
        | TriggerCondition::ControlCount { filter, .. }
        | TriggerCondition::ControlsNone { filter }
        | TriggerCondition::DefendingPlayerControlsNone { filter } => board_membership_read(filter),
        TriggerCondition::QuantityComparison {
            lhs,
            rhs,
            comparator: _,
        } => {
            let mut p = rw_quantity_expr(lhs);
            p.merge(rw_quantity_expr(rhs));
            p
        }
        TriggerCondition::HadCounters { .. } => reads_frozen_of(StateKind::ObjectCounters),
        TriggerCondition::HasCounters { .. } => reads_src_of(StateKind::ObjectCounters),
        TriggerCondition::CounterAddedThisTurn => reads_board_of(StateKind::ObjectCounters),
        // CR 603.3b: Mirrors `CounterAddedThisTurn` — reads the
        // `counter_added_this_turn` board ledger. This is the fail-open CR 603.3b
        // fix: it must declare a board read here, NOT land in the terminal
        // `RwProfile::empty()` group with the tapped sibling (which reads a
        // count-map that is classified as no-read).
        TriggerCondition::FirstTimeObjectCountersAddedThisTurn => {
            reads_board_of(StateKind::ObjectCounters)
        }
        TriggerCondition::SourceIsTapped => reads_src_of(StateKind::TapState),
        TriggerCondition::SourceMatchesFilter { filter: _ } => reads_src_of(StateKind::ObjectPt),
        TriggerCondition::NoSpellsCastLastTurn
        | TriggerCondition::TwoOrMoreSpellsCastLastTurn
        | TriggerCondition::CastSpellThisTurn { .. }
        | TriggerCondition::SpellCastWithVariantThisTurn { .. } => {
            reads_player_of(StateKind::JournalCast)
        }
        TriggerCondition::DuringPlayersTurn { player } => rw_player_filter(player),
        TriggerCondition::SourceEnteredThisTurn
        | TriggerCondition::SourceIsHarnessed
        | TriggerCondition::SourceIsAttacking
        | TriggerCondition::SourceIsTransformed
        | TriggerCondition::SourceIsFaceUp
        | TriggerCondition::SourceIsFaceDown
        | TriggerCondition::SourceInZone { .. }
        | TriggerCondition::IsRenowned { .. }
        | TriggerCondition::WasStartingPlayer { .. } => frozen_source_read(),
        TriggerCondition::ZoneChangeObjectMatchesFilter { .. }
        | TriggerCondition::ZoneChangeObjectIsTapped
        | TriggerCondition::EventDamageSourceMatchesFilter { .. }
        | TriggerCondition::EventObjectMatchesFilter { .. }
        | TriggerCondition::DamagedPlayerIsEventSourceOwner
        | TriggerCondition::TriggeringSpellTargetsFilter { .. }
        // CR 601.2a + CR 603.4: reads the triggering `SpellCast` event's spell
        // object (event-live) — the "what it IS" sibling of the "what it targets"
        // arm above. `filter` is a frozen-event predicate (mirrors the sibling's
        // `{ .. }` elision); it carries no write and no member-bound read.
        | TriggerCondition::TriggeringSpellMatchesFilter { .. } => reads_event_live(),
        TriggerCondition::ManaColorSpent { .. } | TriggerCondition::ManaSpentCondition { .. } => {
            reads_player_of(StateKind::JournalCast)
        }
        TriggerCondition::And { conditions } | TriggerCondition::Or { conditions } => {
            let mut p = RwProfile::empty();
            for c in conditions {
                p.merge(rw_trigger_condition(c));
            }
            p
        }
        TriggerCondition::Not { condition } => rw_trigger_condition(condition),
        TriggerCondition::AttackersDeclaredCount { .. } => RwProfile::empty(),
        TriggerCondition::Descended
        | TriggerCondition::EchoDue
        | TriggerCondition::MinCoAttackers { .. }
        | TriggerCondition::SolveConditionMet
        | TriggerCondition::ClassLevelGE { .. }
        | TriggerCondition::AttractionVisitRoll { .. }
        | TriggerCondition::WasCast { .. }
        | TriggerCondition::WasPlayed
        | TriggerCondition::AdditionalCostPaid { .. }
        | TriggerCondition::CastVariantPaid { .. }
        | TriggerCondition::CastVariantPaidPersistent { .. }
        | TriggerCondition::ActivatedAbilityIsNonMana
        | TriggerCondition::FirstTimeObjectTappedThisTurn
        | TriggerCondition::WasType { .. }
        | TriggerCondition::AttackedThisTurn
        | TriggerCondition::FirstCombatPhaseOfTurn
        | TriggerCondition::HasMaxSpeed
        | TriggerCondition::IsMonarch
        | TriggerCondition::IsInitiative
        | TriggerCondition::NoMonarch
        | TriggerCondition::HasCityBlessing
        | TriggerCondition::CompletedDungeon { .. }
        | TriggerCondition::TributeNotPaid
        | TriggerCondition::CastDuringPhase { .. }
        | TriggerCondition::CastTimingPermission { .. }
        | TriggerCondition::ControlsCommander { .. }
        | TriggerCondition::ChosenLabelIs { .. }
        | TriggerCondition::ExceptFirstDrawInDrawStep
        | TriggerCondition::PlacedByAbilitySource => RwProfile::empty(),
    }
}

fn rw_static_condition(x: &StaticCondition) -> RwProfile {
    match x {
        StaticCondition::DevotionGE { .. }
        | StaticCondition::SharesColorWithMostCommonColorAmongPermanents => reads_zone_membership(),
        StaticCondition::IsPresent { filter } => match filter {
            Some(f) => board_membership_read(f),
            None => reads_zone_membership(),
        },
        StaticCondition::DefendingPlayerControls { filter } => board_membership_read(filter),
        StaticCondition::QuantityComparison {
            lhs,
            rhs,
            comparator: _,
        } => {
            let mut p = rw_quantity_expr(lhs);
            p.merge(rw_quantity_expr(rhs));
            p
        }
        StaticCondition::HasCounters { .. } => reads_src_of(StateKind::ObjectCounters),
        StaticCondition::IsTapped { scope, .. } => read_object_scope(scope, StateKind::TapState),
        StaticCondition::SourceIsTapped => reads_src_of(StateKind::TapState),
        StaticCondition::OpponentPoisonAtLeast { count: _ } => {
            reads_player_of(StateKind::PlayerLife)
        }
        StaticCondition::SpellCastWithVariantThisTurn { .. } => {
            reads_player_of(StateKind::JournalCast)
        }
        StaticCondition::SourceMatchesFilter { filter: _ } => reads_src_of(StateKind::ObjectPt),
        // CR 401/402: reads the controller's library top card (contents + order).
        // A draw/scry/surveil/mill/shuffle writes `HandLibrary`, so marking this
        // gate as reading `HandLibrary` invalidates it whenever the library top
        // can change — the correct dependency for a top-of-library static.
        StaticCondition::TopOfLibraryMatches { filter: _ } => {
            reads_player_of(StateKind::HandLibrary)
        }
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            let mut p = RwProfile::empty();
            for c in conditions {
                p.merge(rw_static_condition(c));
            }
            p
        }
        StaticCondition::Not { condition } => rw_static_condition(condition),
        StaticCondition::UnlessPay { .. } => RwProfile::conservative(),
        StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsAttacking
        | StaticCondition::SourceIsBlocking
        | StaticCondition::SourceIsBlocked
        | StaticCondition::SourceEnteredThisTurn
        | StaticCondition::SourceHasDealtDamage
        | StaticCondition::SourceIsSaddled
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsEnchanted
        | StaticCondition::SourceIsMonstrous
        | StaticCondition::SourceIsHarnessed
        | StaticCondition::SourceAttachedToCreature
        | StaticCondition::SourceIsPaired
        | StaticCondition::SourceInZone { .. }
        // CR 311.2: the source plane's face-up status is a source-object status
        // read (command-zone membership), grouped with the other frozen source
        // status flags (saddled/monstrous/…).
        | StaticCondition::SourceIsFaceUp
        | StaticCondition::WasStartingPlayer { .. } => frozen_source_read(),
        StaticCondition::RecipientHasCounters { .. }
        | StaticCondition::RecipientMatchesFilter { .. }
        | StaticCondition::RecipientAttackingOwnerTarget { .. } => RwProfile::empty(),
        StaticCondition::ChosenColorIs { .. }
        | StaticCondition::ChosenLabelIs { .. }
        | StaticCondition::HasMaxSpeed
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::DayNightIs { .. }
        | StaticCondition::CastVariantPaid { .. }
        | StaticCondition::ClassLevelGE { .. }
        | StaticCondition::IsMonarch
        | StaticCondition::IsInitiative
        | StaticCondition::NoMonarch
        | StaticCondition::HasCityBlessing
        | StaticCondition::CompletedADungeon
        | StaticCondition::Unrecognized { .. }
        | StaticCondition::DuringYourTurn
        | StaticCondition::WasCast { .. }
        | StaticCondition::IsRingBearer
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::ControlsCommander { .. }
        | StaticCondition::SourceControllerEquals { .. }
        | StaticCondition::EnchantedIsFaceDown
        | StaticCondition::AdditionalCostPaid
        | StaticCondition::CastingAsVariant { .. }
        | StaticCondition::None => RwProfile::empty(),
    }
}

// ---------------------------------------------------------------------------
// Filter / player / scope reads.
// ---------------------------------------------------------------------------

fn filter_prop_reads_player_choice_label(prop: &FilterProp) -> bool {
    match prop {
        FilterProp::ControllerChoseLabel { .. } => true,
        FilterProp::AnyOf { props } => props.iter().any(filter_prop_reads_player_choice_label),
        FilterProp::Not { prop } => filter_prop_reads_player_choice_label(prop),
        _ => false,
    }
}

/// A filter used as a READ carrier (target_chooser, nested filters). Selectors
/// are read-free; event-context refs contribute event reads (and D5 flags for
/// the 12 tags). Composite filters descend to catch nested event refs.
fn rw_target_filter(x: &TargetFilter) -> RwProfile {
    let mut p = match x {
        // D5 carriers (9 TargetFilter tags of the 12).
        TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::StackSpell
        | TargetFilter::CostPaidObject => legacy_ref(),
        // Non-D5 event refs. `ParentTargetSlot` is NOT one of the 12 retained tags
        // (it serializes as an object key the frozen serde oracle never matched —
        // the write path `target_is_legacy_ref` excludes it too), so it must NOT
        // set `legacy_batch_prompt`; it is a live event read like the others here.
        TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSourceController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::ChosenDamageSource { .. } => reads_event_live(),
        TargetFilter::Not { filter } | TargetFilter::TrackedSetFiltered { filter, .. } => {
            rw_target_filter(filter)
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            let mut p = RwProfile::empty();
            for f in filters {
                p.merge(rw_target_filter(f));
            }
            p
        }
        // CR 607.2d / CR 607.2m (by analogy): durable per-player anchor-label reads.
        TargetFilter::PlayerWhoChoseLabel { label: _ } => reads_player_of(StateKind::Other),
        TargetFilter::Typed(tf) => {
            if tf
                .properties
                .iter()
                .any(filter_prop_reads_player_choice_label)
            {
                reads_player_of(StateKind::Other)
            } else {
                RwProfile::empty()
            }
        }
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        // CR 201.5a: a bare object reference is a read-free selector (mirrors
        // `SpecificObject`); the member-bound bit is added by the trailing
        // `member_bound_target_filter` union below.
        | TargetFilter::GrantingObject
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::ChosenCard
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        // CR 201.5a: `OriginalSource` is a read-free object selector (concretized
        // to `SpecificObject` at resolution).
        | TargetFilter::OriginalSource
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => RwProfile::empty(),
    };
    // CR 603.10a (PR-6.75 c5): a member-bound referent used as a read carrier
    // (target_chooser / nested filter) refuses batch-T1. Position-agnostic —
    // `member_bound_target_filter` descends composites itself.
    p.reads_member_bound |= member_bound_target_filter(x);
    p
}

fn rw_player_filter(x: &PlayerFilter) -> RwProfile {
    match x {
        PlayerFilter::OpponentLostLife | PlayerFilter::OpponentGainedLife => {
            reads_player_of(StateKind::JournalLife)
        }
        PlayerFilter::OpponentDealtDamage {
            source: _,
            kind: _,
            min_sources: _,
        } => reads_player_of(StateKind::JournalLife),
        // D5 carrier.
        PlayerFilter::TriggeringPlayer => legacy_ref(),
        PlayerFilter::OpponentOtherThanTriggering
        | PlayerFilter::OpponentOfTriggeringPlayer
        | PlayerFilter::OpponentOfTriggeringPlayerNotAttacked
        | PlayerFilter::ParentObjectTargetController
        | PlayerFilter::ParentObjectTargetOwner => reads_event_live(),
        PlayerFilter::ControlsCount {
            filter,
            count,
            relation: _,
            comparator: _,
        } => {
            let mut p = board_membership_read(filter);
            p.merge(rw_quantity_expr(count));
            p
        }
        PlayerFilter::PlayerAttribute {
            attr,
            value,
            relation: _,
            comparator: _,
        } => {
            let mut p = rw_quantity_ref(attr);
            p.merge(rw_quantity_expr(value));
            p
        }
        PlayerFilter::AllExcept { exclude } => rw_player_filter(exclude),
        // CR 603.10a: the owners of the per-source exile set are a member-bound
        // look-back referent ⇒ refuse batch-T1.
        PlayerFilter::OwnersOfCardsExiledBySource => member_bound_read(),
        // CR 508.6 + CR 603.10a: resolves the enchanted-player anchor (the
        // source's `AttachedTo` host — a per-source look-back referent, exactly
        // like `ControllerRef::EnchantedPlayer`) and reads this-combat attack
        // declarations against it ⇒ member-bound (refuse batch-T1).
        PlayerFilter::OpponentAttackingEnchantedPlayer => member_bound_read(),
        PlayerFilter::Controller
        | PlayerFilter::Opponent
        | PlayerFilter::DefendingPlayer
        | PlayerFilter::HasLostTheGame
        | PlayerFilter::OpponentAttacked { .. }
        | PlayerFilter::All
        | PlayerFilter::HighestSpeed
        | PlayerFilter::ZoneChangedThisWay
        | PlayerFilter::PerformedActionThisWay { .. }
        | PlayerFilter::VotedFor { .. }
        | PlayerFilter::ChosenPlayer { .. } => RwProfile::empty(),
    }
}

fn rw_player_scope(x: &PlayerScope) -> RwProfile {
    match x {
        PlayerScope::ParentObjectTargetController => reads_event_live(),
        PlayerScope::AllPlayers { exclude, .. } => match exclude {
            Some(e) => rw_player_scope(e),
            None => RwProfile::empty(),
        },
        // CR 603.10a: per-source look-back referent ⇒ member-bound.
        PlayerScope::SourceChosenPlayer => member_bound_read(),
        // CR 513.1: `AnyTurn` is a turn-agnostic duration deadline reached via
        // `rw_duration`'s `UntilNextStepOf` walk — a pure timing referent that
        // reads no game state, exactly like the controller/scoped scopes.
        PlayerScope::Controller
        | PlayerScope::ScopedPlayer
        | PlayerScope::Target
        | PlayerScope::Opponent { .. }
        | PlayerScope::RecipientController
        | PlayerScope::AnyTurn
        | PlayerScope::DefendingPlayer => RwProfile::empty(),
    }
}

fn rw_controller_ref(x: &ControllerRef) -> RwProfile {
    match x {
        // D5 carriers.
        ControllerRef::ParentTargetController
        | ControllerRef::ParentTargetOwner
        | ControllerRef::TriggeringPlayer => legacy_ref(),
        // CR 603.10a: per-source look-back referents (Vote anchored on the source's
        // chosen player is a global APNAP interaction, not rescued by owner-partition
        // commutativity) ⇒ member-bound.
        ControllerRef::SourceChosenPlayer | ControllerRef::EnchantedPlayer => member_bound_read(),
        ControllerRef::You
        | ControllerRef::Opponent
        | ControllerRef::ScopedPlayer
        | ControllerRef::TargetPlayer
        // CR 109.4: runtime-read-identical to `TargetPlayer` (declared-target read,
        // no sibling-mutable state).
        | ControllerRef::TargetOpponent
        | ControllerRef::DefendingPlayer
        // CR 102.1: a live read of `state.active_player` — no sibling-mutable
        // state, empty RW profile (mirrors `DefendingPlayer`).
        | ControllerRef::ActivePlayer
        // resolution-local (ResolvedAbility.chosen_players)
        | ControllerRef::ChosenPlayer { .. } => RwProfile::empty(),
    }
}

// ---------------------------------------------------------------------------
// N-E unit pairings (§5.4). Build ASTs directly; assert profile+conflict.
// Each pairing is discriminating: the paired assertions bracket exactly one
// classification decision (see the revert-fail table in the impl report).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityKind, ChoiceType, Comparator, CountScope, PtValue, TargetSelectionMode,
    };
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{ObjectId, TrackedSetId};
    use crate::types::player::PlayerId;

    // ---- builders ----
    fn ra(effect: Effect) -> ResolvedAbility {
        ResolvedAbility::new(effect, vec![], ObjectId(1), PlayerId(0))
    }
    fn cond(mut a: ResolvedAbility, c: AbilityCondition) -> ResolvedAbility {
        a.condition = Some(c);
        a
    }
    fn qfix(v: i32) -> QuantityExpr {
        QuantityExpr::Fixed { value: v }
    }
    fn qref(r: QuantityRef) -> QuantityExpr {
        QuantityExpr::Ref { qty: r }
    }
    fn creature() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::creature())
    }
    fn sub(t: &str) -> TargetFilter {
        TargetFilter::Typed(TypedFilter::creature().subtype(t.to_string()))
    }
    fn power_src() -> QuantityRef {
        QuantityRef::Power {
            scope: ObjectScope::Source,
        }
    }
    fn tough_recip() -> QuantityRef {
        QuantityRef::Toughness {
            scope: ObjectScope::Recipient,
        }
    }
    fn counters_src() -> QuantityRef {
        QuantityRef::CountersOn {
            scope: ObjectScope::Source,
            counter_type: None,
        }
    }
    fn obj_count(f: TargetFilter) -> QuantityRef {
        QuantityRef::ObjectCount { filter: f }
    }
    fn put_counter_all(count: QuantityExpr, target: TargetFilter) -> Effect {
        Effect::PutCounterAll {
            count,
            target,
            counter_type: CounterType::Plus1Plus1,
        }
    }
    fn put_counter(count: QuantityExpr, target: TargetFilter) -> Effect {
        Effect::PutCounter {
            count,
            target,
            counter_type: CounterType::Plus1Plus1,
        }
    }
    fn remove_counter(target: TargetFilter) -> Effect {
        Effect::RemoveCounter {
            counter_type: None,
            count: qfix(1),
            target,
        }
    }
    fn gain_life(amount: QuantityExpr) -> Effect {
        Effect::GainLife {
            amount,
            player: TargetFilter::Controller,
        }
    }
    fn sacrifice_self() -> Effect {
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: qfix(1),
            min_count: 0,
        }
    }
    fn token(types: &[&str], count: QuantityExpr) -> Effect {
        Effect::Token {
            name: "t".into(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: types.iter().map(|s| s.to_string()).collect(),
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count,
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        }
    }
    fn change_zone(origin: Option<Zone>, dest: Zone, target: TargetFilter) -> Effect {
        Effect::ChangeZone {
            origin,
            destination: dest,
            target,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::default(),
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        }
    }
    fn copy_spell(target: TargetFilter) -> Effect {
        Effect::CopySpell {
            target,
            retarget: crate::types::ability::CopyRetargetPermission::KeepOriginalTargets,
            copier: None,
            additional_modifications: vec![],
            starting_loyalty_from_casualty_sacrifice: false,
        }
    }
    fn qcheck(lhs: QuantityRef, rhs: i32) -> AbilityCondition {
        AbilityCondition::QuantityCheck {
            lhs: qref(lhs),
            rhs: qfix(rhs),
            comparator: Comparator::GE,
        }
    }

    // ---- group structures ----
    fn se() -> GroupStructure {
        gs(true, false, false, false, true, SourceCensus::unknown())
    }
    fn se_phase() -> GroupStructure {
        gs(true, false, false, false, false, SourceCensus::unknown())
    }
    fn se_disjoint() -> GroupStructure {
        gs(true, false, false, true, true, SourceCensus::unknown())
    }
    fn batch() -> GroupStructure {
        gs(false, false, true, false, true, SourceCensus::unknown())
    }
    /// A controller-uniform (not owner-aligned) co-departure batch — the PR-6.75 c5
    /// batch-T1 shape.
    fn batch_uniform() -> GroupStructure {
        let mut s = batch();
        s.controller_uniformity = ControllerUniformity::Uniform;
        s
    }
    fn gs(
        same_event: bool,
        all_same_source: bool,
        self_departed: bool,
        excludes: bool,
        present: bool,
        source_census: SourceCensus,
    ) -> GroupStructure {
        GroupStructure {
            same_event,
            all_same_source,
            all_sources_self_departed: self_departed,
            event_object_excludes_sources: excludes,
            event_object_present: present,
            source_census,
            // PR-6.75: default `Mixed` ⇒ every prior verdict is byte-preserved (no
            // span/fused refinement and no batch-T1 consulted). Uniformity pins pass
            // `Uniform`/`UniformAligned` explicitly.
            controller_uniformity: ControllerUniformity::Mixed,
        }
    }

    fn conflicts(a: &ResolvedAbility, s: &GroupStructure) -> bool {
        profiles_conflict(&ability_rw_profile(a), s)
    }

    // ===================== PR-6.75 c5 batch-T1 unit pins (B-4 / B-7) =====================

    fn typed_ctrl(c: ControllerRef) -> TargetFilter {
        let mut tf = TypedFilter::creature();
        tf.controller = Some(c);
        TargetFilter::Typed(tf)
    }

    /// B-7 — `member_bound_target_filter` family axis (each TRUE row is a per-source
    /// tracked/chosen/attachment/event-context referent; each FALSE row rides
    /// another carrier or is uniformity-/owner-invariant). Nested composites descend.
    #[test]
    fn b7_member_bound_target_filter_families() {
        let ts = || TargetFilter::TrackedSet {
            id: TrackedSetId(0),
        };
        // TRUE: tracked/exiled/chosen/attachment/event-context refs + nesting.
        for f in [
            ts(),
            TargetFilter::Not {
                filter: Box::new(ts()),
            },
            TargetFilter::And {
                filters: vec![TargetFilter::Controller, ts()],
            },
            TargetFilter::TrackedSetFiltered {
                id: TrackedSetId(0),
                filter: Box::new(creature()),
                caused_by: None,
            },
            TargetFilter::ChosenCard,
            TargetFilter::HasChosenName,
            TargetFilter::AttachedTo,
            TargetFilter::EventTarget,
            TargetFilter::TriggeringSourceController,
            TargetFilter::OriginalController,
            typed_ctrl(ControllerRef::SourceChosenPlayer),
        ] {
            assert!(
                member_bound_target_filter(&f),
                "member-bound TRUE expected for {f:?}"
            );
        }
        // FALSE: source carriers, legacy-12, resolution-local, uniformity-/owner-invariant.
        for f in [
            TargetFilter::SelfRef,
            TargetFilter::TriggeringSource,
            TargetFilter::ParentTarget,
            TargetFilter::Controller,
            TargetFilter::Owner,
            TargetFilter::ScopedPlayer,
            TargetFilter::LastRevealed,
            TargetFilter::DefendingPlayer,
            typed_ctrl(ControllerRef::You),
            TargetFilter::None,
        ] {
            assert!(
                !member_bound_target_filter(&f),
                "member-bound FALSE expected for {f:?}"
            );
        }
    }

    /// B-7 — the quantity-arm split: per-source tracked/exiled/cast-context refs set
    /// `reads_member_bound`; turn-/commander-scoped refs stay clean.
    #[test]
    fn b7_quantity_member_bound_split() {
        for r in [
            QuantityRef::TrackedSetSize,
            QuantityRef::CardsExiledBySource,
            QuantityRef::CostXPaid,
            QuantityRef::ChosenNumber,
            QuantityRef::ConvokedCreatureCount,
        ] {
            assert!(
                rw_quantity_ref(&r).reads_member_bound,
                "quantity member-bound expected for {r:?}"
            );
        }
        for r in [
            QuantityRef::TurnsTaken,
            QuantityRef::DungeonsCompleted,
            QuantityRef::ExiledFromHandThisResolution,
        ] {
            assert!(
                !rw_quantity_ref(&r).reads_member_bound,
                "quantity member-unbound expected for {r:?}"
            );
        }
    }

    /// B-7 — `Choose{persist:true}` stores per-source `chosen_attributes` (member-
    /// bound); `persist:false` is resolution-local (slithermuse, member-unbound).
    #[test]
    fn b7_choose_persist_member_bound() {
        let choose = |persist: bool| Effect::Choose {
            choice_type: ChoiceType::Opponent { restriction: None },
            persist,
            selection: TargetSelectionMode::default(),
        };
        assert!(
            ability_rw_profile(&ra(choose(true))).reads_member_bound,
            "persist:true ⇒ member-bound"
        );
        assert!(
            !ability_rw_profile(&ra(choose(false))).reads_member_bound,
            "persist:false ⇒ member-unbound"
        );
    }

    /// B-7 — the `gs()` default is fail-closed `Mixed` (every prior verdict byte-
    /// preserved: no span/fused refinement and no batch-T1 consulted).
    #[test]
    fn b7_gs_default_is_mixed() {
        assert!(matches!(
            batch().controller_uniformity,
            ControllerUniformity::Mixed
        ));
        assert!(matches!(
            batch_uniform().controller_uniformity,
            ControllerUniformity::Uniform
        ));
    }

    /// B-7 — the batch-T1 clause in `profiles_conflict`: a source-independent,
    /// member-unbound, no-event profile whose sibling membership feed WOULD conflict
    /// is auto-ordered under `Uniform`, prompted under `Mixed`, and prompted under
    /// `Uniform` once `reads_member_bound` is set. Pins the `!= Mixed` and
    /// `!reads_member_bound` conjuncts against the SAME feed.
    #[test]
    fn b7_batch_t1_clause_conjuncts() {
        let feed = || {
            let mut p = RwProfile::empty();
            p.reads_board = KindSet::one(StateKind::SetMembership);
            p.reads_membership_census = Census::Any;
            p.reads_membership_zones = ZoneSpan::Any;
            p.writes_external = KindSet::one(StateKind::SetMembership);
            p.writes_membership_external_census = Census::Any;
            p.writes_membership_external_zones = ZoneSpan::Any;
            p
        };
        assert!(
            !profiles_conflict(&feed(), &batch_uniform()),
            "T1: uniform + source-independent + member-unbound ⇒ auto"
        );
        assert!(
            profiles_conflict(&feed(), &batch()),
            "T1 != Mixed conjunct: Mixed ⇒ gate off ⇒ membership feed prompts"
        );
        let mut mb = feed();
        mb.reads_member_bound = true;
        assert!(
            profiles_conflict(&mb, &batch_uniform()),
            "T1 !reads_member_bound conjunct: member-bound ⇒ T1 refused ⇒ feed prompts"
        );
    }

    /// B-4 — the `!writes_event_object.any()` conjunct. An event-object counter
    /// write (unreachable in isolation via the AST — every event-object write also
    /// sets `legacy_batch_prompt`/`reads_member_bound` — so pinned on a constructed
    /// profile) refuses T1, letting the counter feed prompt; clearing it auto-orders.
    #[test]
    fn b4_writes_event_object_conjunct() {
        let mut p = RwProfile::empty();
        p.reads_board = KindSet::one(StateKind::ObjectCounters);
        p.writes_external = KindSet::one(StateKind::ObjectCounters);
        p.writes_event_object = KindSet::one(StateKind::ObjectCounters);
        assert!(
            profiles_conflict(&p, &batch_uniform()),
            "B-4: event-object write refuses T1 ⇒ counter feed reached ⇒ prompt"
        );
        p.writes_event_object = KindSet::EMPTY;
        assert!(
            !profiles_conflict(&p, &batch_uniform()),
            "B-4 revert: without the event-object write, T1 auto-orders"
        );
    }

    /// R4 — the same-event event-object read/write feed discriminator
    /// (`profiles_conflict`: `s.same_event && s.event_object_present &&
    /// reads_and_writes_event_object`, CR 603.3b + CR 608.2h). A same-event group of
    /// DISTINCT sources that WRITES the shared triggering object AND READS its live
    /// characteristic is order-observable — but `feeds()` is structurally blind
    /// (`reads_event_live` records no KindSet). This pins the fix + all three guards.
    /// Revert-fail (measured): the NEG is `source_independent`, so BOTH edits are
    /// load-bearing — dropping the `:992` fast-path exclusion auto-orders it at the
    /// T1 disjunct, and dropping the discriminator lets it fall through every row to
    /// the final `return false` (the feed rows never see the KindSet-less live read).
    #[test]
    fn r4_same_event_event_object_feed_conjunct() {
        let feed = || {
            let mut p = RwProfile::empty();
            p.reads_event_live = true;
            p.writes_external = KindSet::one(StateKind::ObjectCounters);
            p.writes_event_object = KindSet::one(StateKind::ObjectCounters);
            p
        };
        // NEG (the fix): same-event, event object present, live read × event-object
        // write ⇒ PROMPT. `se()` is distinct-source, event_object_present true.
        assert!(
            profiles_conflict(&feed(), &se()),
            "R4 NEG: same-event event-object read×write feed ⇒ prompt"
        );
        // Conjunction guard A — the live READ alone (no event-object write) ⇒ AUTO.
        // Proves it is a feed (both endpoints), not a blanket same-event event-live
        // prompt: `reads_event_live` alone stays source-independent T1-clean.
        let mut read_only = feed();
        read_only.writes_external = KindSet::EMPTY;
        read_only.writes_event_object = KindSet::EMPTY;
        assert!(
            !profiles_conflict(&read_only, &se()),
            "R4 guard-A: event-live read with NO event-object write ⇒ auto"
        );
        // Conjunction guard B — the event-object WRITE alone (no live read) ⇒ AUTO
        // (an unread write is applied identically by every member, order-invariant).
        let mut write_only = feed();
        write_only.reads_event_live = false;
        assert!(
            !profiles_conflict(&write_only, &se()),
            "R4 guard-B: event-object write with NO event-live read ⇒ auto"
        );
        // event_object_present guard — the full feed but a Phase event (no live event
        // object; the write no-ops, targeting.rs:951) ⇒ AUTO (mirrors effective_external).
        assert!(
            !profiles_conflict(&feed(), &se_phase()),
            "R4 event_object_present guard: no live event object ⇒ write no-ops ⇒ auto"
        );
        // all_same_source guard — the feed on ONE shared source ⇒ identical f over one
        // shared event object (deterministic accumulation) ⇒ AUTO.
        let same_src = gs(true, true, false, false, true, SourceCensus::unknown());
        assert!(
            !profiles_conflict(&feed(), &same_src),
            "R4 all_same_source guard: shared source ⇒ f_A = f_B ⇒ auto"
        );
    }

    // ===================== base shapes =====================

    #[test]
    fn base_chaotic_goo_flipcoin_self_counters_clean() {
        // FlipCoin{win: PutCounter(SelfRef), lose: RemoveCounter(SelfRef)} — no
        // reads ⇒ clean even though the self-writes disable source-independence.
        let e = Effect::FlipCoin {
            win_effect: Some(Box::new(def(put_counter(qfix(1), TargetFilter::SelfRef)))),
            lose_effect: Some(Box::new(def(remove_counter(TargetFilter::SelfRef)))),
            flipper: TargetFilter::Controller,
        };
        let p = ability_rw_profile(&ra(e));
        assert!(
            p.writes_self.object_counters,
            "FlipCoin body descends to self-counter write"
        );
        assert!(!profiles_conflict(&p, &se_phase()));
    }

    #[test]
    fn base_gutter_grime_live_src_counter_read_vs_token_membership_clean() {
        // Observer alive: CountersOn{Source} LIVE read × token membership write —
        // counters vs membership don't feed (T3).
        let e = token(&["Creature", "Ooze"], qref(counters_src()));
        assert!(!conflicts(&ra(e), &batch()));
    }

    #[test]
    fn base_mana_crypt_flipcoin_player_damage_clean() {
        let e = Effect::FlipCoin {
            win_effect: None,
            lose_effect: Some(Box::new(def(Effect::DealDamage {
                amount: qfix(3),
                target: TargetFilter::Controller,
                damage_source: None,
                excess: None,
            }))),
            flipper: TargetFilter::Controller,
        };
        assert!(!conflicts(&ra(e), &se_phase()));
    }

    #[test]
    fn base_fruit_src_pt_read_vs_life_write_clean() {
        // Toughness{Source} read × life write — no feed (ObjectPt vs PlayerLife).
        let e = gain_life(qref(QuantityRef::Toughness {
            scope: ObjectScope::Source,
        }));
        assert!(!conflicts(&ra(e), &se()));
    }

    #[test]
    fn base_quirion_dryad_write_only_self_counter_clean() {
        assert!(!conflicts(
            &ra(put_counter(qfix(1), TargetFilter::SelfRef)),
            &se()
        ));
    }

    #[test]
    fn base_copyspell_selfref_clean_vs_topofstack_conflict() {
        // Walk-classification discriminator (D4, CR 707.10/707.10c): SelfRef/
        // explicit reads the original by id (no board read); the untargeted
        // fallback reads the MUTABLE stack top.
        let self_p = ability_rw_profile(&ra(copy_spell(TargetFilter::SelfRef)));
        let fallback_p = ability_rw_profile(&ra(copy_spell(TargetFilter::Any)));
        assert!(!self_p.reads_board.stack_shape, "SelfRef reads by id");
        assert!(
            fallback_p.reads_board.stack_shape,
            "fallback reads the mutable stack top"
        );
        // Under same-event the two identical source-independent copies commute
        // (f∘f) ⇒ both auto. The fallback's mutable-read hazard is order-relevant
        // on the distinct-event path, where the fallback conflicts and SelfRef
        // stays clean — the classification distinction made observable.
        assert!(conflicts(&ra(copy_spell(TargetFilter::Any)), &batch()));
        assert!(!conflicts(&ra(copy_spell(TargetFilter::SelfRef)), &batch()));
    }

    #[test]
    fn base_case_a_live_power_read_vs_board_counter_write_conflict() {
        // "put +1/+1 on each creature; draw if power>=6" — live Power{Source}
        // read × PutCounterAll board write; counters feed P/T.
        let a = cond(
            ra(put_counter_all(qfix(1), creature())),
            qcheck(power_src(), 6),
        );
        assert!(conflicts(&a, &se()));
    }

    #[test]
    fn base_graveyard_return_board_membership_conflict() {
        // board creature-count read × return-to-battlefield membership write,
        // census overlap (creature).
        let a = cond(
            ra(change_zone(
                Some(Zone::Graveyard),
                Zone::Battlefield,
                creature(),
            )),
            qcheck(obj_count(creature()), 1),
        );
        assert!(conflicts(&a, &batch()));
    }

    // ===================== (i) Mana × unless-pay guard =====================

    #[test]
    fn ne_i_mana_unless_pay_guard() {
        // echo: unless-pay + self-sac, NO pool write ⇒ clean.
        let mut echo = ability_rw_profile(&ra(sacrifice_self()));
        echo.has_pay_or_unless = true;
        assert!(!echo.writes_pool);
        assert!(!profiles_conflict(&echo, &se()));
        // synthetic: pool write + unless-pay ⇒ combination guard ⇒ conflict.
        let mut synth = RwProfile::empty();
        synth.writes_pool = true;
        synth.has_pay_or_unless = true;
        assert!(profiles_conflict(&synth, &se()));
    }

    // ===================== (ii) Recipient vs Source =====================

    #[test]
    fn ne_ii_recipient_vs_source() {
        // Canopy Gargantuan: Toughness{Recipient} ⇒ read-modify-write, no
        // sibling-read record ⇒ clean.
        let gargantuan = ra(put_counter_all(qref(tough_recip()), creature()));
        assert!(!conflicts(&gargantuan, &se()));
        // Ouroboroid: Power{Source} live read × PutCounterAll external write ⇒
        // counters feed P/T ⇒ conflict.
        let ouroboroid = ra(put_counter_all(qref(power_src()), creature()));
        assert!(conflicts(&ouroboroid, &se()));
    }

    // ===================== (iii) T1 completion =====================

    #[test]
    fn ne_iii_t1_source_independence() {
        // Endless Ranks: board count read × token membership write, no self
        // write ⇒ source-independent ⇒ same-event fast path ⇒ clean.
        let endless = ra(token(
            &["Creature", "Zombie"],
            qref(obj_count(sub("Zombie"))),
        ));
        assert!(ability_rw_profile(&endless).source_independent());
        assert!(!conflicts(&endless, &se()));
        // + a self-counter rider ⇒ source-DEPENDENT ⇒ falls to the board row ⇒
        // census overlap (zombie) ⇒ conflict.
        let mut dependent = endless.clone();
        dependent = dependent.sub_ability(ra(put_counter(qfix(1), TargetFilter::SelfRef)));
        assert!(!ability_rw_profile(&dependent).source_independent());
        assert!(conflicts(&dependent, &se()));
    }

    // ===================== (iv) census overlap =====================

    #[test]
    fn ne_iv_census_overlap() {
        // Pestilence: creature-count read × self-sac of an ENCHANTMENT source ⇒
        // census-disjoint ⇒ clean.
        let pestilence = cond(ra(sacrifice_self()), qcheck(obj_count(creature()), 1));
        let s = gs(
            true,
            false,
            false,
            false,
            false,
            SourceCensus::from_tags(["Enchantment".to_string()]),
        );
        assert!(!profiles_conflict(&ability_rw_profile(&pestilence), &s));
        // Docent: Wizard-count read × Wizard-token write + self-transform ⇒
        // overlap ⇒ conflict.
        let docent = cond(
            ra(token(&["Creature", "Wizard"], qfix(1))).sub_ability(ra(Effect::Transform {
                target: TargetFilter::SelfRef,
            })),
            qcheck(obj_count(sub("Wizard")), 1),
        );
        assert!(conflicts(&docent, &se()));
    }

    #[test]
    fn major1_unfiltered_zone_membership_read_conflicts() {
        // MAJOR-1: a whole-zone `GraveyardSize` read carries census `Any`
        // (unextractable ⇒ overlap assumed). "return all creature cards from
        // your graveyard to the battlefield; draw if graveyard has >=3 cards" —
        // board GraveyardSize read × sibling return-to-battlefield membership
        // write ⇒ census-overlap conflict on the departure-batch path (where the
        // same-event f∘f short-circuit does not apply).
        let a = cond(
            ra(change_zone(
                Some(Zone::Graveyard),
                Zone::Battlefield,
                creature(),
            )),
            qcheck(
                QuantityRef::GraveyardSize {
                    player: PlayerScope::Controller,
                },
                3,
            ),
        );
        assert!(conflicts(&a, &batch()));
    }

    #[test]
    fn zone_census_battlefield_write_vs_graveyard_read_discriminates() {
        // Tombstone Stairwell: a battlefield Zombie-token creation (Token{creature}
        // ⇒ SetMembership dest = Battlefield) whose COUNT reads the GRAVEYARD
        // creature count. The write and the read overlap on TYPE (creature) but
        // their ZONES are disjoint (CR 400.1: a fresh token touches only the
        // battlefield; the count reads the graveyard) ⇒ no feed ⇒ clean. The
        // frozen source condition (`SourceEnteredThisTurn`) only disables the T1
        // source-independent fast path so the feed rows are reached — exactly what
        // Tombstone's `SourceInZone{Battlefield}` intervening-if does.
        let in_zone = |z: Zone| {
            let mut tf = TypedFilter::creature();
            tf.properties.push(FilterProp::InZone { zone: z });
            TargetFilter::Typed(tf)
        };
        let disjoint = cond(
            ra(token(
                &["Creature"],
                qref(obj_count(in_zone(Zone::Graveyard))),
            )),
            AbilityCondition::SourceEnteredThisTurn,
        );
        assert!(
            !conflicts(&disjoint, &se()),
            "battlefield token write × GRAVEYARD creature-count read ⇒ zone-disjoint ⇒ clean"
        );

        // The SAME read/write with the count scoped to the BATTLEFIELD (matching
        // zones) ⇒ census AND zone overlap ⇒ conflict. This is the discriminating
        // witness: a zone-BLIND census would report the disjoint pairing as a
        // conflict too, so dropping the zone check flips the first assertion.
        let same_zone = cond(
            ra(token(
                &["Creature"],
                qref(obj_count(in_zone(Zone::Battlefield))),
            )),
            AbilityCondition::SourceEnteredThisTurn,
        );
        assert!(
            conflicts(&same_zone, &se()),
            "battlefield token write × BATTLEFIELD creature-count read ⇒ same zone ⇒ conflict"
        );
    }

    // ===================== (v) chain-root =====================

    #[test]
    fn ne_v_chain_root() {
        // Smoldering Egg: PutCounter{SelfRef} → RemoveCounter{ParentTarget} +
        // CountersOn{Source} read ⇒ chain root SelfRef ⇒ self-write ⇒ clean.
        let egg = cond(
            ra(put_counter(qfix(1), TargetFilter::SelfRef))
                .sub_ability(ra(remove_counter(TargetFilter::ParentTarget))),
            qcheck(counters_src(), 1),
        );
        assert!(!conflicts(&egg, &se()));
        // Re-rooted at a Typed filter (root NOT SelfRef) ⇒ external counter write
        // × live src-counter read ⇒ conflict.
        let rerooted = cond(
            ra(put_counter(qfix(1), creature()))
                .sub_ability(ra(remove_counter(TargetFilter::ParentTarget))),
            qcheck(counters_src(), 1),
        );
        assert!(conflicts(&rerooted, &se()));
    }

    // ===================== (vi) event-object disjointness =====================

    #[test]
    fn ne_vi_event_object_disjointness() {
        // Railway Brawler: PutCounter{TriggeringSource, count: Power{Source}} with
        // a source-excluding valid_card ⇒ event-object write excluded from
        // src-read scoping ⇒ clean.
        let brawler = ra(put_counter(
            qref(power_src()),
            TargetFilter::TriggeringSource,
        ));
        assert!(!conflicts(&brawler, &se_disjoint()));
        // Without source-exclusion the event object can be a sibling's source ⇒
        // external counter write feeds the live Power{Source} read ⇒ conflict.
        assert!(conflicts(&brawler, &se()));
    }

    // ===================== (vii) parentless-root =====================

    #[test]
    fn ne_vii_parentless_root() {
        // Root Bounce{ParentTarget} (parentless) + a SelfRef-scoped membership
        // read. On a ZoneChanged trigger the referent is the EVENT object.
        let ast = cond(
            ra(Effect::Bounce {
                target: TargetFilter::ParentTarget,
                destination: None,
                selection: crate::types::ability::BounceSelection::Targeted,
            }),
            qcheck(
                obj_count(TargetFilter::And {
                    filters: vec![TargetFilter::SelfRef],
                }),
                1,
            ),
        );
        // (a) source-excluding valid_card ⇒ rule 2 clears ⇒ clean.
        assert!(!conflicts(&ast, &se_disjoint()));
        // (b) no exclusion ⇒ event object can be a sibling source ⇒ conflict.
        assert!(conflicts(&ast, &se()));
        // (c) Phase trigger (no event object) ⇒ None ⇒ no write ⇒ clean.
        assert!(!conflicts(&ast, &se_phase()));
    }

    // ===================== D5 legacy-visitor tag × position matrix =====================

    /// Mechanical proof that `legacy_batch_prompt` (via the authoritative
    /// `contains_legacy_event_ref` visitor, driven through the production
    /// `ability_rw_profile` / `trigger_condition_rw_profile` entry points) fires
    /// for EACH of the 12 frozen event-context tags in EACH structural position —
    /// including the effect target/count positions the read/write walk drops (the
    /// D5 holes). This is the coverage whose absence let the 50-card fail-open ship.
    /// Revert-fail witness: dropping the `p.legacy_batch_prompt =
    /// contains_legacy_event_ref(a)` override flips every `Discard`/
    /// `PutAtLibraryPosition`/`Sacrifice`-count/`GainEnergy` assertion below to
    /// false (the walk never routes those fields through a legacy leaf detector).
    #[test]
    fn d5_legacy_visitor_tag_x_position_matrix() {
        use crate::types::ability::{
            CardSelectionMode, CastManaObjectScope, CastManaSpentMetric, ControllerRef,
            LibraryPosition, PlayerFilter,
        };

        // Production profiling entry points — NOT the private visitor directly.
        let legacy = |a: &ResolvedAbility| ability_rw_profile(a).legacy_batch_prompt();
        let tlegacy = |c: &TriggerCondition| trigger_condition_rw_profile(c).legacy_batch_prompt();

        // Dropped-target effects (the D5 hole class: `target: _` in `rw_effect`).
        let discard = |t: TargetFilter| Effect::Discard {
            count: qfix(1),
            target: t,
            unless_filter: None,
            filter: None,
            selection: CardSelectionMode::Chosen,
        };
        let put_at_lib = |t: TargetFilter| Effect::PutAtLibraryPosition {
            target: t,
            count: qfix(1),
            position: LibraryPosition::Top,
        };
        let destroy = |t: TargetFilter| Effect::Destroy {
            target: t,
            cant_regenerate: false,
        };

        // ---- The 9 TargetFilter carriers × 5 positions ----
        let tf_tags = [
            TargetFilter::TriggeringSpellController,
            TargetFilter::TriggeringSpellOwner,
            TargetFilter::TriggeringPlayer,
            TargetFilter::TriggeringSource,
            TargetFilter::ParentTarget,
            TargetFilter::ParentTargetController,
            TargetFilter::ParentTargetOwner,
            TargetFilter::StackSpell,
            TargetFilter::CostPaidObject,
        ];
        for tag in &tf_tags {
            // dropped-target effect positions (the fail-open holes).
            assert!(legacy(&ra(discard(tag.clone()))), "Discard target {tag:?}");
            assert!(
                legacy(&ra(put_at_lib(tag.clone()))),
                "PutAtLibraryPosition target {tag:?}"
            );
            // routed effect target.
            assert!(legacy(&ra(destroy(tag.clone()))), "Destroy target {tag:?}");
            // sub-ability (chained) target.
            assert!(
                legacy(&ra(gain_life(qfix(1))).sub_ability(ra(discard(tag.clone())))),
                "sub-ability target {tag:?}"
            );
            // nested filter position.
            assert!(
                legacy(&ra(destroy(TargetFilter::And {
                    filters: vec![creature(), tag.clone()],
                }))),
                "nested And filter {tag:?}"
            );
        }

        // ---- The 3 QuantityRef carriers × 3 positions ----
        let qr_tags = [
            QuantityRef::EventContextAmount,
            QuantityRef::EventContextSourceCostX,
            QuantityRef::ManaSpentToCast {
                scope: CastManaObjectScope::TriggeringSpell,
                metric: CastManaSpentMetric::Total,
            },
        ];
        for tag in &qr_tags {
            // effect count (god-eternal bontu class).
            assert!(
                legacy(&ra(Effect::Sacrifice {
                    target: creature(),
                    count: qref(tag.clone()),
                    min_count: 0,
                })),
                "Sacrifice count {tag:?}"
            );
            // effect amount (dropped-target effect with a dynamic amount).
            assert!(
                legacy(&ra(Effect::GainEnergy {
                    amount: qref(tag.clone()),
                })),
                "GainEnergy amount {tag:?}"
            );
            // trigger-level intervening-if condition.
            assert!(
                tlegacy(&TriggerCondition::QuantityComparison {
                    lhs: qref(tag.clone()),
                    rhs: qfix(0),
                    comparator: Comparator::GE,
                }),
                "trigger condition {tag:?}"
            );
        }

        // ---- ObjectScope::CostPaidObject (the 12th tag) as a quantity scope ----
        assert!(
            legacy(&ra(Effect::GainEnergy {
                amount: qref(QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject,
                }),
            })),
            "CostPaidObject quantity scope"
        );

        // ---- PlayerFilter::TriggeringPlayer (effect player_filter + ability scope) ----
        assert!(
            legacy(&ra(Effect::DamageEachPlayer {
                amount: qfix(1),
                player_filter: PlayerFilter::TriggeringPlayer,
            })),
            "DamageEachPlayer player_filter TriggeringPlayer"
        );
        let mut with_scope = ra(gain_life(qfix(1)));
        with_scope.player_scope = Some(PlayerFilter::TriggeringPlayer);
        assert!(legacy(&with_scope), "ability player_scope TriggeringPlayer");

        // ---- ControllerRef carriers (ability starting_with) ----
        for cr in [
            ControllerRef::TriggeringPlayer,
            ControllerRef::ParentTargetController,
            ControllerRef::ParentTargetOwner,
        ] {
            let mut a = ra(gain_life(qfix(1)));
            a.starting_with = Some(cr.clone());
            assert!(legacy(&a), "starting_with {cr:?}");
        }

        // ---- NEGATIVE controls (discrimination: no tag ⇒ false) ----
        assert!(
            !legacy(&ra(discard(TargetFilter::Controller))),
            "Discard{{Controller}} carries no frozen tag"
        );
        assert!(
            !legacy(&ra(destroy(creature()))),
            "Destroy{{creature}} carries no frozen tag"
        );
        assert!(
            !legacy(&ra(Effect::Sacrifice {
                target: creature(),
                count: qfix(1),
                min_count: 0,
            })),
            "Sacrifice with a fixed count carries no frozen tag"
        );
        assert!(
            !tlegacy(&TriggerCondition::SourceIsTapped),
            "SourceIsTapped carries no frozen tag"
        );
    }

    /// #5072 dual-walk (§6.3): `ObjectScope::OtherRevealedCard` is DELIBERATELY
    /// classified as a per-resolution local — `legacy_object_scope == false` (not
    /// one of the 12 retained refs) and `read_object_scope == RwProfile::empty()`
    /// (the other revealer's card is surfaced by THIS ability's own reveal and its
    /// read is the card's intrinsic printed MV, observed by no sibling write). The
    /// classification mirrors `ObjectScope::Recipient`, proving the new variant was
    /// classified on purpose rather than silently defaulted.
    #[test]
    fn other_revealed_card_scope_is_per_resolution_local() {
        // `RwProfile` has no `PartialEq`; compare via its `Debug` form, which
        // fully renders every read/write field.
        let dbg = |p: RwProfile| format!("{p:?}");
        let empty = dbg(RwProfile::empty());
        assert!(
            !legacy_object_scope(&ObjectScope::OtherRevealedCard),
            "OtherRevealedCard is not a retained legacy ref"
        );
        for kind in [StateKind::ObjectPt, StateKind::ObjectCounters] {
            assert_eq!(
                dbg(read_object_scope(&ObjectScope::OtherRevealedCard, kind)),
                empty,
                "OtherRevealedCard reads nothing observable ({kind:?})"
            );
            // Deliberately identical to the Recipient per-resolution-local precedent.
            assert_eq!(
                dbg(read_object_scope(&ObjectScope::OtherRevealedCard, kind)),
                dbg(read_object_scope(&ObjectScope::Recipient, kind)),
                "OtherRevealedCard mirrors the Recipient per-resolution-local profile"
            );
        }
        // Discrimination: a genuine board ref (Target) is NOT empty, so the empty
        // assertions above cannot pass vacuously.
        assert_ne!(
            dbg(read_object_scope(&ObjectScope::Target, StateKind::ObjectPt)),
            empty,
            "Target is a live-board ref — proves the empty checks discriminate"
        );
    }

    // ===================== PR-6.75 commit-4 read/write levers =====================
    // Each lever gets a discriminating POSITIVE (the commuting shape no longer
    // conflicts) + NEGATIVE control (a structurally-adjacent NON-commuting shape
    // still conflicts, so the POS can't pass vacuously). Revert-fail evidence (the
    // pre-lever mutation that turns each POS red) is recorded in the driver report.

    /// §L1: `QuantityRef::ZoneCardCount { zone, card_types, filter: None }` now
    /// extracts BOTH the read census (from `card_types`, CR 205) and the read zone
    /// (from `ZoneRef`, CR 400.1) instead of collapsing to `Any`/`Any`. "creature
    /// cards in your graveyard" reads a GRAVEYARD/creature census — zone-disjoint
    /// from a battlefield creature token, but zone+census-overlapping a creature
    /// written INTO the graveyard.
    #[test]
    fn l1_zone_card_count_census_and_zone_extraction() {
        let gy_creatures = QuantityRef::ZoneCardCount {
            zone: ZoneRef::Graveyard,
            card_types: vec![TypeFilter::Creature],
            filter: None,
            scope: CountScope::Controller,
        };
        // POS: graveyard/creature read × battlefield creature-token write —
        // census overlaps (creature) but zones are disjoint ⇒ no feed.
        let pos = cond(
            ra(token(&["Creature"], qfix(1))),
            qcheck(gy_creatures.clone(), 1),
        );
        assert!(
            !conflicts(&pos, &batch()),
            "graveyard-creature read × battlefield token ⇒ zone-disjoint ⇒ clean"
        );
        // NEG: same read × a creature written INTO the graveyard — census AND zone
        // overlap ⇒ conflict. A zone-blind (pre-lever `Any`) census would also
        // conflict on the POS, so this pins the discrimination on the zone axis.
        let neg = cond(
            ra(change_zone(
                Some(Zone::Battlefield),
                Zone::Graveyard,
                creature(),
            )),
            qcheck(gy_creatures, 1),
        );
        assert!(
            conflicts(&neg, &batch()),
            "graveyard-creature read × creature-to-graveyard write ⇒ zone+census overlap ⇒ conflict"
        );
    }

    /// §L2 (CR 400.7 + CR 400.1): a per-turn zone-change journal read is keyed to
    /// its DESTINATION zone, so a "died this turn" (→graveyard) read is disjoint
    /// from a battlefield token creation while an "entered this turn" (→battlefield)
    /// read overlaps it. `SacrificedThisTurn` is graveyard-keyed the same way.
    #[test]
    fn l2_zone_journal_reads_keyed_to_destination_zone() {
        let died = |to: Zone| QuantityRef::ZoneChangeCountThisTurn {
            from: Some(Zone::Battlefield),
            to: Some(to),
            filter: creature(),
        };
        // POS: "a creature died this turn" (→graveyard) × battlefield token ⇒
        // zone-disjoint ⇒ clean.
        let pos = cond(
            ra(token(&["Creature"], qfix(1))),
            qcheck(died(Zone::Graveyard), 1),
        );
        assert!(
            !conflicts(&pos, &batch()),
            "died(graveyard) × battlefield token ⇒ clean"
        );
        // NEG: "a creature entered this turn" (→battlefield) × the same token ⇒
        // zone overlap ⇒ conflict.
        let neg = cond(
            ra(token(&["Creature"], qfix(1))),
            qcheck(died(Zone::Battlefield), 1),
        );
        assert!(
            conflicts(&neg, &batch()),
            "entered(battlefield) × battlefield token ⇒ conflict"
        );

        // SacrificedThisTurn is pinned to the graveyard (CR 701.21a): disjoint from
        // a battlefield token, overlapping a creature moved into the graveyard.
        let sac = QuantityRef::SacrificedThisTurn {
            player: PlayerScope::Controller,
            filter: creature(),
        };
        let sac_pos = cond(ra(token(&["Creature"], qfix(1))), qcheck(sac.clone(), 1));
        assert!(
            !conflicts(&sac_pos, &batch()),
            "sacrificed(graveyard) × battlefield token ⇒ clean"
        );
        let sac_neg = cond(
            ra(change_zone(
                Some(Zone::Battlefield),
                Zone::Graveyard,
                creature(),
            )),
            qcheck(sac, 1),
        );
        assert!(
            conflicts(&sac_neg, &batch()),
            "sacrificed(graveyard) × creature-to-graveyard ⇒ conflict"
        );
    }

    /// §L2 (CR 122.3 + CR 603.10a): "a counter was put on a permanent this turn" is
    /// a settled monotone-up look-back — a FROZEN read that a sibling PutCounter
    /// never feeds (Fairgrounds Trumpeter). A LIVE counter read on the same self
    /// write still conflicts.
    #[test]
    fn l2_counter_added_this_turn_is_frozen_lookback() {
        let added = QuantityRef::CounterAddedThisTurn {
            actor: CountScope::Controller,
            counters: crate::types::counter::CounterMatch::Any,
            target: creature(),
        };
        // POS: frozen journal read × self PutCounter ⇒ never fed ⇒ clean.
        let pos = cond(
            ra(put_counter(qfix(1), TargetFilter::SelfRef)),
            qcheck(added, 1),
        );
        assert!(
            !conflicts(&pos, &se()),
            "monotone journal look-back × self counter ⇒ clean"
        );
        // NEG: a LIVE board counter read (`CountersOn{Target}`) × the same self
        // counter write ⇒ same-kind ObjectCounters feed ⇒ conflict.
        let live = QuantityRef::CountersOn {
            scope: ObjectScope::Target,
            counter_type: None,
        };
        let neg = cond(
            ra(put_counter(qfix(1), TargetFilter::SelfRef)),
            qcheck(live, 1),
        );
        assert!(
            conflicts(&neg, &se()),
            "live board counter read × self counter write ⇒ conflict"
        );
    }

    /// §L3 (CR 400.7 + CR 603.10a): "if that creature WAS a [type]"
    /// (`use_lki: true`) is a frozen LKI read of the departed/event object, never
    /// altered by a sibling board write. Present-tense (`use_lki: false`) stays a
    /// live board `ObjectPt` read.
    #[test]
    fn l3_target_matches_filter_lki_is_frozen_read() {
        let put_self = || ra(put_counter(qfix(1), TargetFilter::SelfRef));
        // POS: LKI "was a Cleric" × PutCounter{self} ⇒ frozen read ⇒ clean
        // (Taborax / Uglúk / Overgrowth Elemental).
        let pos = cond(
            put_self(),
            AbilityCondition::TargetMatchesFilter {
                filter: creature(),
                use_lki: true,
                subject_slot: None,
            },
        );
        assert!(
            !conflicts(&pos, &se()),
            "LKI 'was X' read × self counter ⇒ frozen ⇒ clean"
        );
        // NEG: present-tense "is a Cleric" × the same write ⇒ live ObjectPt read ×
        // ObjectCounters write ⇒ CR 613.4 feed ⇒ conflict.
        let neg = cond(
            put_self(),
            AbilityCondition::TargetMatchesFilter {
                filter: creature(),
                use_lki: false,
                subject_slot: None,
            },
        );
        assert!(
            conflicts(&neg, &se()),
            "live 'is X' read × self counter ⇒ conflict"
        );
    }

    /// §L6 (CR 303.4d + CR 301.5c): an Aura/Equipment is never attached to itself
    /// (an Aura can't enchant itself, an Equipment can't equip itself),
    /// so an `AttachedTo` `valid_card` (the enchanted creature — the Ordeals'
    /// "whenever enchanted creature attacks") provably EXCLUDES every source. The
    /// classifier output is wired straight into the group structure so the excluded
    /// event-object counter write drops from the source-counter-read feed.
    #[test]
    fn l6_attached_to_excludes_source() {
        // Direct classifier assertions (the lever).
        assert!(
            filter_excludes_source(&TargetFilter::AttachedTo),
            "AttachedTo excludes source"
        );
        assert!(
            !filter_excludes_source(&TargetFilter::SelfRef),
            "SelfRef does not exclude"
        );
        assert!(
            !filter_excludes_source(&creature()),
            "a bare creature filter does not exclude"
        );

        // Ordeal shape: source Aura reads its own 3-counter quest, event-object
        // write puts a counter on the enchanted creature (TriggeringSource).
        let ordeal = cond(
            ra(put_counter(qfix(1), TargetFilter::TriggeringSource)),
            qcheck(counters_src(), 3),
        );
        // POS: AttachedTo ⇒ excludes=true ⇒ event-object write dropped ⇒ clean.
        let pos = gs(
            true,
            false,
            false,
            filter_excludes_source(&TargetFilter::AttachedTo),
            true,
            SourceCensus::unknown(),
        );
        assert!(!profiles_conflict(&ability_rw_profile(&ordeal), &pos));
        // NEG: SelfRef ⇒ excludes=false ⇒ the counter write feeds the read ⇒ conflict.
        let neg = gs(
            true,
            false,
            false,
            filter_excludes_source(&TargetFilter::SelfRef),
            true,
            SourceCensus::unknown(),
        );
        assert!(profiles_conflict(&ability_rw_profile(&ordeal), &neg));
    }

    /// §L7 (CR 608.2c + CR 603.3b): an `ObjectsShareQuality` operand that is the
    /// card THIS resolution revealed (`LastRevealed`) is a per-resolution local —
    /// observed by no sibling write (mirrors `RevealedHasCardType => EMPTY`). A live
    /// board operand keeps the fail-closed `ObjectPt` read.
    #[test]
    fn l7_objects_share_quality_last_revealed_is_local() {
        let share = |subject: TargetFilter, reference: TargetFilter| {
            AbilityCondition::ObjectsShareQuality {
                subject,
                reference,
                quality: crate::types::ability::SharedQuality::CreatureType,
            }
        };
        // POS: revealed-card × revealed-card ⇒ both local ⇒ no read × self counter
        // write ⇒ clean (Winnower Patrol / Kithkin Zephyrnaut).
        let pos = cond(
            ra(put_counter(qfix(1), TargetFilter::SelfRef)),
            share(TargetFilter::LastRevealed, TargetFilter::LastRevealed),
        );
        assert!(
            !conflicts(&pos, &se()),
            "revealed × revealed operands ⇒ local ⇒ clean"
        );
        // NEG: a LIVE board operand (a creature you control) × revealed card ⇒
        // board ObjectPt read × ObjectCounters write ⇒ conflict.
        let neg = cond(
            ra(put_counter(qfix(1), TargetFilter::SelfRef)),
            share(creature(), TargetFilter::LastRevealed),
        );
        assert!(
            conflicts(&neg, &se()),
            "live board operand × revealed card ⇒ conflict"
        );
    }

    /// §L9 (CR 707.10 + CR 115.7): a `CopySpell` targeting `TriggeringSource` /
    /// `ParentTarget` reads the original BY ID, not the mutable stack top — so
    /// independent copies commute (Pyromancer Ascension / Curse of Echoes /
    /// Ominous Lockbox). `ChangeTargets` is a precise StackShape write, not the
    /// maximal-conservative board read.
    #[test]
    fn l9_copy_spell_by_id_and_change_targets_stack_write() {
        // POS: id-referential copies carry no StackShape BOARD read ⇒ commute.
        for by_id in [TargetFilter::TriggeringSource, TargetFilter::ParentTarget] {
            let p = ability_rw_profile(&ra(copy_spell(by_id.clone())));
            assert!(
                !p.reads_board.stack_shape,
                "{by_id:?} copies the original by id"
            );
            assert!(
                !conflicts(&ra(copy_spell(by_id.clone())), &batch()),
                "{by_id:?} copies commute"
            );
        }
        // NEG: the untargeted top-of-stack fallback reads the MUTABLE stack top.
        let fb = ability_rw_profile(&ra(copy_spell(TargetFilter::Any)));
        assert!(
            fb.reads_board.stack_shape,
            "untargeted fallback reads the stack top"
        );
        assert!(conflicts(&ra(copy_spell(TargetFilter::Any)), &batch()));

        // ChangeTargets: a StackShape write only — NOT the conservative board read.
        let ct = ability_rw_profile(&ra(Effect::ChangeTargets {
            target: TargetFilter::StackSpell,
            scope: crate::types::game_state::RetargetScope::Single,
            forced_to: None,
        }));
        assert!(
            ct.writes_external.stack_shape,
            "ChangeTargets writes StackShape"
        );
        assert!(
            !ct.reads_board.object_pt,
            "ChangeTargets is not the maximal-conservative read"
        );
        assert!(
            !conflicts(
                &ra(Effect::ChangeTargets {
                    target: TargetFilter::StackSpell,
                    scope: crate::types::game_state::RetargetScope::Single,
                    forced_to: None,
                }),
                &batch()
            ),
            "independent per-copy retargets commute"
        );
    }

    /// §L12 (CR 701.60 + CR 701.60a): suspect/unsuspect is an idempotent
    /// designation with NO observable RW kind — never the `Other` catch-all that
    /// falsely conflicted with Frantic Scapegoat's "if ~ is suspected" source read.
    #[test]
    fn l12_suspect_unsuspect_no_observable_write() {
        // "if ~ is suspected" — a source read (SourceMatchesFilter ⇒ reads_src).
        let is_suspected = || AbilityCondition::SourceMatchesFilter { filter: creature() };
        // POS: Suspect (empty profile) × the source read ⇒ no write to feed ⇒ clean.
        for status in [
            Effect::Suspect {
                target: creature(),
                scope: crate::types::ability::EffectScope::Single,
            },
            Effect::Unsuspect {
                target: creature(),
                scope: crate::types::ability::EffectScope::Single,
            },
        ] {
            let pos = cond(ra(status), is_suspected());
            assert!(
                !conflicts(&pos, &se()),
                "idempotent status designation × source read ⇒ clean"
            );
        }
        // NEG control: an effect that DOES write `Other` (Goad) × the same source
        // read ⇒ `Other` conflicts with any read ⇒ conflict.
        let neg = cond(ra(Effect::Goad { target: creature() }), is_suspected());
        assert!(
            conflicts(&neg, &se()),
            "an `Other` write × source read ⇒ conflict"
        );
    }

    /// §L13 (CR 506.4c + CR 614.10): removing from combat / skipping a step or turn
    /// is an idempotent SEQUENCING write (`TurnStructure`) observed by no profiled
    /// read — not the maximal-conservative fallback (Gustcloak Savior / Lost in the
    /// Woods / Time Bends). `TurnStructure` only self-conflicts.
    #[test]
    fn l13_remove_from_combat_and_skips_are_turn_structure() {
        for eff in [
            Effect::RemoveFromCombat {
                target: TargetFilter::Any,
            },
            Effect::SkipNextStep {
                target: TargetFilter::Controller,
                step: crate::types::ability::StepSkipTarget::CombatPhase,
                count: qfix(1),
                scope: crate::types::ability::SkipScope::NextOccurrence,
            },
            Effect::SkipNextTurn {
                target: TargetFilter::Controller,
                count: qfix(1),
            },
        ] {
            let p = ability_rw_profile(&ra(eff.clone()));
            assert!(
                p.writes_external.turn_structure,
                "{eff:?} is a TurnStructure write"
            );
            assert!(
                !p.reads_board.object_pt,
                "{eff:?} is not the conservative fallback"
            );
        }
        // POS: RemoveFromCombat (TurnStructure write) × a live board ObjectPt read ⇒
        // TurnStructure feeds no ObjectPt read ⇒ clean.
        let pos = cond(
            ra(Effect::RemoveFromCombat {
                target: TargetFilter::Any,
            }),
            AbilityCondition::TargetMatchesFilter {
                filter: creature(),
                use_lki: false,
                subject_slot: None,
            },
        );
        assert!(
            !conflicts(&pos, &batch()),
            "TurnStructure write × board ObjectPt read ⇒ clean"
        );
        // NEG: the dormant self-conflict row — a (hand-built) TurnStructure READ ×
        // a TurnStructure write DOES feed (fail-closed for any future sequencing read).
        let mut self_conflict = RwProfile::empty();
        self_conflict.reads_board.set(StateKind::TurnStructure);
        self_conflict.writes_external.set(StateKind::TurnStructure);
        assert!(
            profiles_conflict(&self_conflict, &batch()),
            "TurnStructure read × write ⇒ conflict"
        );
    }

    /// §L14 (CR 500 + CR 500.8): extra turns / additional phases are `TurnStructure`
    /// sequencing writes (two extra turns commute), not the `Other` catch-all that
    /// falsely conflicted with a co-occurring source read (Lighthouse Chronologist /
    /// Second Chance / Regenerations Restored / Time Bends).
    #[test]
    fn l14_extra_turn_and_phase_are_turn_structure_not_other() {
        // AdditionalPhase is likewise a TurnStructure write, never `Other`.
        let ap = ability_rw_profile(&ra(Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: crate::types::phase::Phase::PostCombatMain,
            after: crate::types::phase::Phase::PostCombatMain,
            followed_by: vec![],
            count: qfix(1),
            attacker_restriction: None,
        }));
        assert!(
            ap.writes_external.turn_structure,
            "AdditionalPhase is a TurnStructure write"
        );
        assert!(
            !ap.writes_external.other,
            "AdditionalPhase is not the `Other` catch-all"
        );

        // A source counter read co-occurring with the sequencing write.
        let extra_turn = || {
            cond(
                ra(Effect::ExtraTurn {
                    target: TargetFilter::Controller,
                }),
                qcheck(counters_src(), 3),
            )
        };
        // POS: ExtraTurn (TurnStructure) × source counter read ⇒ no feed ⇒ clean.
        assert!(
            !conflicts(&extra_turn(), &se()),
            "TurnStructure write × source counter read ⇒ clean"
        );
        // NEG: the SAME read × an `Other` write (Goad) ⇒ conflict — proving
        // TurnStructure is not `Other`.
        let other = cond(
            ra(Effect::Goad { target: creature() }),
            qcheck(counters_src(), 3),
        );
        assert!(
            conflicts(&other, &se()),
            "`Other` write × source counter read ⇒ conflict"
        );
    }

    /// §voltstorm (CR 700.2): a modal resolves ONE mode; the classifier descends
    /// every `mode_abilities` entry as a UNION rather than falling to
    /// `conservative()`. If the union has no feed, no single choice (and no
    /// cross-choice sibling pair) is order-observable (Voltstorm Angel).
    #[test]
    fn voltstorm_mode_abilities_descend_as_union() {
        // Base effect is inert (PairWith{Self} ⇒ empty profile), isolating the union.
        let base = || Effect::PairWith {
            target: TargetFilter::SelfRef,
        };
        // POS: all modes independent (a life gain and a draw — writes, no reads) ⇒
        // union has no feed ⇒ clean.
        let mut pos = ra(base());
        pos.mode_abilities = vec![
            def(gain_life(qfix(1))),
            def(Effect::Draw {
                count: qfix(1),
                target: TargetFilter::Controller,
            }),
        ];
        assert!(
            !conflicts(&pos, &batch()),
            "independent modes ⇒ union clean"
        );
        // NEG: one mode writes counters on creatures, another reads a board
        // ObjectPt ⇒ the UNION feeds (counters change P/T) ⇒ conflict.
        let mut reader = def(gain_life(qfix(1)));
        reader.condition = Some(AbilityCondition::TargetMatchesFilter {
            filter: creature(),
            use_lki: false,
            subject_slot: None,
        });
        let mut neg = ra(base());
        neg.mode_abilities = vec![def(put_counter_all(qfix(1), creature())), reader];
        assert!(
            conflicts(&neg, &batch()),
            "one mode reads what another mode writes ⇒ union conflict"
        );
    }

    /// The new `StateKind::TurnStructure` kind: `KindSet` add/union/subtract behave,
    /// and the `feeds` matrix isolates it — a sequencing read × sequencing write
    /// conflicts, but it neither feeds nor is fed by `ObjectPt`.
    #[test]
    fn turn_structure_kind_isolated_self_conflict() {
        let ts = KindSet::one(StateKind::TurnStructure);
        assert!(ts.turn_structure);
        assert!(
            KindSet::EMPTY.union(ts).turn_structure,
            "union carries TurnStructure"
        );
        assert!(
            !ts.minus(ts).turn_structure,
            "subtract clears TurnStructure"
        );
        let (nc, nz) = (Census::None, ZoneSpan::None);
        let sg = SpanGate::ungated();
        assert!(
            feeds(ts, ts, &nc, &nc, &nz, &nz, sg),
            "TurnStructure read × write ⇒ self-conflict"
        );
        let pt = KindSet::one(StateKind::ObjectPt);
        assert!(
            !feeds(pt, ts, &nc, &nc, &nz, &nz, sg),
            "TurnStructure write does not feed ObjectPt"
        );
        assert!(
            !feeds(ts, pt, &nc, &nc, &nz, &nz, sg),
            "ObjectPt write does not feed TurnStructure"
        );
    }

    // CR 513.1 + CR 603.7b (Gate-1): `PlayerScope::AnyTurn` reached via a
    // duration walk (`rw_duration` → `rw_player_scope`) is a pure timing
    // referent — it must return a byte-identical profile to the `Controller`
    // scope and must NOT hit a stranded `unreachable!()`. A future card with
    // "until the next end step" on a duration-walked effect (GenericEffect,
    // pump, …) would otherwise panic the ordering-parity profiler.
    #[test]
    fn any_turn_duration_scope_profiles_like_controller_no_panic() {
        let any_turn = rw_duration(&Duration::UntilNextStepOf {
            step: crate::types::phase::Phase::End,
            player: PlayerScope::AnyTurn,
        });
        let controller = rw_duration(&Duration::UntilNextStepOf {
            step: crate::types::phase::Phase::End,
            player: PlayerScope::Controller,
        });
        assert!(
            !any_turn.reads_member_bound && !any_turn.reads_event_live,
            "AnyTurn is a pure timing referent — no state read"
        );
        assert_eq!(
            any_turn.reads_member_bound, controller.reads_member_bound,
            "AnyTurn and Controller durations profile identically"
        );
    }

    // ---- test-local helper ----
    fn def(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, effect)
    }

    // ===================== PR-6.75 S-7 per-arm span pins =====================
    // Shape-level pins on each refined extraction arm's span field, paired
    // positive/negative on the arm's own axis. These SUPPORT the S1–S6
    // production-path discriminators (which drive `group_is_order_independent`).

    fn discard(count: QuantityExpr, target: TargetFilter) -> Effect {
        Effect::Discard {
            count,
            target,
            unless_filter: None,
            filter: None,
            selection: crate::types::ability::CardSelectionMode::Chosen,
        }
    }
    fn handsize(player: PlayerScope) -> QuantityExpr {
        qref(QuantityRef::HandSize { player })
    }
    fn opp_scope() -> PlayerScope {
        PlayerScope::Opponent {
            aggregate: crate::types::ability::AggregateFunction::Min,
        }
    }
    fn scoped(mut a: ResolvedAbility, pf: PlayerFilter) -> ResolvedAbility {
        a.player_scope = Some(pf);
        a
    }
    fn search_own() -> Effect {
        Effect::SearchLibrary {
            source_zones: vec![Zone::Library],
            filter: creature(),
            count: qfix(1),
            reveal: false,
            target_player: None,
            selection_constraint: crate::types::ability::SearchSelectionConstraint::None,
            split: None,
        }
    }
    fn change_zone_under(target: TargetFilter, enters_under: Option<ControllerRef>) -> Effect {
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            target,
            owner_library: false,
            enter_transformed: false,
            enters_under,
            enter_tapped: crate::types::zones::EtbTapState::default(),
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        }
    }
    fn bounce(target: TargetFilter) -> Effect {
        Effect::Bounce {
            target,
            destination: None,
            selection: crate::types::ability::BounceSelection::default(),
        }
    }

    #[test]
    fn s7_handsize_read_span_player_axis() {
        // §4.3.1: HandSize player axis → reads_player_span (You vs Opponents).
        let you = ability_rw_profile(&ra(Effect::Draw {
            count: handsize(PlayerScope::Controller),
            target: TargetFilter::Controller,
        }));
        assert_eq!(you.reads_player_span, PlayerSpan::You);
        let opp = ability_rw_profile(&ra(Effect::Draw {
            count: handsize(opp_scope()),
            target: TargetFilter::Controller,
        }));
        assert_eq!(opp.reads_player_span, PlayerSpan::Opponents);
    }

    #[test]
    fn s7_discard_write_span_scope_threading() {
        // §4.3.3: scoped-Opponent Discard ⇒ Opponents; unscoped, target Controller ⇒ You.
        let scoped_opp = scoped(
            ra(discard(qfix(1), TargetFilter::Controller)),
            PlayerFilter::Opponent,
        );
        assert_eq!(
            ability_rw_profile(&scoped_opp).writes_player_span,
            PlayerSpan::Opponents
        );
        let you = ra(discard(qfix(1), TargetFilter::Controller));
        assert_eq!(ability_rw_profile(&you).writes_player_span, PlayerSpan::You);
    }

    #[test]
    fn s7_discard_fused_pattern_match_and_non_match() {
        // §4.4: fused count HandSize{ScopedPlayer} + target Controller ⇒
        // reads_player_fused, NOT reads_player.
        let fused = ability_rw_profile(&ra(discard(
            handsize(PlayerScope::ScopedPlayer),
            TargetFilter::Controller,
        )));
        assert!(fused.reads_player_fused.hand_library);
        assert!(!fused.reads_player.hand_library);
        // Non-fused: a genuine opp-hand count ⇒ reads_player, NOT fused.
        let plain = ability_rw_profile(&ra(discard(
            handsize(opp_scope()),
            TargetFilter::Controller,
        )));
        assert!(plain.reads_player.hand_library);
        assert!(!plain.reads_player_fused.hand_library);
        // Non-fused: HandSize{ScopedPlayer} but target NOT Controller ⇒ not fused.
        let wrong_target = ability_rw_profile(&ra(discard(
            handsize(PlayerScope::ScopedPlayer),
            TargetFilter::Any,
        )));
        assert!(!wrong_target.reads_player_fused.hand_library);
        assert!(wrong_target.reads_player.hand_library);
    }

    #[test]
    fn s7_search_change_zone_chain_fact() {
        // §4.3.5/6: Search(own library) → ChangeZone{Library→Bf, Any} ⇒ You.
        let present = ra(search_own()).sub_ability(ra(change_zone_under(TargetFilter::Any, None)));
        assert_eq!(
            ability_rw_profile(&present).writes_membership_external_ctrl,
            PlayerSpan::You
        );
        // Explicit enters_under Opponent conflicts with the search's You ⇒ Any.
        let opp_entry = ra(search_own()).sub_ability(ra(change_zone_under(
            TargetFilter::Any,
            Some(ControllerRef::Opponent),
        )));
        assert_eq!(
            ability_rw_profile(&opp_entry).writes_membership_external_ctrl,
            PlayerSpan::Any
        );
        // Absent chain (bare ChangeZone, no search, no enters_under) ⇒ Any (fail-closed).
        let absent = ability_rw_profile(&ra(change_zone_under(TargetFilter::Any, None)));
        assert_eq!(absent.writes_membership_external_ctrl, PlayerSpan::Any);
    }

    #[test]
    fn s7_self_bounce_hand_span() {
        // §4.3.4: a self hand-move (Bounce{SelfRef}→hand) ⇒ You; an external bounce
        // leaves the hand write unrefined (None ⇒ effective-Any at conflict time).
        assert_eq!(
            ability_rw_profile(&ra(bounce(TargetFilter::SelfRef))).writes_player_span,
            PlayerSpan::You
        );
        assert_eq!(
            ability_rw_profile(&ra(bounce(creature()))).writes_player_span,
            PlayerSpan::None
        );
    }
}
