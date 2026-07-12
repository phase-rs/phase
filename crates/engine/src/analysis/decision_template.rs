//! CR 732.2a: a shortcut is "a sequence of game choices, for all players, that may
//! be legally taken based on the current game state and the predictable results."
//! A [`DecisionTemplate`] captures that sequence so it can be replayed verbatim when a
//! simultaneous-trigger group recurs (CR 603.3b ordering) or driven across loop
//! iterations as a predictable shortcut (CR 732.2a). PURELY ADDITIVE / offline —
//! never called from the reducer in this phase.

use crate::types::game_state::{GameState, WaitingFor, YieldTarget};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaType;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use serde::{Deserialize, Serialize};
// NOTE: `matches_target_filter`/`FilterContext`/`TargetFilter` are NOT imported — their
// only consumer, `TargetSchedule::IndexedClass`, is deferred to Phase 4/B3 (RULED
// Deferral 2). `ResourceAxis` is likewise not imported — its only consumer,
// `IterationCount::UntilResource`, is deferred out of Phase 1 (reviewer G7).

/// REUSED verbatim from the priority-yield subsystem. CR 117.3d is the priority-pass
/// *provenance* of the [`YieldTarget`] type ("…the next player in turn order receives
/// priority") — it is NOT an object-identity rule; CR 400.7 is the object-*identity*
/// rule the matcher actually enforces. `ThisObject{source_id,incarnation}` binds one
/// incarnation (a re-entered permanent bumps `incarnation` and stops matching —
/// CR 400.7); `AllCopies{card_id}` binds card identity (survives token death
/// CR 704.5d, matches new copies). For loops minting fresh tokens each cycle prefer the
/// `AllCopies` arm — ObjectId+incarnation churn every iteration, card identity does not.
pub type DecisionSource = YieldTarget;

/// 0-based iteration index within a `Scheduled` replay. CR 732.2a: the schedule is a
/// pure function of THIS value (never of a prior iteration's outcome).
pub type IterationIndex = u32;

/// CR 603.3b (TriggerOrdering) / CR 732.2a (LoopChoice): which decision family a
/// template captures. The `key` discriminant that lets one `decision_templates` Vec
/// hold both the trigger-order templates B2 consults and the loop-choice templates
/// B3/B5 will add, so the gate can filter to `TriggerOrdering` only. `LoopChoice` has
/// no Phase-2 consumer (reserved), but the FILTER it enables is load-bearing now (the
/// gate must ignore non-ordering templates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DecisionKind {
    TriggerOrdering,
    LoopChoice,
}

/// Order-insensitive identity of a recurring decision group. `sources` is stored
/// **sorted + coalesced** (canonical `(identity, multiplicity)` multiset) so equality
/// and dedup are order-independent — requires `Ord` on [`DecisionSource`]. A group
/// "recurs" (and a shrinking deferred tail still matches) when its source multiset is a
/// **sub-multiset** of a template's `sources` — see [`DecisionGroupKey::covers`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DecisionGroupKey {
    /// Canonical `(identity, multiplicity)` pairs, sorted ascending by identity.
    pub sources: Vec<(DecisionSource, u8)>,
    pub kind: DecisionKind,
}

impl DecisionGroupKey {
    /// Coalesce a raw per-trigger source list into canonical sorted
    /// `(identity, multiplicity)` form. One source firing N triggers becomes one
    /// `(source, N)` pair, so both ordering and duplicate-fire are order-independent.
    pub fn from_sources(sources: &[DecisionSource], kind: DecisionKind) -> Self {
        Self {
            sources: coalesce_sources(sources),
            kind,
        }
    }

    /// Sub-multiset test: every `(source, mult)` in `group` has multiplicity ≤ this
    /// key's multiplicity for the same source. A shrinking deferred suffix (⊆ the full
    /// batch) therefore stays covered. Exact-identity match — registration and matching
    /// build each [`DecisionSource`] from the same `(source_id, incarnation)` / `card_id`,
    /// so no incarnation wildcard is needed (a batch never changes a source's
    /// incarnation mid-flight).
    pub fn covers(&self, group: &[DecisionSource]) -> bool {
        coalesce_sources(group).iter().all(|(src, need)| {
            self.sources
                .iter()
                .find(|(s, _)| s == src)
                .is_some_and(|(_, have)| have >= need)
        })
    }

    /// EPHEMERAL (the per-batch CR 603.3b coverage marker) iff every source is a
    /// `ThisObject` incarnation. Mid-batch only; cleared before the next Priority frame.
    pub fn is_ephemeral(&self) -> bool {
        !self.sources.is_empty()
            && self
                .sources
                .iter()
                .all(|(s, _)| matches!(s, YieldTarget::ThisObject { .. }))
    }

    /// PERSISTENT (a saved player-ordering preference, CR 704.5d) iff every source is
    /// an `AllCopies` card identity. Survives across batches and loop iterations.
    pub fn is_persistent(&self) -> bool {
        !self.sources.is_empty()
            && self
                .sources
                .iter()
                .all(|(s, _)| matches!(s, YieldTarget::AllCopies { .. }))
    }
}

/// Sort + coalesce duplicate identities into `(identity, multiplicity)` pairs.
fn coalesce_sources(sources: &[DecisionSource]) -> Vec<(DecisionSource, u8)> {
    let mut sorted: Vec<DecisionSource> = sources.to_vec();
    sorted.sort();
    let mut out: Vec<(DecisionSource, u8)> = Vec::new();
    for s in sorted {
        match out.last_mut() {
            Some((prev, count)) if *prev == s => *count += 1,
            _ => out.push((s, 1)),
        }
    }
    out
}

/// CR 732.2a: the captured player decisions for one recurring decision group.
/// `key` is the order-insensitive identity B2 looks the template up by (its
/// `kind` selects trigger-ordering vs loop-choice; its `sources` multiset is the
/// coverage marker the gate matches a recurring group against).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionTemplate {
    pub owner: PlayerId,
    /// Pins in the group's canonical decision order.
    pub decisions: Vec<PinnedDecision>,
    pub replay: ReplayMode,
    pub key: DecisionGroupKey,
}

/// Identifies one free choice within a group: which source raised it (CR 400.7-stable
/// [`DecisionSource`]) plus a sub-index disambiguating multiple choices from one source
/// (e.g. two target slots on one ability). `PartialEq`/`Eq` only — the
/// [`predictability_gate`] matches slots by equality, and `DecisionSource = YieldTarget`
/// carries no `Ord` derive in Phase 1 (RULED Deferral 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionSlot {
    pub source: DecisionSource,
    pub index: u8,
}

/// CR 603.5: whether a "may" pin takes the optional action or declines it. Typed (not `bool`)
/// so both outcomes are self-documenting at every construction and match site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MayChoiceOption {
    Take,
    Decline,
}

/// CR 732.6: whether an "[A] unless [B]" pin pays [B] to break the loop, or declines and takes [A].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnlessPaymentOption {
    Pay,
    Decline,
}

/// One pinned decision. Variants are distinct CR choice KINDS (ordering / targeting /
/// modal / optional-"may" / "[A] unless [B]" break), not a parameterization axis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PinnedDecision {
    /// CR 603.3b: place this source's trigger at ordering position `pos`.
    Order { source: DecisionSource, pos: u8 },
    /// CR 608.2b: targets for a slot; each re-resolved to a live legal ObjectId per
    /// iteration.
    Targets {
        slot: DecisionSlot,
        targets: Vec<TargetPin>,
    },
    /// CR 700.2 modal: chosen mode indices (mirrors `GameAction::SelectModes.indices`).
    Mode {
        slot: DecisionSlot,
        indices: Vec<usize>,
    },
    /// CR 603.5 / a "may" effect: take the optional action or not.
    MayChoice {
        slot: DecisionSlot,
        take: MayChoiceOption,
    },
    /// CR 732.6: pay or decline an "[A] unless [B]" break.
    UnlessBreak {
        slot: DecisionSlot,
        pay: UnlessPaymentOption,
    },
    /// CR 601.2h + CR 702.51a/b: pay a convoke `ManaPayment` by tapping the minimal
    /// deterministic set of untapped creatures matching the live post-affinity color
    /// requirement. State-independent: the concrete creatures are re-bound LIVE each
    /// iteration (canonical order — lowest ObjectId per needed color) via
    /// `select_convoke_taps`, a pure function of (live legal untapped set, locked cost)
    /// per CR 732.2a — so no per-iteration creature is latched here.
    ConvokeTaps { slot: DecisionSlot },
}

/// CR 732.2a: the READ-side decision schema an interactive loop-shortcut OFFER exposes so the
/// frontend can render the open choices + collect pins. 1:1 read-side dual of the write-side
/// `Vec<PinnedDecision>` (the FE picks from each point's legal set → a pin). Every field is
/// derived from board state the offer recipient may legally see; hidden-info legal targets are
/// redacted for other viewers in `game::visibility::filter_state_for_viewer`. Snapshotted at
/// offer construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutDecisionSchema {
    /// CR 732.1b: the proposed repeat mode. `UntilLethal` for a determinate CR 704.5a /
    /// CR 704.5c drain; `Fixed(n)` seeds the frontend count picker for an optional loop.
    pub iteration_count: IterationCount,
    /// The open per-iteration decision-points needing pins. EMPTY for a choice-free drain.
    pub points: Vec<DecisionPoint>,
    /// CR 702.51a: total untapped creatures the controller may tap for convoke across every
    /// `ConvokeTaps` point — engine-owned so the frontend renders the count directly instead of
    /// re-deriving it (display-layer purity). Sum of each `ConvokeTaps.tappable.len()`.
    #[serde(default)]
    pub convoke_tappable_count: usize,
}

// CR 732.2a: `IterationCount` carries no `Default` and its `Fixed(u32)` is a tuple variant
// (so a derived `#[default]` cannot apply) — hand-impl the forward-compat deser default the
// `#[serde(default)]` on `WaitingFor::LoopShortcut.schema` needs.
impl Default for ShortcutDecisionSchema {
    fn default() -> Self {
        Self {
            iteration_count: IterationCount::Fixed(0),
            points: Vec::new(),
            convoke_tappable_count: 0,
        }
    }
}

/// One open decision-point. `slot` is the same [`DecisionSlot`] the frontend echoes on the
/// [`PinnedDecision`] it produces; `kind` carries that decision's legal option set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionPoint {
    pub slot: DecisionSlot,
    pub kind: DecisionPointKind,
}

/// Legal option set for one decision-point. EXHAUSTIVE, wildcard-free 1:1 read-side peer of
/// the loop-declaration [`PinnedDecision`] variants (`Order` is CR 603.3b trigger-ordering,
/// not a loop-declaration choice — it has no read-side peer). Externally tagged → FE-consumable
/// JSON (`{"ConvokeTaps":{"tappable":[..]}}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionPointKind {
    /// CR 608.2b: the legal targets for the slot (native `find_legal_targets` output).
    Targets {
        legal_targets: Vec<crate::types::ability::TargetRef>,
    },
    /// CR 702.51a: untapped creatures the controller may tap for convoke (informational — the
    /// concrete taps are re-bound live by `select_convoke_taps`).
    ConvokeTaps { tappable: Vec<ObjectId> },
    /// CR 700.2 modal: the selectable mode indices.
    Mode { available_modes: Vec<usize> },
    /// CR 603.5: a binary "may" — the slot alone identifies it (FE renders yes/no).
    MayChoice,
    /// CR 732.6: a binary "[A] unless [B]" break — pay or decline.
    UnlessBreak,
}

/// A pinned target. `ByIdentity` re-resolves to a live legal ObjectId each iteration
/// (CR 608.2b); `Scheduled` is an iteration-indexed pure function (CR 732.2a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetPin {
    ByIdentity(DecisionSource),
    Player(PlayerId),
    Scheduled(TargetSchedule),
}

/// CR 732.2a: how the pins are replayed. `Static` (ordering) ignores the iteration
/// index; `Scheduled` (loop shortcut) makes every choice a pure function of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplayMode {
    Static,
    Scheduled { count: IterationCount },
}

/// CR 732.2a "a loop that repeats a specified number of times". **Phase 1 ships ONLY
/// `Fixed`** (reviewer G7): nothing in Phase 1 reads the count — [`resolve`] takes an
/// explicit `iteration` index, and the count-driven loop that consumes it is Phase 3 /
/// Part A. The count-terminated variants (`UntilLethal` → CR 704.5a "a player with 0 or
/// less life loses"; `UntilResource(ResourceAxis, i64)`) are deferred to the phase that
/// adds their driver, so the shipped surface stays minimal and fully tested. The enum is
/// kept (rather than a bare `u32` field on `Scheduled`) so Phase 3 adds those variants
/// without a field-type change at any `Scheduled` construction site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IterationCount {
    Fixed(u32),
    /// CR 704.5a + CR 732.1b: repeat until a player is at 0-or-less life — the driver
    /// PR-7 Phase 3 adds for the interactive loop-shortcut of a determinate lethal drain.
    /// The terminating condition is the SBA, not a caller-supplied count. (`UntilResource`
    /// stays deferred to Phase 4/B5.)
    UntilLethal,
}

/// CR 732.2a "predictable results / no conditional actions": deterministic,
/// iteration-indexed target variation. EVERY variant is a pure function of
/// (iteration index, live legal set) — NEVER of a prior iteration's OUTCOME. That is
/// enforced BY CONSTRUCTION: no variant carries any prior-outcome/event input, so a
/// "react to what happened" target is unrepresentable (this is what collapses the
/// predictability gate's "no conditional" clause into "total coverage").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetSchedule {
    Constant(DecisionSource),
    RoundRobin(Vec<DecisionSource>),
    /// Pre-declared switch-over: identity for [start, next-start). The switch point is
    /// FIXED IN ADVANCE (not triggered by an in-loop event), keeping it 732.2a-predictable.
    Piecewise(Vec<(u32, DecisionSource)>),
    // NOTE (RULED Deferral 2): `IndexedClass { filter: TargetFilter, stride: i32 }` — an
    // iteration-indexed pick from an object class, evaluated via `matches_target_filter`
    // — is deferred to Phase 4/B3, where a live `FilterContext` source exists.
    // `FilterContext::neutral()` silently mis-evaluates Opponent/controller-scoped
    // filters, so shipping it now is a footgun; its real consumer is B3's "bounce
    // successive cards to hand". Deferring it keeps `evaluate_schedule` free of any
    // `filter.rs` dependency in Phase 1.
}

/// A pin resolved to concrete live values for one iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConcreteDecision {
    Order {
        source: ObjectId,
        pos: u8,
    },
    Targets {
        slot: DecisionSlot,
        targets: Vec<ConcreteTarget>,
    },
    Mode {
        slot: DecisionSlot,
        indices: Vec<usize>,
    },
    MayChoice {
        slot: DecisionSlot,
        take: MayChoiceOption,
    },
    UnlessBreak {
        slot: DecisionSlot,
        pay: UnlessPaymentOption,
    },
    /// CR 601.2h + CR 702.51a/b: the live-resolved convoke tap-set for this iteration —
    /// `(creature, mana_type)` pairs to feed as `GameAction::TapForConvoke`. Re-bound each
    /// iteration by `select_convoke_taps` (lowest-ObjectId-per-color canonical order).
    ConvokeTaps {
        slot: DecisionSlot,
        creatures: Vec<(ObjectId, ManaType)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcreteTarget {
    Object(ObjectId),
    Player(PlayerId),
}

/// Why a replay could not produce a legal concrete decision this iteration. **Selection
/// is by PIN KIND, never by `ReplayMode`** (reviewer G2): a `Static`-mode template can
/// carry `Targets` pins (an ordered AND targeted trigger), so the failure kind is chosen
/// by which pin/target is being resolved, independent of whether the template is
/// `Static` or `Scheduled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayFailure {
    /// CR 608.2b: a TARGET pin (`Targets`'s `ByIdentity`, or a `Scheduled` schedule) no
    /// longer resolves to a legal live object (left its zone / ceased to exist). Raised
    /// whenever a *target* is illegal-or-absent, in ANY `ReplayMode` — a `Static`-mode
    /// `Targets` pin with a removed target yields THIS, not `MissingSource`. ⇒ abort the
    /// auto-shortcut, hand back to manual.
    IllegalTarget {
        slot: DecisionSlot,
        source: DecisionSource,
    },
    /// CR 400.7: an ORDER pin's source (`Order`) is absent from the current battlefield
    /// ⇒ the ordering template no longer matches ⇒ fall through to a normal manual
    /// prompt. Raised ONLY for the `Order` pin kind, in any `ReplayMode`.
    MissingSource { source: DecisionSource },
    /// A `RoundRobin`/`Piecewise` schedule has no entry covering this iteration index.
    ScheduleExhausted { slot: DecisionSlot },
    /// CR 702.51b: no legal untapped-creature tap-set covers the live convoke
    /// requirement (the post-affinity locked cost can't be paid by the available
    /// untapped creatures + pool) ⇒ abort the auto-shortcut, hand back to manual.
    UnpayableConvoke { slot: DecisionSlot },
}

/// CR 732.2a + CR 608.2b: resolve every pin to concrete live values for `iteration`.
/// PURE — reads `state`, mutates nothing, dispatches nothing. Iterates
/// `template.decisions` and resolves EACH pin by its OWN kind; **the failure kind is a
/// function of the pin/target kind, NOT of `template.replay`** (reviewer G2).
/// `template.replay` is caller-facing metadata only (`Static` = replay this ordering
/// identically, `iteration` ignored by the pins it carries; `Scheduled { count }` =
/// caller drives `iteration` over `0..count`) and is NOT consulted for failure selection
/// here.
pub fn resolve(
    template: &DecisionTemplate,
    iteration: IterationIndex,
    state: &GameState,
) -> Result<Vec<ConcreteDecision>, ReplayFailure> {
    template
        .decisions
        .iter()
        .map(|pin| resolve_pin(pin, iteration, state))
        .collect()
}

/// Resolve one pin. The failure kind is selected HERE by the pin kind (G2): an `Order`
/// source that is absent yields `MissingSource` (CR 400.7); an absent target yields
/// `IllegalTarget` (CR 608.2b) — the SAME missing identity, different failure, chosen by
/// where it sits, not by `ReplayMode`.
fn resolve_pin(
    pin: &PinnedDecision,
    iteration: IterationIndex,
    state: &GameState,
) -> Result<ConcreteDecision, ReplayFailure> {
    match pin {
        // CR 603.3b: replay this source's trigger at its pinned ordering position. The
        // source must still be on the battlefield or the ordering template no longer
        // matches (CR 400.7).
        PinnedDecision::Order { source, pos } => {
            let id = resolve_source(source, state).ok_or_else(|| ReplayFailure::MissingSource {
                source: source.clone(),
            })?;
            Ok(ConcreteDecision::Order {
                source: id,
                pos: *pos,
            })
        }
        // CR 608.2b: re-resolve each target to a live legal object THIS iteration.
        PinnedDecision::Targets { slot, targets } => {
            let concrete = targets
                .iter()
                .map(|t| resolve_target(t, slot, iteration, state))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ConcreteDecision::Targets {
                slot: slot.clone(),
                targets: concrete,
            })
        }
        // CR 700.2 / CR 603.5 / CR 732.6: pure recorded choices — no object to resolve,
        // no per-iteration legality re-check, copied straight through.
        PinnedDecision::Mode { slot, indices } => Ok(ConcreteDecision::Mode {
            slot: slot.clone(),
            indices: indices.clone(),
        }),
        PinnedDecision::MayChoice { slot, take } => Ok(ConcreteDecision::MayChoice {
            slot: slot.clone(),
            take: *take,
        }),
        PinnedDecision::UnlessBreak { slot, pay } => Ok(ConcreteDecision::UnlessBreak {
            slot: slot.clone(),
            pay: *pay,
        }),
        // CR 601.2h + CR 702.51a/b: re-bind the convoke tap-set LIVE against this
        // iteration's board. The caster + locked remaining cost come from the live
        // `ManaPayment` prompt (CR 601.2f cost-lock); the single-authority selector
        // `select_convoke_taps` picks the minimal deterministic set (lowest ObjectId
        // per needed color). No legal set ⇒ `UnpayableConvoke` (CR 702.51b).
        PinnedDecision::ConvokeTaps { slot } => {
            let (player, cost) = match (&state.waiting_for, state.pending_cast.as_ref()) {
                (WaitingFor::ManaPayment { player, .. }, Some(pending)) => {
                    (*player, pending.cost.clone())
                }
                _ => return Err(ReplayFailure::UnpayableConvoke { slot: slot.clone() }),
            };
            match crate::game::mana_payment::select_convoke_taps(state, player, &cost) {
                Some(creatures) => Ok(ConcreteDecision::ConvokeTaps {
                    slot: slot.clone(),
                    creatures,
                }),
                None => Err(ReplayFailure::UnpayableConvoke { slot: slot.clone() }),
            }
        }
    }
}

/// Resolve one target pin. CR 608.2b: a by-identity or scheduled target must still be a
/// legal live object; an absent one is `IllegalTarget`.
fn resolve_target(
    pin: &TargetPin,
    slot: &DecisionSlot,
    iteration: IterationIndex,
    state: &GameState,
) -> Result<ConcreteTarget, ReplayFailure> {
    match pin {
        TargetPin::ByIdentity(source) => resolve_source(source, state)
            .map(ConcreteTarget::Object)
            .ok_or_else(|| ReplayFailure::IllegalTarget {
                slot: slot.clone(),
                source: source.clone(),
            }),
        TargetPin::Player(p) => Ok(ConcreteTarget::Player(*p)),
        TargetPin::Scheduled(sched) => evaluate_schedule(sched, slot, iteration, state),
    }
}

/// Re-bind a stored `DecisionSource` to a live battlefield `ObjectId`. The battlefield
/// analogue of `GameState::is_priority_yielded`'s matching arms. KIND-AGNOSTIC: returns
/// `None` on no match, and the CALLER maps that to the pin-kind-appropriate
/// `ReplayFailure` (`Order` ⇒ `MissingSource`, a target ⇒ `IllegalTarget`) — the single
/// seam where G2's per-pin-kind failure selection is realized.
pub(crate) fn resolve_source(src: &DecisionSource, state: &GameState) -> Option<ObjectId> {
    match src {
        // CR 400.7: bind ONE incarnation — a re-entered permanent bumps `incarnation`
        // and stops matching. A `None` incarnation matches an object that latched none
        // (synthetic/delayed), mirroring `is_priority_yielded`'s `Option == Option`.
        YieldTarget::ThisObject {
            source_id,
            incarnation,
            ..
        } => state
            .objects
            .get(source_id)
            .filter(|o| o.zone == Zone::Battlefield)
            .filter(|o| incarnation.is_none() || *incarnation == Some(o.incarnation))
            .map(|o| o.id),
        // CR 704.5d: bind CARD identity — survives a token source ceasing to exist and
        // matches any live copy. Choose the lowest `ObjectId` deterministically (the
        // inner `u64` is public; no `Ord` derive) so replay is reproducible even though
        // `im::HashMap` iteration order is not.
        YieldTarget::AllCopies { card_id, .. } => state
            .objects
            .values()
            .filter(|o| o.zone == Zone::Battlefield && o.card_id == *card_id)
            .min_by_key(|o| o.id.0)
            .map(|o| o.id),
    }
}

/// CR 732.2a predictability firewall: EXHAUSTIVE `match` over [`TargetSchedule`] with NO
/// wildcard arm — a future outcome-carrying variant breaks this build (mirrored by the
/// `target_schedule_predictability_firewall_is_exhaustive` test). Every variant is a
/// pure fn of (iteration index, live set); each selects a `DecisionSource`, then
/// re-binds it to a live legal object (CR 608.2b, via `resolve_source`).
fn evaluate_schedule(
    sched: &TargetSchedule,
    slot: &DecisionSlot,
    iter: IterationIndex,
    state: &GameState,
) -> Result<ConcreteTarget, ReplayFailure> {
    let source: &DecisionSource = match sched {
        TargetSchedule::Constant(src) => src,
        TargetSchedule::RoundRobin(schedule) => {
            if schedule.is_empty() {
                return Err(ReplayFailure::ScheduleExhausted { slot: slot.clone() });
            }
            &schedule[iter as usize % schedule.len()]
        }
        TargetSchedule::Piecewise(schedule) => schedule
            .iter()
            .filter(|(start, _)| *start <= iter)
            .max_by_key(|(start, _)| *start)
            .map(|(_, src)| src)
            .ok_or_else(|| ReplayFailure::ScheduleExhausted { slot: slot.clone() })?,
    };
    resolve_source(source, state)
        .map(ConcreteTarget::Object)
        .ok_or_else(|| ReplayFailure::IllegalTarget {
            slot: slot.clone(),
            source: source.clone(),
        })
}

/// CR 732.2a firewall: a `Scheduled` template may auto-drive a shortcut only if every
/// free choice in the cycle is pinned (TOTAL COVERAGE). "No conditional on a prior
/// iteration's outcome" needs NO runtime check — it is unrepresentable in
/// [`TargetSchedule`] by construction (see the type doc); a choice a player could only
/// make reactively is one they cannot pin, which surfaces HERE as an unpinned slot.
/// Per-iteration legality (CR 608.2b) is [`resolve`]'s re-check, run for each iteration
/// up to the count by the caller (later phase).
pub fn predictability_gate(
    template: &DecisionTemplate,
    required_slots: &[DecisionSlot],
) -> Result<(), PredictabilityViolation> {
    for slot in required_slots {
        if !template.decisions.iter().any(|pin| &pin_slot(pin) == slot) {
            return Err(PredictabilityViolation::UnpinnedChoice { slot: slot.clone() });
        }
    }
    Ok(())
}

/// The slot a pin addresses. Exhaustive over `PinnedDecision` (no wildcard): an `Order`
/// pin raises exactly one ordering decision per source, addressed by that source at
/// sub-index 0; the other kinds carry an explicit slot.
fn pin_slot(pin: &PinnedDecision) -> DecisionSlot {
    match pin {
        PinnedDecision::Order { source, .. } => DecisionSlot {
            source: source.clone(),
            index: 0,
        },
        PinnedDecision::Targets { slot, .. }
        | PinnedDecision::Mode { slot, .. }
        | PinnedDecision::MayChoice { slot, .. }
        | PinnedDecision::UnlessBreak { slot, .. }
        | PinnedDecision::ConvokeTaps { slot } => slot.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PredictabilityViolation {
    /// CR 732.2a: a cycle choice slot has no matching `PinnedDecision` ⇒ not a
    /// describable predictable sequence ⇒ no auto-resolve.
    UnpinnedChoice { slot: DecisionSlot },
}

/// CR 732.2a + CR 608.2b: why a declared pin is not a LEGAL answer to the offered decision
/// schema. `validate_pins` is the fail-closed VALUE-legality firewall paired with
/// [`predictability_gate`]'s COVERAGE check: the gate proves every offered slot is pinned;
/// this proves every pin's VALUE lies inside the slot's offered legal set. Any violation ⇒
/// the declare handler rejects the shortcut and hands back to manual play (no APNAP, no
/// drive, no crown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinValidation {
    /// The pin addresses a slot the offer never exposed (no matching `DecisionPoint`).
    UnexposedSlot { slot: DecisionSlot },
    /// CR 608.2b: a `Targets` pin resolves to a value outside the slot's offered
    /// `legal_targets` (or fails to resolve to a live legal object at all).
    IllegalPinValue { slot: DecisionSlot },
    /// CR 700.2: a `Mode` pin names an index outside the slot's `available_modes`.
    IllegalModeIndex { slot: DecisionSlot },
}

/// Map a resolved concrete target to its wire-side [`TargetRef`] peer (the read-side
/// schema's `legal_targets` element type).
fn concrete_to_target_ref(t: ConcreteTarget) -> crate::types::ability::TargetRef {
    match t {
        ConcreteTarget::Object(id) => crate::types::ability::TargetRef::Object(id),
        ConcreteTarget::Player(p) => crate::types::ability::TargetRef::Player(p),
    }
}

/// CR 732.2a + CR 608.2b: the fail-closed VALUE-legality firewall for a declared shortcut.
/// Verifies every pin in `template` is a LEGAL answer to `schema` — each pin's slot is one
/// the offer exposed, and each pin's resolved value lies inside that slot's offered legal
/// set. `period` (the drive count from [`shortcut_drive_period`]) bounds the iteration
/// indices a scheduled target pin is re-resolved for, so a `RoundRobin`/`Piecewise` schedule
/// is validated at EVERY index it will drive. EXHAUSTIVE over [`PinnedDecision`] with no
/// wildcard: `Order` (CR 603.3b trigger-ordering) and `ConvokeTaps` (object-growth-internal,
/// re-bound live by `select_convoke_taps`) carry no FE-declared target-legality, so they are
/// SKIPPED explicitly. Runs once at declare (the board is frozen through Accept); the drive's
/// per-iteration [`resolve`] is the runtime CR 608.2b backstop.
pub fn validate_pins(
    schema: &ShortcutDecisionSchema,
    template: &DecisionTemplate,
    period: IterationIndex,
    state: &GameState,
) -> Result<(), PinValidation> {
    for pin in &template.decisions {
        match pin {
            PinnedDecision::Targets { slot, targets } => {
                let point = schema
                    .points
                    .iter()
                    .find(|p| p.slot == *slot)
                    .ok_or_else(|| PinValidation::UnexposedSlot { slot: slot.clone() })?;
                let DecisionPointKind::Targets { legal_targets } = &point.kind else {
                    return Err(PinValidation::IllegalPinValue { slot: slot.clone() });
                };
                // CR 608.2b: re-resolve every target at every driven iteration index and
                // require the concrete value to be an offered legal target. A scheduled pin
                // that cannot resolve to a live legal object is itself an illegal value.
                for t in targets {
                    for i in 0..period.max(1) {
                        let concrete = resolve_target(t, slot, i, state)
                            .map_err(|_| PinValidation::IllegalPinValue { slot: slot.clone() })?;
                        if !legal_targets.contains(&concrete_to_target_ref(concrete)) {
                            return Err(PinValidation::IllegalPinValue { slot: slot.clone() });
                        }
                    }
                }
            }
            PinnedDecision::Mode { slot, indices } => {
                let point = schema
                    .points
                    .iter()
                    .find(|p| p.slot == *slot)
                    .ok_or_else(|| PinValidation::UnexposedSlot { slot: slot.clone() })?;
                let DecisionPointKind::Mode { available_modes } = &point.kind else {
                    return Err(PinValidation::IllegalModeIndex { slot: slot.clone() });
                };
                for idx in indices {
                    if !available_modes.contains(idx) {
                        return Err(PinValidation::IllegalModeIndex { slot: slot.clone() });
                    }
                }
            }
            // CR 603.5 / CR 732.6: binary choices — an exposed matching point is the only
            // legality requirement (the FE renders yes/no; no value set to bound).
            PinnedDecision::MayChoice { slot, .. } | PinnedDecision::UnlessBreak { slot, .. } => {
                if !schema.points.iter().any(|p| p.slot == *slot) {
                    return Err(PinValidation::UnexposedSlot { slot: slot.clone() });
                }
            }
            // CR 603.3b trigger-ordering + CR 702.51a convoke: not FE-declared target
            // legality — skipped explicitly (no wildcard, so a future pin kind build-breaks
            // here rather than silently passing unvalidated).
            PinnedDecision::Order { .. } | PinnedDecision::ConvokeTaps { .. } => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::identifiers::CardId;

    fn this_obj(id: u64, inc: Option<u64>) -> DecisionSource {
        YieldTarget::ThisObject {
            source_id: ObjectId(id),
            incarnation: inc,
            trigger_description: None,
        }
    }

    fn all_copies(card_id: u64) -> DecisionSource {
        YieldTarget::AllCopies {
            card_id: CardId(card_id),
            trigger_description: None,
        }
    }

    /// T6: `DecisionPointKind` serializes externally tagged (`{"ConvokeTaps":{...}}`) — the
    /// FE-consumable JSON shape the WASM bridge passes through — and round-trips equal. Revert:
    /// switching the enum to internal/adjacent tagging changes the top-level key and fails.
    #[test]
    fn decision_point_kind_convoke_taps_serde_shape() {
        let kind = DecisionPointKind::ConvokeTaps {
            tappable: vec![ObjectId(2), ObjectId(5)],
        };
        let json = serde_json::to_value(&kind).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({ "ConvokeTaps": { "tappable": [2, 5] } })
        );
        let back: DecisionPointKind = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, kind);
    }

    /// T6: a full `ShortcutDecisionSchema` carrying a `Targets` point round-trips equal, and the
    /// hand-impl `Default` is the forward-compat deser seed the `#[serde(default)]` needs.
    #[test]
    fn shortcut_decision_schema_round_trips_and_defaults() {
        let schema = ShortcutDecisionSchema {
            iteration_count: IterationCount::UntilLethal,
            points: vec![DecisionPoint {
                slot: DecisionSlot {
                    source: all_copies(7),
                    index: 0,
                },
                kind: DecisionPointKind::Targets {
                    legal_targets: vec![
                        crate::types::ability::TargetRef::Object(ObjectId(3)),
                        crate::types::ability::TargetRef::Player(PlayerId(1)),
                    ],
                },
            }],
            convoke_tappable_count: 2,
        };
        let json = serde_json::to_value(&schema).expect("serialize");
        let back: ShortcutDecisionSchema = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, schema);
        assert_eq!(
            ShortcutDecisionSchema::default(),
            ShortcutDecisionSchema {
                iteration_count: IterationCount::Fixed(0),
                points: vec![],
                convoke_tappable_count: 0,
            }
        );
    }

    /// Phase-1 `resolve`/gate tests don't consult `key`; give every template an empty
    /// `TriggerOrdering` key so the shape compiles.
    fn tri_key() -> DecisionGroupKey {
        DecisionGroupKey {
            sources: vec![],
            kind: DecisionKind::TriggerOrdering,
        }
    }

    /// Insert a battlefield object with the given storage id / card id / incarnation.
    fn bf_object(state: &mut GameState, id: u64, card_id: u64, incarnation: u64) {
        let oid = ObjectId(id);
        let mut o = GameObject::new(
            oid,
            CardId(card_id),
            PlayerId(0),
            "Combo Piece".to_string(),
            Zone::Battlefield,
        );
        o.incarnation = incarnation;
        state.objects.insert(oid, o);
    }

    fn order_source(out: &ConcreteDecision) -> ObjectId {
        match out {
            ConcreteDecision::Order { source, .. } => *source,
            other => panic!("expected Order, got {other:?}"),
        }
    }

    fn targeted_object(out: &ConcreteDecision) -> ObjectId {
        match out {
            ConcreteDecision::Targets { targets, .. } => match targets[0] {
                ConcreteTarget::Object(id) => id,
                ConcreteTarget::Player(_) => panic!("expected an object target"),
            },
            other => panic!("expected Targets, got {other:?}"),
        }
    }

    /// T1: a `Static` template of 3 `Order` pins over 3 battlefield objects replays the
    /// pins IN THE PINNED ORDER, each mapped to its live `ObjectId`. Discriminator: a
    /// different pin order yields a different output vector — output tracks the pinned
    /// order, not a fixed/sorted order.
    #[test]
    fn static_template_reproduces_order() {
        let mut state = GameState::new_two_player(7);
        bf_object(&mut state, 10, 10, 0);
        bf_object(&mut state, 11, 11, 0);
        bf_object(&mut state, 12, 12, 0);

        let template = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![
                PinnedDecision::Order {
                    source: this_obj(12, None),
                    pos: 0,
                },
                PinnedDecision::Order {
                    source: this_obj(10, None),
                    pos: 1,
                },
                PinnedDecision::Order {
                    source: this_obj(11, None),
                    pos: 2,
                },
            ],
            replay: ReplayMode::Static,
            key: tri_key(),
        };
        let out = resolve(&template, 0, &state).expect("all sources live");
        let ids: Vec<ObjectId> = out.iter().map(order_source).collect();
        assert_eq!(
            ids,
            vec![ObjectId(12), ObjectId(10), ObjectId(11)],
            "resolve preserves the pinned decision order and maps each source to its id"
        );
        // pos threads through untouched.
        let poses: Vec<u8> = out
            .iter()
            .map(|d| match d {
                ConcreteDecision::Order { pos, .. } => *pos,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(poses, vec![0, 1, 2]);

        // DISCRIMINATOR: a re-ordered template produces a different output vector.
        let reordered = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![
                PinnedDecision::Order {
                    source: this_obj(10, None),
                    pos: 0,
                },
                PinnedDecision::Order {
                    source: this_obj(11, None),
                    pos: 1,
                },
                PinnedDecision::Order {
                    source: this_obj(12, None),
                    pos: 2,
                },
            ],
            replay: ReplayMode::Static,
            key: tri_key(),
        };
        let ids2: Vec<ObjectId> = resolve(&reordered, 0, &state)
            .unwrap()
            .iter()
            .map(order_source)
            .collect();
        assert_ne!(
            ids, ids2,
            "output order tracks the pinned order, not a fixed/sorted order"
        );
    }

    /// T2: a `RoundRobin([A,B])` schedule cycles A,B,A,B across iterations 0..4, each
    /// re-bound to a live id. Discriminator: iter1 ≠ iter0 (a Constant impl would give
    /// A,A,A,A) and iter2 == iter0 (the cycle wraps).
    #[test]
    fn scheduled_roundrobin_cycles_targets() {
        let mut state = GameState::new_two_player(7);
        bf_object(&mut state, 20, 20, 0);
        bf_object(&mut state, 21, 21, 0);

        let slot = DecisionSlot {
            source: this_obj(99, None),
            index: 0,
        };
        let template = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Targets {
                slot,
                targets: vec![TargetPin::Scheduled(TargetSchedule::RoundRobin(vec![
                    this_obj(20, None),
                    this_obj(21, None),
                ]))],
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(4),
            },
            key: tri_key(),
        };
        let at = |it: u32| targeted_object(&resolve(&template, it, &state).unwrap()[0]);
        assert_eq!(at(0), ObjectId(20));
        assert_eq!(at(1), ObjectId(21));
        assert_eq!(at(2), ObjectId(20));
        assert_eq!(at(3), ObjectId(21));
        assert_ne!(at(1), at(0), "a Constant impl (A,A,A,A) would fail this");
        assert_eq!(at(2), at(0), "the round-robin wraps at len");
    }

    /// T4: a `Piecewise([(0,A),(2,B)])` schedule holds A for iters 0,1 and switches to B
    /// at exactly iter 2. AND a `Piecewise([(1,A)])` with no entry covering iter 0 ⇒
    /// `ScheduleExhausted` — the non-vacuous exhaustion path (formerly exercised by the
    /// deferred T3).
    #[test]
    fn scheduled_piecewise_switches() {
        let mut state = GameState::new_two_player(7);
        bf_object(&mut state, 20, 20, 0);
        bf_object(&mut state, 21, 21, 0);

        let slot = DecisionSlot {
            source: this_obj(99, None),
            index: 0,
        };
        let template = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Targets {
                slot: slot.clone(),
                targets: vec![TargetPin::Scheduled(TargetSchedule::Piecewise(vec![
                    (0, this_obj(20, None)),
                    (2, this_obj(21, None)),
                ]))],
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(4),
            },
            key: tri_key(),
        };
        let at = |it: u32| targeted_object(&resolve(&template, it, &state).unwrap()[0]);
        assert_eq!(at(0), ObjectId(20));
        assert_eq!(at(1), ObjectId(20), "still A just before the switch");
        assert_eq!(at(2), ObjectId(21), "switches to B at exactly iter 2");
        assert_eq!(at(3), ObjectId(21));

        // No entry covers iter 0 (earliest start=1 > 0) ⇒ ScheduleExhausted.
        let uncovered = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Targets {
                slot,
                targets: vec![TargetPin::Scheduled(TargetSchedule::Piecewise(vec![(
                    1,
                    this_obj(20, None),
                )]))],
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(1),
            },
            key: tri_key(),
        };
        assert!(matches!(
            resolve(&uncovered, 0, &state).unwrap_err(),
            ReplayFailure::ScheduleExhausted { .. }
        ));
    }

    /// T5 (G2): a **`Static`**-mode template whose `Targets` `ByIdentity` target has left
    /// the battlefield yields `IllegalTarget` (CR 608.2b), NOT `MissingSource` — proving
    /// failure selection is by PIN KIND, not `ReplayMode` (a mode-keyed impl would emit
    /// `MissingSource` under `Static`). Control (target present) ⇒ Ok.
    #[test]
    fn static_targets_pin_removed_target_yields_illegal_target_608_2b() {
        let src = this_obj(30, Some(1));
        let template = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Targets {
                slot: DecisionSlot {
                    source: src.clone(),
                    index: 0,
                },
                targets: vec![TargetPin::ByIdentity(src)],
            }],
            replay: ReplayMode::Static,
            key: tri_key(),
        };
        // Target absent.
        let absent = GameState::new_two_player(7);
        let err = resolve(&template, 0, &absent).unwrap_err();
        assert!(matches!(err, ReplayFailure::IllegalTarget { .. }));
        assert!(
            !matches!(err, ReplayFailure::MissingSource { .. }),
            "a Static-mode target failure is IllegalTarget (per pin kind), not MissingSource"
        );
        // Control: target present ⇒ Ok (not a silent stale id).
        let mut present = GameState::new_two_player(7);
        bf_object(&mut present, 30, 30, 1);
        assert!(resolve(&template, 0, &present).is_ok());
    }

    /// T5b (G2 sibling): the SAME `Static` mode with an `Order` pin (different pin kind)
    /// whose source is removed yields `MissingSource` (CR 400.7), NOT `IllegalTarget`.
    /// Together T5+T5b prove failure selection is per pin kind, not per mode.
    #[test]
    fn static_order_pin_removed_source_yields_missing_source_400_7() {
        let src = this_obj(40, Some(2));
        let template = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Order {
                source: src,
                pos: 0,
            }],
            replay: ReplayMode::Static,
            key: tri_key(),
        };
        let absent = GameState::new_two_player(7);
        let err = resolve(&template, 0, &absent).unwrap_err();
        assert!(matches!(err, ReplayFailure::MissingSource { .. }));
        assert!(
            !matches!(err, ReplayFailure::IllegalTarget { .. }),
            "an Order-pin source failure is MissingSource, not IllegalTarget"
        );
        let mut present = GameState::new_two_player(7);
        bf_object(&mut present, 40, 40, 2);
        assert!(resolve(&template, 0, &present).is_ok());
    }

    /// T6 (CR 400.7, multi-authority): a re-entered permanent (same `ObjectId`,
    /// `incarnation` bumped) no longer matches a pin latched to the prior incarnation ⇒
    /// `resolve_source` `None`. Control: the matching incarnation resolves. An id-only
    /// matcher would wrongly resolve the stale pin.
    #[test]
    fn reentry_incarnation_invalidates_thisobject() {
        let mut state = GameState::new_two_player(7);
        bf_object(&mut state, 50, 50, 5); // current incarnation is 5

        assert_eq!(
            resolve_source(&this_obj(50, Some(4)), &state),
            None,
            "a bumped incarnation (5 ≠ latched 4) must NOT match (CR 400.7)"
        );
        assert_eq!(
            resolve_source(&this_obj(50, Some(5)), &state),
            Some(ObjectId(50)),
            "the matching incarnation resolves — the matcher reads incarnation, not just id"
        );
    }

    /// T7 (multi-authority): two battlefield objects share a `card_id`; `AllCopies`
    /// resolves to the LOWEST `ObjectId`, stably. Adding a lower-id same-card object
    /// moves the result to it — proving deterministic-lowest, not `im::HashMap` order.
    #[test]
    fn allcopies_resolves_deterministically() {
        let mut state = GameState::new_two_player(7);
        bf_object(&mut state, 60, 100, 0);
        bf_object(&mut state, 65, 100, 0);
        assert_eq!(resolve_source(&all_copies(100), &state), Some(ObjectId(60)));
        assert_eq!(
            resolve_source(&all_copies(100), &state),
            Some(ObjectId(60)),
            "stable across calls"
        );

        bf_object(&mut state, 55, 100, 0); // a lower-id copy
        assert_eq!(
            resolve_source(&all_copies(100), &state),
            Some(ObjectId(55)),
            "resolves to the new lowest id — deterministic-lowest, not hash order"
        );
    }

    /// T8 (CR 732.2a): the predictability gate rejects a required slot with no matching
    /// pin (`UnpinnedChoice`); a fully-pinned template over the same required slots
    /// passes. A gate that didn't diff required-vs-pinned would fail the negative half.
    #[test]
    fn gate_rejects_unpinned_choice() {
        let slot_a = DecisionSlot {
            source: this_obj(70, None),
            index: 0,
        };
        let slot_b = DecisionSlot {
            source: this_obj(71, None),
            index: 0,
        };
        let required = vec![slot_a.clone(), slot_b.clone()];

        // Pins only slot_a ⇒ slot_b is unpinned.
        let partial = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::MayChoice {
                slot: slot_a.clone(),
                take: MayChoiceOption::Take,
            }],
            replay: ReplayMode::Static,
            key: tri_key(),
        };
        assert_eq!(
            predictability_gate(&partial, &required).unwrap_err(),
            PredictabilityViolation::UnpinnedChoice {
                slot: slot_b.clone()
            },
            "the specific unpinned slot is reported"
        );

        // POSITIVE PAIR: pin both ⇒ Ok.
        let full = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![
                PinnedDecision::MayChoice {
                    slot: slot_a,
                    take: MayChoiceOption::Take,
                },
                PinnedDecision::Targets {
                    slot: slot_b,
                    targets: vec![],
                },
            ],
            replay: ReplayMode::Static,
            key: tri_key(),
        };
        assert!(
            predictability_gate(&full, &required).is_ok(),
            "a fully-pinned template passes the gate"
        );
    }

    /// T9 (G3, compile-enforced): this exhaustive, wildcard-free `match` mirrors
    /// `evaluate_schedule`'s. Adding an outcome-carrying `TargetSchedule` variant fails
    /// to compile in BOTH, forcing re-review of the CR 732.2a predictability firewall.
    #[test]
    fn target_schedule_predictability_firewall_is_exhaustive() {
        let variants = [
            TargetSchedule::Constant(this_obj(1, None)),
            TargetSchedule::RoundRobin(vec![this_obj(1, None)]),
            TargetSchedule::Piecewise(vec![(0, this_obj(1, None))]),
        ];
        for sched in &variants {
            // NO wildcard arm: each variant is a pure fn of (iteration index, live set),
            // carrying no prior-outcome input.
            let is_pure = match sched {
                TargetSchedule::Constant(_) => true,
                TargetSchedule::RoundRobin(_) => true,
                TargetSchedule::Piecewise(_) => true,
            };
            assert!(is_pure);
        }
    }

    /// Insert an untapped GREEN 1/1 creature controlled by P0 on the battlefield.
    fn green_creature(state: &mut GameState, id: u64) {
        use crate::types::card_type::CoreType;
        let oid = ObjectId(id);
        let mut o = GameObject::new(
            oid,
            CardId(id),
            PlayerId(0),
            "Saproling".to_string(),
            Zone::Battlefield,
        );
        o.card_types.core_types = vec![CoreType::Creature];
        o.color = vec![crate::types::mana::ManaColor::Green];
        state.objects.insert(oid, o);
        state.battlefield.push_back(oid);
    }

    /// Convoke-pin unit (§11): `resolve_pin(ConvokeTaps)` at a live `ManaPayment{Convoke}`
    /// delegates to the single-authority `select_convoke_taps`, pulling the locked cost from
    /// `pending_cast` and the payer from the prompt. Positive: a `{G}` pending cost + two
    /// green creatures ⇒ `ConcreteDecision::ConvokeTaps` with the minimal lowest-id set.
    /// Negative (revert-failing wiring): away from a `ManaPayment` prompt (no `pending_cast`)
    /// ⇒ `Err(UnpayableConvoke)` — proves the pin never fabricates taps without a live cost.
    #[test]
    fn convoke_pin_resolves_minimal_set_and_fails_closed() {
        use crate::types::game_state::{ConvokeMode, PendingCast};
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType};

        let slot = DecisionSlot {
            source: this_obj(99, None),
            index: 0,
        };
        let pin = PinnedDecision::ConvokeTaps { slot: slot.clone() };

        // Positive: at a live ManaPayment{Convoke} with a {G} locked cost + two green creatures.
        let mut state = GameState::new_two_player(7);
        green_creature(&mut state, 40);
        green_creature(&mut state, 41);
        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::unimplemented("test", "convoke pin fixture"),
            Vec::new(),
            ObjectId(40),
            PlayerId(0),
        );
        let pending = PendingCast::new(
            ObjectId(50),
            CardId(50),
            ability,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            },
        );
        state.pending_cast = Some(Box::new(pending));
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: Some(ConvokeMode::Convoke),
        };

        let out = resolve_pin(&pin, 0, &state).expect("convoke pin resolves to a tap set");
        match out {
            ConcreteDecision::ConvokeTaps { creatures, .. } => {
                assert_eq!(
                    creatures,
                    vec![(ObjectId(40), ManaType::Green)],
                    "{{G}} ⇒ exactly one tap, lowest-id green (CR 702.51b), delegated to select_convoke_taps"
                );
            }
            other => panic!("expected ConvokeTaps, got {other:?}"),
        }

        // Negative (fail-closed wiring): default two-player state is at Priority with no
        // pending_cast ⇒ the pin cannot read a live cost ⇒ UnpayableConvoke.
        let idle = GameState::new_two_player(7);
        assert!(
            matches!(
                resolve_pin(&pin, 0, &idle),
                Err(ReplayFailure::UnpayableConvoke { .. })
            ),
            "no live ManaPayment/pending_cast ⇒ UnpayableConvoke (never fabricate taps)"
        );
    }
}
