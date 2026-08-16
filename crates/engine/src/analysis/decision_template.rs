//! CR 732.2a: a shortcut is "a sequence of game choices, for all players, that may
//! be legally taken based on the current game state and the predictable results."
//! A [`DecisionTemplate`] captures that sequence so it can be replayed verbatim when a
//! simultaneous-trigger group recurs (CR 603.3b ordering) or driven across loop
//! iterations as a predictable shortcut (CR 732.2a). PURELY ADDITIVE / offline —
//! never called from the reducer in this phase.

use crate::types::game_state::{GameState, WaitingFor, YieldTarget};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaType};
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
/// B3/B5 will add, so the gate can filter to `TriggerOrdering` only. The FILTER it
/// enables is load-bearing now (the gate must ignore non-ordering templates), and
/// `LoopChoice`'s own consumer is now known: it is the CROSS-EPISODE CARRIER. A
/// `LoopChoice` entry in `GameState::decision_templates` survives the CR 603.3b batch
/// boundary — `GameState::clear_ephemeral_trigger_order_templates`' retain predicate is
/// scoped to `TriggerOrdering` — so it is the vehicle a later episode's declaration can
/// ride, and it is still POPULATED BY PHASE 4 AND BY NOTHING TODAY. PINNED BY A SHIPPED
/// ROW: `fantastic_four_bounded_loop::r3b_driven_a_loop_choice_carrier_survives_a_whole_
/// accepted_f4_drive` plants the `(kind × ephemerality)` cells on the real 4-player board
/// and drives a whole accepted CR 732.2a shortcut through `apply()` — the `LoopChoice`
/// cell survives (`3 → 2`) while the `TriggerOrdering` ephemeral cell beside it does not.
/// `loop_shortcut_ranking::r3b_*` is the seam-level statement of the same predicate.
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DecisionTemplate {
    pub owner: PlayerId,
    /// Pins in the group's canonical decision order.
    pub decisions: Vec<PinnedDecision>,
    pub replay: ReplayMode,
    pub key: DecisionGroupKey,
}

/// Identifies one free choice within a group: which source raised it (CR 400.7-stable
/// [`DecisionSource`]) plus a sub-index disambiguating multiple choices from one source
/// (e.g. two target slots on one ability). It derives a canonical order because
/// `GameAction::DeclareShortcut` participates in deterministic AI action ordering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DecisionSlot {
    pub source: DecisionSource,
    pub index: u8,
}

impl DecisionSlot {
    /// CR 601.2c (reached for a triggered ability via CR 603.3d) + CR 115.2: the
    /// ANNOUNCEMENT target slot a BOUNDED-CYCLE ENTRY publishes.
    ///
    /// SCOPE OF THE AUTHORITY, stated narrowly because the number 0 is not globally
    /// reserved: these two constructors are the single authority for the SUB-INDEX SHARED
    /// BY `game::engine::entry_publishes_pin_slots` AND `GameState::loop_answer_journal` —
    /// the publisher and the journal writers must agree, the way
    /// `game::engine::object_decision_source` already makes them agree on the source half.
    /// The `record_loop_pin` recast-template producer runs its OWN sub-index namespace over
    /// the same source (0 for its `Targets`/`ConvokeTaps` pin, 1 for its `ManaColor` pin)
    /// and deliberately does NOT route through here — its indices answer a different
    /// question and coincide numerically only by accident.
    ///
    /// `pub`, not `pub(crate)`: `crates/engine/tests/integration/` is a SEPARATE CRATE, and
    /// a `pub(crate)` constructor is unnameable there, so the integration rows would
    /// hand-roll the very literal this constructor exists to delete (the shape
    /// `object_decision_source`'s `pub(crate)` already forces on `may_source_key` in
    /// `fantastic_four_bounded_loop.rs` and on `decision_source` in `natural_balance.rs`).
    /// Every field of this `pub` struct in this `pub` module is already `pub`, so this adds
    /// no reachability the type does not already have — it only removes the literal.
    pub fn target(source: DecisionSource) -> Self {
        Self { source, index: 0 }
    }

    /// CR 603.5: the "may" gate on the SAME source — a second choice of one ability
    /// instance, which is exactly what the sub-index exists to disambiguate. Same scoped
    /// authority and same visibility rationale as [`DecisionSlot::target`].
    pub fn may(source: DecisionSource) -> Self {
        Self { source, index: 1 }
    }
}

/// CR 603.5: whether a "may" pin takes the optional action or declines it. Typed (not `bool`)
/// so both outcomes are self-documenting at every construction and match site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MayChoiceOption {
    Take,
    Decline,
}

/// CR 732.6: whether an "[A] unless [B]" pin pays [B] to break the loop, or declines and takes [A].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum UnlessPaymentOption {
    Pay,
    Decline,
}

/// The observed answer to ONE published decision slot, from ONE seat, across the current
/// loop-detection window. Journalled under the key `(DecisionSlot, PlayerId)`
/// (`GameState::record_loop_answer`), so a seat can only ever answer for itself and the
/// sub-index keeps the two slots one source can publish (CR 601.2c target, CR 603.5 "may")
/// in two entries rather than collapsing them into one latched conflict.
///
/// CR 732.2a says a shortcut proposal describes "a sequence of game choices, for all
/// players, that may be legally taken based on the current game state and the predictable
/// results of the sequence of choices", and that this sequence "may be a non-repetitive
/// series of choices, a loop that repeats a specified number of times, multiple loops, or
/// nested loops, and may even cross multiple turns". A series whose answers DIFFER between
/// iterations is therefore not, by itself, the conditional action the rule bars; what the
/// rule actually bars is narrower — "It can't include conditional actions, where the
/// outcome of a game event determines the next action a player takes."
///
/// This engine refuses on a differing answer anyway. That is an ENGINE-CAPABILITY LIMIT,
/// DELIBERATELY MORE CONSERVATIVE THAN CR 732.2a REQUIRES — not a rule the CR states.
/// [`DecisionTemplate`] pins exactly one `MayChoice` per published slot per cycle, so a
/// non-uniform series has no representation here; it is the same "a choice a player could
/// only make reactively is one they cannot pin" disposition [`predictability_gate`]'s
/// CR 732.2a firewall doc already records. Failing to offer is the fail-closed direction:
/// strictly fewer offers, never a wrong pin.
// `Copy` is DROPPED here (and nowhere else): `LoopAnswerValue::Targets` carries a `Vec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopAnswer {
    /// Every observed iteration answered this (slot, seat) pair identically.
    Uniform(LoopAnswerValue),
    /// Two observed iterations of the same (slot, seat) pair disagreed. Latched:
    /// never returns to `Uniform`. See the type doc — the refusal this produces is this
    /// engine's conservative policy, NOT a CR 732.2a mandate.
    Conflicted,
}

/// The VALUE of one observed answer. Variants are distinct CR choice KINDS — the same axis
/// [`PinnedDecision`] and [`DecisionPointKind`] already partition, and under the same
/// CR 732.2a umbrella ("a sequence of game choices, for all players") that makes each of
/// those ONE type rather than one type per rule section. It is therefore a PARTIAL
/// observation-side projection of that kind space (2 of the 6 [`DecisionPointKind`]
/// variants), TOTALIZED at the consumer's wildcard-free `(DecisionPointKind,
/// LoopAnswerValue)` match — not an exhaustive peer of either.
///
/// Parameterizing [`LoopAnswer::Uniform`] rather than adding a `UniformTargets` sibling is
/// deliberate: a `X`/`TargetX` sibling pair is CLAUDE.md's sibling-cluster smell, and it
/// would put the KIND axis on the same enum level as the LATCH axis (`Uniform` vs
/// `Conflicted`), which is the layer conflation CLAUDE.md's enum-design rule forbids.
///
/// DELIBERATELY DERIVES NO `Serialize`/`Deserialize`, exactly as [`LoopAnswer`] does: the
/// `#[serde(skip)]` on `GameState::loop_answer_journal` is enforced AT COMPILE TIME by the
/// absence of that derive, and adding one here would silently re-open persistence of a
/// transient window. ([`TargetPin`] and [`MayChoiceOption`] do derive it; the bar lives on
/// the two enums above them, which is where it was put.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopAnswerValue {
    /// CR 603.5: take the optional action, or decline it.
    May(MayChoiceOption),
    /// CR 608.2b + CR 601.2c: the announced targets for one slot, in announcement order.
    Targets(Vec<TargetPin>),
}

/// One pinned decision. Variants are distinct CR choice KINDS (ordering / targeting /
/// modal / optional-"may" / "[A] unless [B]" break), not a parameterization axis.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    /// iteration (fodder-first order — reproduced tokens preferred, then lowest ObjectId
    /// within each class) via `select_convoke_taps`, a pure function of (live legal untapped
    /// set, locked cost) per CR 732.2a — so no per-iteration creature is latched here.
    ConvokeTaps { slot: DecisionSlot },
    /// CR 608.2d + CR 605.3b: a fixed mana-color choice for an "add one mana of any color"
    /// mana ability whose product pays a colored downstream cost (the loop's mana-neutrality
    /// authority — e.g. Relic of Legends' Blue feeding Freed from the Real's `{U}` untap).
    /// LATCHED to the color the player produced in the demonstrated iteration and copied
    /// through unchanged every replay (the color is a constant, not a per-iteration re-binding —
    /// CR 732.2a "predictable results"). Distinct CR choice KIND (color selection, CR 608.2d),
    /// not a parameterization of the target/modal/may axes. `slot.index` disambiguates it from a
    /// tap-cost `Targets` pin on the SAME mana-ability source.
    ManaColor {
        slot: DecisionSlot,
        color: ManaColor,
    },
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
    /// CR 732.2a: the largest number of repetitions this proposal may legally specify — the
    /// minimum over every applicable CR 704 elimination bound and finite-pool bound, over
    /// every LIVING player, aggregated per declarable victim, clamped to
    /// `MAX_SHORTCUT_CYCLES`. `IterationCount` above is the *suggestion*; this is the
    /// *bound*, and they are deliberately separate fields: a proposal that exceeds this
    /// contains a conditional action (an in-proposal CR 704.5a / CR 704.5c / CR 104.3c /
    /// CR 121.4 elimination would decide what happens next), which CR 732.2a forbids.
    ///
    /// The single count authority: the declared-count check in `game::engine` rejects a
    /// `Fixed(n)` above it, and `game::interaction` publishes it as the count picker's
    /// ceiling. Every offer built before the bounded-offer phase carries
    /// `MAX_SHORTCUT_CYCLES`, and those checks were inert until the bounded-cycle producer
    /// began narrowing it.
    ///
    /// DELIBERATELY NOT MIRRORED in `client/src/adapter/types.ts::ShortcutDecisionSchema`:
    /// the frontend never reads the raw bound, it reads the already-clamped ceiling the
    /// engine publishes as `InteractionShortcutCountSpec::Fixed { max }`. Mirroring it
    /// would hand the display layer a second number it would have to reconcile — exactly
    /// the derive-in-the-frontend the layer rule forbids.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// The open per-iteration decision-points needing pins. EMPTY for a choice-free drain.
    pub points: Vec<DecisionPoint>,
    /// CR 702.51a: total untapped creatures the controller may tap for convoke across every
    /// `ConvokeTaps` point — engine-owned so the frontend renders the count directly instead of
    /// re-deriving it (display-layer purity). Sum of each `ConvokeTaps.tappable.len()`.
    #[serde(default)]
    pub convoke_tappable_count: usize,
}

/// A schema deserialized from a pre-bound snapshot carries no CR 732.2a count bound. The
/// forward-compatible default is the global safety limit, which is what every producer
/// emitted before the field existed — so an old save round-trips byte-equivalently.
fn default_max_iterations() -> u32 {
    crate::game::engine::MAX_SHORTCUT_CYCLES
}

// CR 732.2a: `IterationCount` carries no `Default` and its `Fixed(u32)` is a tuple variant
// (so a derived `#[default]` cannot apply) — hand-impl the forward-compat deser default the
// `#[serde(default)]` on `WaitingFor::LoopShortcut.schema` needs.
impl Default for ShortcutDecisionSchema {
    fn default() -> Self {
        Self {
            iteration_count: IterationCount::Fixed(0),
            max_iterations: default_max_iterations(),
            points: Vec::new(),
            convoke_tappable_count: 0,
        }
    }
}

impl ShortcutDecisionSchema {
    /// CR 732.2a: `true` iff this offer's producer NARROWED the repetition bound below the
    /// engine-wide safety cap — i.e. it measured a CR 704.5a / CR 704.5c / CR 104.3c
    /// threshold inside the loop. A producer that cannot compute a real bound publishes
    /// `MAX_SHORTCUT_CYCLES` (see `max_iterations` above), so an unnarrowed offer is NOT
    /// bounded in this sense.
    ///
    /// The SINGLE AUTHORITY for that question, and the reason it is a method rather than
    /// an inline comparison repeated at each caller: `MAX_SHORTCUT_CYCLES` is `pub(crate)`
    /// to the engine, so `phase-ai`'s declare policy cannot name it and would otherwise
    /// hard-code the literal. This predicate crosses the crate boundary; the constant does
    /// not.
    pub fn is_bounded(&self) -> bool {
        self.max_iterations < crate::game::engine::MAX_SHORTCUT_CYCLES
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
        min_targets: u32,
        max_targets: u32,
        ordered: bool,
    },
    /// CR 702.51a: untapped creatures the controller may tap for convoke (informational — the
    /// concrete taps are re-bound live by `select_convoke_taps`).
    ConvokeTaps { tappable: Vec<ObjectId> },
    /// CR 700.2 modal: the selectable mode indices.
    Mode {
        available_modes: Vec<usize>,
        min_modes: u32,
        max_modes: u32,
        allow_repeats: bool,
    },
    /// CR 603.5: a binary "may" — the slot alone identifies it (FE renders yes/no).
    MayChoice,
    /// CR 732.6: a binary "[A] unless [B]" break — pay or decline.
    UnlessBreak,
    /// CR 608.2d: a fixed mana-color choice — informational read-side (the color is latched;
    /// the FE renders it read-only, there is no per-iteration re-selection). Externally
    /// tagged → `{"ManaColor":{"color":"Blue"}}`.
    ManaColor { color: ManaColor },
}

/// CR 115.2 + CR 601.2c: WHO one announcement names, stored in re-bindable form. The
/// storable dual of [`ConcreteTarget`], which already draws exactly this two-way split at
/// the RESOLVED end of the same pipeline — so this adds no categorical boundary, it gives
/// the existing one a pre-resolution spelling.
///
/// PROVENANCE, and it is the whole point of the type: a `Seat` here is a TARGET
/// (CR 601.2c), judged by `game::targeting::player_is_legal_target` — existence PLUS
/// CR 702.11c hexproof / CR 702.18a shroud / CR 702.16b protection. A merely CHOSEN player
/// (CR 115.10a — e.g. a CR 701.34a proliferate choice) is NOT this type; it stays
/// [`TargetPin::Player`] and keeps its existence-only authority. Two questions, two
/// spellings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AnnouncementSubject {
    Object(DecisionSource),
    Seat(PlayerId),
}

/// Why a subject list is not a legal [`Ranking`]. Both clauses are refused at CONSTRUCTION,
/// which is what makes [`Ranking::head`] infallible — no `Option` leaks into the resolver,
/// and a wire-supplied list fails the LOAD rather than the drive (the same disposition
/// `reject_zero_bound_shortcut_offer` takes for a wire-sourced `max_iterations`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RankingError {
    /// An empty ranking names nobody: there is no head to announce.
    Empty,
    /// CR 601.2c: an announcement names its choice per target. A repeated subject is not an
    /// ordering — it is the same declaration twice, and it would make the tail unreachable.
    DuplicateSubject,
}

impl std::fmt::Display for RankingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("a ranking must name at least one announcement subject"),
            Self::DuplicateSubject => {
                f.write_str("a ranking must not name the same announcement subject twice")
            }
        }
    }
}

/// CR 732.1 + CR 732.2a: a DECLARED, ORDERED pre-commitment over announcement subjects for
/// ONE slot. A one-element ranking IS the old constant pin; that is the parameterization,
/// and it is why there is no `Ranked` sibling of [`TargetSchedule`].
///
/// CONSUMED AT AN EPISODE BOUNDARY, NEVER MID-DRIVE. Within one accepted drive only
/// [`Ranking::head`] is ever resolved (see `evaluate_schedule`): advancing to a later entry
/// because a game event removed the head would be the conditional action CR 732.2a bars, and
/// CR 732.2a also requires the sequence to END at a place where a player has priority —
/// which the drive-end handback already is. The tail is a pre-declaration for the NEXT
/// episode, validated by THAT episode's `validate_pins` against THAT episode's published
/// legal set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "Vec<AnnouncementSubject>")]
pub struct Ranking(Vec<AnnouncementSubject>);

impl Ranking {
    /// The invariant is expressed ONCE, here — and this is the `TryFrom` the
    /// `#[serde(try_from)]` shim runs, so a wire-supplied empty or duplicated list is
    /// refused before any resolver sees it.
    pub fn new(subjects: Vec<AnnouncementSubject>) -> Result<Self, RankingError> {
        if subjects.is_empty() {
            return Err(RankingError::Empty);
        }
        // Sort a view, never the payload: the declared ORDER is the whole point of the type.
        // `Ord` is derived (it has to be — `DecisionTemplate` derives it for deterministic AI
        // action ordering), so this is n·log n on a wire-length-bounded list rather than the
        // quadratic scan a non-`Hash` payload would otherwise force.
        let mut seen: Vec<&AnnouncementSubject> = subjects.iter().collect();
        seen.sort_unstable();
        if seen.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(RankingError::DuplicateSubject);
        }
        Ok(Self(subjects))
    }

    /// The constant case; every mechanical migration site uses it. Infallible — one element
    /// can violate neither clause.
    pub fn one(subject: AnnouncementSubject) -> Self {
        Self(vec![subject])
    }

    /// The ONLY reader inside `evaluate_schedule` (CR 732.2a: a drive resolves the head and
    /// never advances past it). Infallible by the non-empty invariant `new`/`one` enforce.
    pub fn head(&self) -> &AnnouncementSubject {
        &self.0[0]
    }

    /// The whole list, for the callers that must see past the head without resolving it:
    /// the hidden-source redaction walk (`game::visibility`) and the wire length bound.
    pub fn iter(&self) -> impl Iterator<Item = &AnnouncementSubject> {
        self.0.iter()
    }
}

impl TryFrom<Vec<AnnouncementSubject>> for Ranking {
    type Error = RankingError;

    fn try_from(subjects: Vec<AnnouncementSubject>) -> Result<Self, Self::Error> {
        Self::new(subjects)
    }
}

/// A pinned target. `ByIdentity` re-resolves to a live legal ObjectId each iteration
/// (CR 608.2b); `Scheduled` is an iteration-indexed pure function (CR 732.2a).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TargetPin {
    ByIdentity(DecisionSource),
    /// A CONSTANT **CHOICE-class** seat (CR 115.10a): this pin answers EVERY firing of its
    /// source within the period with the declared player. A seat is state-independent by
    /// construction — it can never denote "the newest copy" — so no iteration can turn the
    /// pin into the conditional action CR 732.2a forbids.
    ///
    /// # THE TWO CLASSES NOW HAVE TWO SPELLINGS, AND THIS ONE IS THE CHOICE CLASS
    ///
    /// CR 115.10a: "unless that object or player is identified by the word 'target' … it's
    /// not a target". A seat this pin names was CHOSEN, not targeted, so [`resolve_target`]
    /// judges it by `game::players::player_exists_for_choice` — EXISTENCE ONLY. Applying the
    /// targeting-only exclusions (CR 702.11c hexproof / CR 702.18a shroud / CR 702.16b
    /// protection) here would refuse legal CR 732.2a proposals; that over-veto is what
    /// `game::engine`'s `a_shrouded_player_pin_is_still_published_by_the_offer_builder` and
    /// `a_shrouded_seat_is_untargetable_yet_still_choosable_at_the_pin_recheck` (this
    /// module) exist to keep out. Its live in-process producer is the CR 701.34a proliferate
    /// arm (`game::engine::apply_action` → `record_loop_pin`).
    ///
    /// A CR 601.2c **TARGET**-class seat is a different question and takes the other
    /// spelling: [`AnnouncementSubject::Seat`] inside a [`Ranking`] inside
    /// [`TargetPin::Scheduled`], judged by `game::targeting::player_is_legal_target`. Both
    /// TARGET-class producers emit that spelling —
    /// `game::engine::record_trigger_target_answer` (the engine's own CR 601.2c
    /// announcement journal) and `game::interaction::materialize_loop_shortcut_response`
    /// (the human ingress of the same point kind). The spelling IS the provenance, so the
    /// authority is selected by what the answer IS, never by who submitted it.
    Player(PlayerId),
    Scheduled(TargetSchedule),
}

/// CR 732.2a: how the pins are replayed. `Static` (ordering) ignores the iteration
/// index; `Scheduled` (loop shortcut) makes every choice a pure function of it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
///
/// AND THE PURITY INVARIANT IS NARROWER THAN "consults the live set", now that each step
/// carries a [`Ranking`] rather than a single subject: a variant consults the live legal set
/// to **re-bind the declared subject** (CR 400.7); it never uses the live set to
/// **substitute a different subject**. Selecting a different entry because a game event
/// removed the first is exactly the conditional action CR 732.2a bars — which is why a
/// `Ranking` is advanced only at an episode boundary, by a caller, never by
/// `evaluate_schedule`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TargetSchedule {
    Constant(Ranking),
    RoundRobin(Vec<Ranking>),
    /// Pre-declared switch-over: identity for [start, next-start). The switch point is
    /// FIXED IN ADVANCE (not triggered by an in-loop event), keeping it 732.2a-predictable.
    Piecewise(Vec<(u32, Ranking)>),
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
    /// CR 608.2d: the copy-through mana color for this iteration (never fails — no state lookup).
    ManaColor {
        slot: DecisionSlot,
        color: ManaColor,
    },
    /// CR 601.2h + CR 702.51a/b: the live-resolved convoke tap-set for this iteration —
    /// `(creature, mana_type)` pairs to feed as `GameAction::TapForConvoke`. Re-bound each
    /// iteration by `select_convoke_taps` in `DetectionFodderFirst` order (CR 702.51a lets the
    /// loop replay tap reproduced fodder rather than a stable-partition engine permanent).
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
    /// CR 608.2b: a TARGET pin no longer resolves to a legal live target (left its zone /
    /// ceased to exist / CR 800.4 + CR 102.1 left the game). Raised whenever a *target* is
    /// illegal-or-absent, in ANY `ReplayMode` — a `Static`-mode `Targets` pin with a
    /// removed target yields THIS, not `MissingSource`. ⇒ abort the auto-shortcut, hand
    /// back to manual.
    ///
    /// Carries the [`TargetPin`] itself rather than a `DecisionSource`: `Player` and
    /// `Scheduled` pins are equally capable of going illegal, and a `DecisionSource` can
    /// name neither. Parameterizing the existing variant keeps ONE "target went illegal"
    /// failure instead of growing a per-pin-kind sibling cluster.
    IllegalTarget { slot: DecisionSlot, pin: TargetPin },
    /// CR 400.7: an ORDER pin's source does not re-bind to a live ability instance —
    /// [`resolve_ability_instance`] finds no object of that identity at that incarnation in a
    /// zone it admits (`Battlefield`; also `Command` for a `ThisObject` source) ⇒ the ordering
    /// template no longer matches
    /// ⇒ fall through to a normal manual prompt. Raised ONLY for the `Order` pin kind, in any
    /// `ReplayMode`.
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
        // CR 603.3b: replay this source's trigger at its pinned ordering position. The pin
        // re-binds to the SAME live ability instance (CR 400.7 incarnation) still present in
        // a zone the accessor admits — `Battlefield`, plus `Command` for a `ThisObject`
        // source — not merely to something still on the battlefield. `Command` is admitted
        // because a whole CLASS of sources functions from there (emblems CR 114.4; plane /
        // scheme / conspiracy cards CR 113.6p; face-up plane and phenomenon cards CR 901.7;
        // Eminence commanders CR 113.6b), but the accessor tests zone presence and identity
        // only — whether a given ability functions is `game::functioning_abilities`'
        // question, not this accessor's.
        //
        // Resolving an `Order` pin GRANTS NO CAPABILITY, and the six points that consume
        // `resolve`'s output split two ways. FIVE read the vec's ELEMENTS, and not one of them
        // reads a `ConcreteDecision::Order`'s PAYLOAD: `inject_pinned_answer`'s two arms
        // `find_map` for their own kind, `pinned_targets_for_source` and
        // `pinned_mana_color_for_source` skip it in an `if let`, and the `ManaPayment` beat's
        // exhaustive match aborts on its mere presence (`Order { .. }`, fields discarded). ONE
        // — the per-cycle re-check in `materialize_fixed_shortcut` — reads only the `is_err()`
        // VERDICT and discards the vec entirely. (LABELED CODE READ: the six consumption
        // points, enumerated and read at both revisions.)
        //
        // At the five ELEMENT readers an ORDER-ONLY template still fails closed; what the
        // re-bind changes is only WHERE: the abort moves out of `resolve()` and into the
        // consumer — the trailing `Err(RecastAbort)` of the two `pinned_*` seams, the
        // `find_map`'s `ok_or(RecastAbort)` in the injector, the match arm at the
        // `ManaPayment` beat.
        //
        // At the VERDICT reader nothing moves downstream, because there is nothing downstream
        // to move to: that gate consults only `resolve`'s `Result`. A pin that fails to
        // re-bind breaks the cycle loop AT THAT ITERATION, committing only the cycles before
        // it — and for a source the accessor never admits, that is iteration 0, so no cycle
        // commits at all. Once every pin re-binds, the gate stops breaking; what happens after
        // it is governed by the element readers above, not by this gate. That is true for
        // every template shape, Order-only and mixed alike, because the gate never looks at an
        // element.
        //
        // For a mixed template whose OTHER pins themselves resolve, the payoff lands at an
        // element reader that has an element of its own kind. Two of the five answer from a
        // `Targets` element — `pinned_targets_for_source` and the injector's
        // `TriggerTargetSelection` arm — and the one measured here is
        // `pinned_targets_for_source`: on the measured value `[Order @ command zone,
        // Targets @ battlefield]`, with the `Order` pin re-binding and the `Targets` pin itself
        // resolving, it now returns that `Targets` pin's own answer, where before the
        // command-zone `Order` pin discarded it and the seam aborted. Where a pin does NOT
        // resolve, the template still fails whole at every one of the six consumers, because
        // `resolve` is a per-pin `Result` collect that each consumer gates on before reading
        // anything: a mixed template carrying a `Targets` pin whose own target is not on the
        // battlefield (CR 608.2b) aborts after this commit exactly as before it. It is NOT
        // that every element reader succeeds on such a template: a reader that finds no
        // element of its own kind (`pinned_mana_color_for_source`, the injector's `MayChoice`
        // arm) still reaches its `Err(RecastAbort)`, and the `ManaPayment` beat still aborts on
        // the `Order` element's mere presence — after this commit exactly as before it, for
        // every mixed template.
        //
        // The bound that makes this capability-neutral at the `DeclareShortcut` wire is DRIVE
        // EQUALITY, not submittability. When an `Order` element re-binds, resolving WITH it
        // yields the identical answer to resolving without it at every one of the six
        // consumers, EXCEPT at the `ManaPayment` beat, where its presence forces
        // `Err(RecastAbort)` — strictly more fail-closed, never more permissive. When it does
        // NOT re-bind, the `Result` collect fails the whole template at every consumer; this
        // commit changes which sources re-bind, never that disposition. Where the same
        // template minus its `Order` pins is itself submittable, that drive was therefore
        // already expressible by omitting them. It is NOT always submittable: `pin_slot`
        // addresses an `Order` pin to `{source, index 0}` and `validate_pins`' `Order` arm is
        // the one arm that checks nothing, so an `Order` pin can be the sole cover of a
        // required point, and dropping it then fails `predictability_gate`. That shape is
        // pre-existing and zone-independent — this commit changes only which sources can be
        // spelled into it, never the shape itself.
        //
        // No template's ACCEPTANCE changes here: `declaration_conforms` is
        // `predictability_gate` + `validate_pins`, and neither reaches `resolve_pin`. What
        // changes is what an already-accepted template does. (Pinned by
        // `game::engine::stage2_injector_tests::a_command_zone_order_pin_stops_poisoning_the_template_without_gaining_capability`
        // rows R/N/N2/P/P-minus; the structural clauses above are labeled code reads.)
        PinnedDecision::Order { source, pos } => {
            let id = resolve_ability_instance(source, state).ok_or_else(|| {
                ReplayFailure::MissingSource {
                    source: source.clone(),
                }
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
        // CR 608.2d: the mana color is latched at record time and copied through unchanged —
        // no object to resolve, no per-iteration legality re-check (a color cannot become
        // illegal), CR 732.2a "predictable results".
        PinnedDecision::ManaColor { slot, color } => Ok(ConcreteDecision::ManaColor {
            slot: slot.clone(),
            color: *color,
        }),
        // CR 601.2h + CR 702.51a/b: re-bind the convoke tap-set LIVE against this
        // iteration's board. The caster + locked remaining cost come from the live
        // `ManaPayment` prompt (CR 601.2f cost-lock); the single-authority selector
        // `select_convoke_taps` picks the minimal deterministic set. A `ConvokeTaps` pin is
        // minted ONLY by the loop-replay template (`build_recast_template`), so this replay
        // artifact uses `DetectionFodderFirst`: CR 702.51a makes convoke optional, so the
        // replay MAY tap the reproduced fodder it recreates each period rather than a
        // stable-partition engine permanent (which would drift the CR 732.2a object-growth
        // cover check and suppress a valid loop). No legal set ⇒ `UnpayableConvoke` (CR 702.51b).
        PinnedDecision::ConvokeTaps { slot } => {
            let (player, cost) = match (&state.waiting_for, state.pending_cast.as_ref()) {
                (WaitingFor::ManaPayment { player, .. }, Some(pending)) => {
                    (*player, pending.cost.clone())
                }
                _ => return Err(ReplayFailure::UnpayableConvoke { slot: slot.clone() }),
            };
            match crate::game::mana_payment::select_convoke_taps(
                state,
                player,
                &cost,
                crate::game::mana_payment::ConvokeTapOrder::DetectionFodderFirst,
            ) {
                Some(creatures) => Ok(ConcreteDecision::ConvokeTaps {
                    slot: slot.clone(),
                    creatures,
                }),
                None => Err(ReplayFailure::UnpayableConvoke { slot: slot.clone() }),
            }
        }
    }
}

/// Resolve one target pin. CR 608.2b: a by-identity, player, or scheduled target must
/// still be a legal live target; an absent or departed one is `IllegalTarget`.
fn resolve_target(
    pin: &TargetPin,
    slot: &DecisionSlot,
    iteration: IterationIndex,
    state: &GameState,
) -> Result<ConcreteTarget, ReplayFailure> {
    let illegal = || ReplayFailure::IllegalTarget {
        slot: slot.clone(),
        pin: pin.clone(),
    };
    match pin {
        TargetPin::ByIdentity(source) => resolve_source(source, state)
            .map(ConcreteTarget::Object)
            .ok_or_else(illegal),
        // CR 115.10a + CR 701.34a: this seam answers "may this seat be CHOSEN", not "may it
        // be TARGETED". A proliferate choice is not a target, so the targeting-only
        // exclusions (CR 702.11c hexproof / CR 702.18a shroud / CR 702.16b protection) must
        // NOT be applied here — doing so would refuse a legal CR 732.2a proposal (the
        // over-veto class). Existence is delegated to
        // `game::players::player_exists_for_choice` (CR 800.4 + CR 102.1, phasing per the
        // CR 702.26b MIRROR).
        //
        // AND THE OTHER HALF, so nobody "fixes" this back into the over-veto: declare-path
        // TARGET legality is enforced by `validate_pins`' `legal_targets.contains(..)`
        // against the offer's PUBLISHED set, which `ability_utils::build_target_slots`
        // derives through `targeting::find_legal_targets` →
        // `static_abilities::player_cannot_be_targeted_by`. It is NOT enforced here, and
        // does not need to be.
        //
        // THE RESIDUAL'S DEFERRED FIX SHAPE HAS LANDED FOR THE IN-PROCESS SURFACE: the two
        // classes now have TWO SPELLINGS, so they are distinguishable AT THIS SEAM by the
        // variant alone. `TargetPin::Player` is the CHOICE class (this arm, existence only);
        // a CR 601.2c TARGET-class seat is `AnnouncementSubject::Seat` inside a `Ranking`
        // inside `TargetPin::Scheduled`, resolved by the arm below through
        // `targeting::player_is_legal_target`. THE PREVIOUS TEXT WAS SCOPED TOO NARROWLY and
        // is corrected rather than deleted: it named only `record_loop_pin` (three sites,
        // one of which — the CR 701.34a proliferate-target arm — is a genuine CHOICE) and
        // was SILENT about the `record_loop_answer` route, along which
        // `game::engine::record_trigger_target_answer` did produce a TARGET-class
        // `TargetPin::Player` from a `WaitingFor::TriggerTargetSelection` announcement. That
        // producer, and the human ingress of the same point kind
        // (`game::interaction::materialize_loop_shortcut_response`), now emit the ranked
        // spelling. So no IN-PROCESS producer can reach this arm with a target any more, and
        // "not live today" is now an enforced property rather than a census result — pinned
        // by `tests/integration/loop_shortcut_seat_pin_census.rs`.
        //
        // OPEN RESIDUAL — the object-growth route, which is why this arm is still not a
        // sufficient authority on its own. INVARIANT: a `TargetPin::Player` must never reach
        // materialization validated only against a legal set derived from the declared pins
        // themselves. `try_offer_object_growth_shortcut` builds its points through
        // `pinned_decisions_to_points`, whose legal sets come FROM the pins, so on that
        // route the offer would ratify its own pin — and CR 732.2a admits only a sequence
        // "that may be legally taken based on the current game state", which a self-derived
        // set cannot establish. That hazard is class-independent: it is about WHERE the
        // legal set came from, not about which spelling the pin uses, so the split narrows
        // this residual's producer surface without closing it.
        //
        // WHAT REMAINS OPEN, PRECISELY — PINS ARRIVE WIRE-SOURCED, and no in-process
        // invariant covers that. `LoopActionContext` is
        // `#[serde(from = "LoopActionContextRepr")]`, and that shim's `From` impl installs
        // the deserialized vector verbatim (`pins: r.pins`), so a restored save can still
        // carry a `TargetPin::Player` a foreign writer MEANT as a target. The wire carries
        // the spelling, not the writer's intent, so the split cannot adjudicate that case —
        // it can only make the honest spelling available and make the in-process producers
        // use it. `GameState::migrate_transient_loop_sequence` keeps a loaded sequence ONLY
        // for a save captured in a `LoopShortcut` / `RespondToShortcut` window, and on that
        // route the pins are replayed by the accept→materialize drive through
        // `build_recast_template` → `decision_template::resolve`, i.e. through THIS call —
        // so a wire pin's EXISTENCE half is authority-enforced here too. Same class as the
        // wire-sourced `max_iterations` defect `reject_zero_bound_shortcut_offer` closes: a
        // load-seam value the in-process producer census cannot see.
        //
        // DAMAGE MODE if a wire producer does that: `CycleOutcome::Abort` rolls back only
        // the crossing cycle, so cycles `0..k` stay committed under a pin no authority ever
        // validated. Note what is NOT the damage mode, because the two are easy to conflate:
        // a correctly-spelled ranked seat that becomes an illegal target mid-drive is
        // handled BY CONSTRUCTION and is not a residual at all — `evaluate_schedule`
        // resolves `head()` only and never slides to a later entry, so the drive aborts at
        // the boundary (CR 115.7a: "if a target can't be changed to another legal target,
        // the original target is unchanged, even if the original target is itself illegal by
        // then"; CR 732.2a bars the conditional action sliding would be).
        TargetPin::Player(p) => crate::game::players::player_exists_for_choice(state, *p)
            .then_some(ConcreteTarget::Player(*p))
            .ok_or_else(illegal),
        TargetPin::Scheduled(sched) => evaluate_schedule(sched, slot, iteration, state),
    }
}

/// Re-bind a stored `DecisionSource` to a live battlefield `ObjectId`. The battlefield
/// analogue of `GameState::is_priority_yielded`'s matching arms. KIND-AGNOSTIC: returns
/// `None` on no match, and the CALLER maps that to the pin-kind-appropriate
/// `ReplayFailure` (`Order` ⇒ `MissingSource`, a target ⇒ `IllegalTarget`) — G2's
/// per-pin-kind failure selection.
///
/// The `Order` pin and the two `game::engine` slot seams enter through
/// [`resolve_ability_instance`] rather than here; this function is that accessor's
/// BATTLEFIELD DISJUNCT, and it remains the whole answer for the TARGET path
/// ([`resolve_target`]'s `Object` arm and `evaluate_schedule`'s `Object` head), where the
/// battlefield filter IS the CR 608.2b legality re-check.
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

/// CR 608.2b + CR 114.4 + CR 113.6p: re-bind a stored `DecisionSource` to the live ABILITY
/// INSTANCE it identifies — a different question from [`resolve_source`]'s, and the reason
/// this accessor exists rather than a second spelling at each caller.
///
/// THE ONLY SPELLING of that question, as of the commit that migrated the last two callers.
/// Every production asker routes here: `resolve_pin`'s `Order` arm and `evaluate_schedule`'s
/// `Seat` arm call it directly, and `game::engine::slot_source_prompted` is the `bool`-valued
/// wrapper its four seams use — `inject_pinned_answer`'s `TriggerTargetSelection` and
/// `MayChoice` `find_map` guards, `pinned_targets_for_source`, and
/// `pinned_mana_color_for_source`. There is no bare `resolve_source` slot comparison left in
/// `game::engine`.
///
/// A *pin's* source identifies a TARGET, so [`resolve_source`] is deliberately
/// BATTLEFIELD-ONLY and that filter IS the CR 608.2b legality re-check: a pinned target that
/// left the battlefield must stop matching, and it must not be widened. A *slot's* source
/// only identifies WHICH ability instance prompts. CR 114.2 puts an EMBLEM — "both owned and
/// controlled by that player" — into the COMMAND zone; that is PLACEMENT. Whether the thing
/// placed there can prompt at all is a separate rule, and it is per ABILITY rather than per
/// object: CR 113.6b, "an ability that states which zones it functions in functions only from
/// those zones". So the command-zone disjunct lives here, scoped to object identity plus the
/// pinned CR 400.7 incarnation, exactly as the battlefield arm is.
///
/// The class this disjunct serves is every command-zone-functioning ability source, not
/// emblems alone — which is why the filter is NOT tightened to `obj.is_emblem`:
///
/// * **emblems** — CR 114.4, "abilities of emblems function in the command zone";
/// * **planes, schemes, conspiracies** — CR 113.6p, whose enumeration is "emblems, plane
///   cards, vanguard cards, scheme cards, and conspiracy cards"; `database::synthesis`'s
///   `synthesize_planechase` / `synthesize_archenemy` / `synthesize_conspiracy` stamp
///   `Zone::Command` onto each such face's triggers and statics;
/// * **phenomena** — CR 901.7, "any abilities of a FACE-UP plane card or phenomenon card in
///   the command zone function from that zone" (CR 113.6p's enumeration does not reach this
///   card type; `synthesize_planechase` covers both faces);
/// * **Eminence commanders** — an ordinary card whose own ability declares its zones, i.e.
///   CR 113.6b again, opted in per definition rather than per card type.
///
/// `game::functioning_abilities` is where that opt-in is read (`active_zones` for a static,
/// `trigger_zones` for a trigger), and it — not this accessor — decides whether an ability
/// functions. A single object can carry a Command-functioning static and Battlefield-only
/// triggers at once, so an object-level zone test could never be the functioning authority;
/// identity is what this accessor selects on.
///
/// RESIDUAL, measured and disclosed rather than closed: the command disjunct is
/// `ThisObject`-only. `AllCopies` matches by CARD identity and is battlefield-only, so a
/// command-zone source spelled by card identity — a conspiracy, an Eminence commander — still
/// resolves `None` and fails closed. Graveyard / exile / hand sources resolve `None` too ⇒
/// every caller fails closed (`game::engine::slot_source_prompted` aborts the drive to manual
/// play; `evaluate_schedule`'s `Seat` arm raises `IllegalTarget`).
pub(crate) fn resolve_ability_instance(
    src: &DecisionSource,
    state: &GameState,
) -> Option<ObjectId> {
    if let Some(id) = resolve_source(src, state) {
        return Some(id);
    }
    let YieldTarget::ThisObject {
        source_id,
        incarnation,
        ..
    } = src
    else {
        return None;
    };
    state
        .objects
        .get(source_id)
        .filter(|o| o.zone == Zone::Command)
        .filter(|o| incarnation.is_none() || *incarnation == Some(o.incarnation))
        .map(|o| o.id)
}

/// CR 732.2a predictability firewall: EXHAUSTIVE `match` over [`TargetSchedule`] with NO
/// wildcard arm — a future outcome-carrying variant breaks this build (mirrored by the
/// `target_schedule_predictability_firewall_is_exhaustive` test). Every variant is a
/// pure fn of (iteration index, live set); each selects a [`Ranking`], whose HEAD is then
/// re-bound against live state (CR 608.2b).
///
/// HEAD-ONLY, and that is the CR 732.2a clause rather than a simplification: skipping to a
/// later entry because the head became illegal is a conditional action ("the outcome of a
/// game event determines the next action a player takes"). The tail is the NEXT episode's
/// pre-declaration; only an episode boundary may advance it.
fn evaluate_schedule(
    sched: &TargetSchedule,
    slot: &DecisionSlot,
    iter: IterationIndex,
    state: &GameState,
) -> Result<ConcreteTarget, ReplayFailure> {
    let ranking: &Ranking = match sched {
        TargetSchedule::Constant(ranking) => ranking,
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
            .map(|(_, ranking)| ranking)
            .ok_or_else(|| ReplayFailure::ScheduleExhausted { slot: slot.clone() })?,
    };
    match ranking.head() {
        // CR 608.2b: a pinned TARGET object must still be a live battlefield object — this
        // is exactly the pre-parameterization `Constant` behaviour, unchanged.
        AnnouncementSubject::Object(src) => resolve_source(src, state).map(ConcreteTarget::Object),
        // CR 601.2c + CR 115.1: a ranked seat is a TARGET, so it is judged by
        // `targeting::player_is_legal_target` (existence + CR 702.11c hexproof /
        // CR 702.18a shroud / CR 702.16b protection) rather than by existence alone. Its two
        // trailing arguments describe THE ABILITY INSTANCE that would name the seat, not a
        // target object — hence `resolve_ability_instance` (which admits the command zone —
        // CR 114.4 / CR 113.6p) and NOT `resolve_source` (battlefield-only, and correctly so
        // for a pin).
        //
        // A `None` ANYWHERE in this chain falls through to the `ok_or_else` below: with no
        // live ability instance the engine cannot certify that the object it would ask the
        // CR 702.11c question about still IS that instance (CR 400.7 / CR 608.2b), and
        // CR 732.1 + CR 732.2a make refusing a shortcut free — "the player with priority MAY
        // suggest a shortcut" is a permission, not an obligation, so no declaration published
        // just means the table plays the loop out manually. (CR 732.2b is the RESPONDER rule
        // — each OTHER player accepting or shortening a proposal that already exists — so it
        // cannot govern a proposer that publishes nothing.) Announcing a target we cannot
        // certify is not
        // free. This is the fail-closed branch, not an oversight.
        AnnouncementSubject::Seat(p) => resolve_ability_instance(&slot.source, state)
            .and_then(|src_id| state.objects.get(&src_id).map(|o| (src_id, o.controller)))
            .filter(|(src_id, ctrl)| {
                crate::game::targeting::player_is_legal_target(state, *p, *src_id, *ctrl)
            })
            .map(|_| ConcreteTarget::Player(*p)),
    }
    .ok_or_else(|| ReplayFailure::IllegalTarget {
        slot: slot.clone(),
        pin: TargetPin::Scheduled(sched.clone()),
    })
}

/// CR 732.2a firewall: a `Scheduled` template may auto-drive a shortcut only if every
/// free choice in the cycle is pinned (TOTAL COVERAGE).
///
/// THIS IS WHERE THE NO-CONDITIONAL-ACTIONS CLAUSE IS SATISFIED, AND IT IS SATISFIED BY
/// CONSTRUCTION. CR 732.2a requires a proposal describe "the predictable results of the sequence of
/// choices" and says it "can't include conditional actions, where the outcome of a game event
/// determines the next action a player takes". Pins fix every free choice BEFORE the offer is made,
/// so the sequence the table accepts is the sequence that runs. The two companion gates are
/// `game::engine::try_offer_object_growth_shortcut`'s static rejection of coin flip / die roll /
/// random discard, and `analysis::resource::elimination_bounds` stopping the count strictly short
/// of every CR 704 loss threshold. "No conditional on a prior
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
        | PinnedDecision::ManaColor { slot, .. }
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
/// this proves every pin's VALUE lies inside the slot's offered legal set at every index the
/// ACCEPTED COUNT will drive. Any violation ⇒
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

/// CR 608.2b (B1 schema reification): resolve one pinned target to its live wire-side
/// [`TargetRef`] at `iteration`, for the read-side offer schema (`build_shortcut_schema`). The
/// pinned identity IS its own singleton legal set for a fixed declinable offer (no FE
/// re-selection). `None` if the pin no longer resolves to a live legal object — the drive's
/// per-iteration [`resolve`] is the runtime CR 608.2b backstop that aborts such a broken loop.
pub(crate) fn resolve_target_ref(
    pin: &TargetPin,
    slot: &DecisionSlot,
    iteration: IterationIndex,
    state: &GameState,
) -> Option<crate::types::ability::TargetRef> {
    resolve_target(pin, slot, iteration, state)
        .ok()
        .map(concrete_to_target_ref)
}

/// CR 732.2a + CR 608.2b: the fail-closed VALUE-legality firewall for a declared shortcut.
/// Verifies every pin in `template` is a LEGAL answer to `schema` — each pin's slot is one
/// the offer exposed, and each pin's resolved value lies inside that slot's offered legal
/// set. `validated_range` bounds the iteration indices a scheduled target pin is re-resolved
/// for: every pin is validated at every index in `0..validated_range`. Supplying a range that
/// COVERS the drive is the CALLER's obligation, discharged by
/// `game::engine::shortcut_validated_range`, which reads the range off the declared count
/// rather than off the schedule's own length. EXHAUSTIVE over [`PinnedDecision`] with no
/// wildcard: `Order` (CR 603.3b trigger-ordering) is not a loop-declaration point;
/// `ConvokeTaps` must still address an exposed matching point even though its concrete taps are
/// re-bound live by `select_convoke_taps`. Runs once at declare (the board is frozen through Accept); the drive's
/// per-iteration [`resolve`] is the runtime CR 608.2b backstop.
pub fn validate_pins(
    schema: &ShortcutDecisionSchema,
    template: &DecisionTemplate,
    validated_range: IterationIndex,
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
                let DecisionPointKind::Targets {
                    legal_targets,
                    min_targets,
                    max_targets,
                    ..
                } = &point.kind
                else {
                    return Err(PinValidation::IllegalPinValue { slot: slot.clone() });
                };
                if targets.len() < *min_targets as usize || targets.len() > *max_targets as usize {
                    return Err(PinValidation::IllegalPinValue { slot: slot.clone() });
                }
                // CR 608.2b: re-resolve every target at every driven iteration index and
                // require the concrete value to be an offered legal target. A scheduled pin
                // that cannot resolve to a live legal object is itself an illegal value.
                for t in targets {
                    // CR 732.2b + CR 732.2c: NO `.max(1)` FLOOR. A shortened proposal whose
                    // new ending point is the first deviating choice — CR 732.2b's "that
                    // place becomes the new ending point" — is a ZERO-repetition accepted
                    // proposal, and CR 732.2c makes taking it mandatory, so count 0 must be
                    // representable AND validatable. A floor would validate index 0 of a
                    // range nothing drives, refusing conforming declarations. Validation is
                    // NOT disabled at 0: the slot-exposure (`UnexposedSlot`), pin-kind and
                    // cardinality checks all sit OUTSIDE this loop and still run.
                    //
                    // ⚠ SCOPE: this licenses representing and validating count 0. It does NOT
                    // claim today's Shorten path reaches here with 0 —
                    // `handle_respond_to_shortcut` realizes Shorten as a real priority window,
                    // not an auto-applied `Fixed(0)`.
                    for i in 0..validated_range {
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
                let DecisionPointKind::Mode {
                    available_modes,
                    min_modes,
                    max_modes,
                    allow_repeats,
                } = &point.kind
                else {
                    return Err(PinValidation::IllegalModeIndex { slot: slot.clone() });
                };
                if indices.len() < *min_modes as usize
                    || indices.len() > *max_modes as usize
                    || (!allow_repeats
                        && indices
                            .iter()
                            .collect::<std::collections::HashSet<_>>()
                            .len()
                            != indices.len())
                {
                    return Err(PinValidation::IllegalModeIndex { slot: slot.clone() });
                }
                for idx in indices {
                    if !available_modes.contains(idx) {
                        return Err(PinValidation::IllegalModeIndex { slot: slot.clone() });
                    }
                }
            }
            // CR 603.5 / CR 732.6 / CR 608.2d: binary/fixed choices — an exposed matching point
            // is the only legality requirement (the FE renders the value; no per-iteration value
            // set to bound against — a "may" is yes/no, a `ManaColor` is a latched constant).
            PinnedDecision::MayChoice { slot, .. } => {
                let point = schema
                    .points
                    .iter()
                    .find(|point| point.slot == *slot)
                    .ok_or_else(|| PinValidation::UnexposedSlot { slot: slot.clone() })?;
                if !matches!(point.kind, DecisionPointKind::MayChoice) {
                    return Err(PinValidation::IllegalPinValue { slot: slot.clone() });
                }
            }
            PinnedDecision::UnlessBreak { slot, .. } => {
                let point = schema
                    .points
                    .iter()
                    .find(|point| point.slot == *slot)
                    .ok_or_else(|| PinValidation::UnexposedSlot { slot: slot.clone() })?;
                if !matches!(point.kind, DecisionPointKind::UnlessBreak) {
                    return Err(PinValidation::IllegalPinValue { slot: slot.clone() });
                }
            }
            PinnedDecision::ManaColor { slot, color } => {
                let point = schema
                    .points
                    .iter()
                    .find(|point| point.slot == *slot)
                    .ok_or_else(|| PinValidation::UnexposedSlot { slot: slot.clone() })?;
                if !matches!(point.kind, DecisionPointKind::ManaColor { color: offered } if offered == *color)
                {
                    return Err(PinValidation::IllegalPinValue { slot: slot.clone() });
                }
            }
            // CR 702.51a: concrete convoke objects are rebound live, but the declaration must
            // still pin the exact exposed convoke decision slot.
            PinnedDecision::ConvokeTaps { slot } => {
                let point = schema
                    .points
                    .iter()
                    .find(|point| point.slot == *slot)
                    .ok_or_else(|| PinValidation::UnexposedSlot { slot: slot.clone() })?;
                if !matches!(point.kind, DecisionPointKind::ConvokeTaps { .. }) {
                    return Err(PinValidation::IllegalPinValue { slot: slot.clone() });
                }
            }
            // CR 603.3b: trigger-ordering pins are not loop-declaration points.
            PinnedDecision::Order { .. } => {}
        }
    }
    Ok(())
}

/// CR 732.2a: THE SINGLE AUTHORITY for *"is this declaration a legal answer to this offer's
/// schema?"* — [`predictability_gate`]'s COVERAGE half and [`validate_pins`]' VALUE half, run
/// together against a `required` slot list derived HERE from `schema.points` rather than by
/// each caller.
///
/// Three sites ask that question — `game::engine::handle_declare_shortcut` (the declare
/// firewall), `game::interaction::materialize_loop_shortcut_response` (the human ingress) and
/// `game::engine::build_bounded_declaration` (the engine's own publisher) — and a declaration
/// PUBLISHED under one predicate but ACCEPTED under another is the divergence this exists to
/// make unrepresentable: `declaration.is_some()` is read by `ai_support::candidates` as "the
/// declare handler will take this", and only a shared predicate makes that true.
///
/// # `validated_range` STAYS A PARAMETER, and that is a measurement, not a hedge
///
/// The two pre-existing call sites did NOT pass the same range, so folding one in would adopt
/// one site's semantics for the other:
///
/// * the declare firewall passes `game::engine::shortcut_validated_range(&count, template)` —
///   the range the ACCEPTED COUNT will drive;
/// * the interaction decoder passes `1`, correct by construction there because it emits only
///   ITERATION-INVARIANT pins. That is the property, stated as a property because the variant
///   list has already moved once: the decoder emits [`TargetPin::ByIdentity`] (which
///   [`resolve_target`] resolves without reading `iteration` at all) and
///   [`TargetPin::Scheduled`] carrying [`TargetSchedule::Constant`], whose arm of
///   [`evaluate_schedule`] selects its [`Ranking`] without consulting the index — unlike the
///   `RoundRobin` / `Piecewise` arms beside it, which that decoder does not emit. Its verdict
///   is therefore identical at any range ≥ 1.
///
/// Ranges are nested rather than contradictory — `0..n` re-checks are a superset of `0..m` for
/// `m <= n`, so a wider range is strictly stricter — which is why a PUBLISHER must validate at
/// the widest range it could be declared with: passing there implies passing at every count a
/// declarer may name.
///
/// # Returns `bool`, deliberately
///
/// All three callers discard the failure KIND (they already spelled `.is_err() || .is_err()`)
/// and their dispositions have nothing in common: manual-play handback via
/// `reject_shortcut_declaration`, `InteractionReasonCode::ConstraintUnsatisfied`, and "publish
/// no declaration". A union error type would have no reader. [`predictability_gate`] and
/// [`validate_pins`] stay public and typed for the rows that assert on the specific violation.
pub fn declaration_conforms(
    schema: &ShortcutDecisionSchema,
    template: &DecisionTemplate,
    validated_range: IterationIndex,
    state: &GameState,
) -> bool {
    let required: Vec<DecisionSlot> = schema.points.iter().map(|p| p.slot.clone()).collect();
    predictability_gate(template, &required).is_ok()
        && validate_pins(schema, template, validated_range, state).is_ok()
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

    /// The one-element ranking every pre-parameterization schedule site now spells: a
    /// `Ranking::one(Object(src))` IS the old `Constant(src)`, which is the migration.
    fn obj_rank(src: DecisionSource) -> Ranking {
        Ranking::one(AnnouncementSubject::Object(src))
    }

    fn seat_rank(player: PlayerId) -> Ranking {
        Ranking::one(AnnouncementSubject::Seat(player))
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
            // A NARROWED CR 732.2a bound, deliberately not the default: a round-trip that
            // carried the default would pass even if the field were dropped from the wire.
            max_iterations: 17,
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
                    min_targets: 1,
                    max_targets: 2,
                    ordered: true,
                },
            }],
            convoke_tappable_count: 2,
        };
        let json = serde_json::to_value(&schema).expect("serialize");
        assert_eq!(
            json["max_iterations"], 17,
            "the CR 732.2a bound must reach the wire — a `#[serde(default)]` field that is \
             never serialized would silently reset to the cap on every reload"
        );
        let back: ShortcutDecisionSchema = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, schema);
        assert_eq!(
            ShortcutDecisionSchema::default(),
            ShortcutDecisionSchema {
                iteration_count: IterationCount::Fixed(0),
                max_iterations: crate::game::engine::MAX_SHORTCUT_CYCLES,
                points: vec![],
                convoke_tappable_count: 0,
            }
        );
        // A pre-bound snapshot (no `max_iterations` key at all) must load at the cap, which
        // is exactly what every producer emitted before the field existed.
        let mut legacy = serde_json::to_value(ShortcutDecisionSchema::default()).unwrap();
        legacy
            .as_object_mut()
            .expect("schema serializes as an object")
            .remove("max_iterations");
        assert_eq!(
            serde_json::from_value::<ShortcutDecisionSchema>(legacy)
                .expect("a pre-bound snapshot still deserializes")
                .max_iterations,
            crate::game::engine::MAX_SHORTCUT_CYCLES
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
                    obj_rank(this_obj(20, None)),
                    obj_rank(this_obj(21, None)),
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

    /// FIX-1 (CR 608.2d): a `ManaColor` pin resolves COPY-THROUGH to `ConcreteDecision::ManaColor`
    /// with the SAME latched color and slot, at every iteration — no state lookup, never fails
    /// (a color cannot become illegal, CR 732.2a "predictable results"). Revert-probe: if
    /// `resolve_pin`'s ManaColor arm dropped the pin or mutated the color/slot, this flips.
    #[test]
    fn resolve_mana_color_pin_copies_through() {
        let state = GameState::new_two_player(7);
        let slot = DecisionSlot {
            source: this_obj(404, None),
            index: 1,
        };
        let template = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::ManaColor {
                slot: slot.clone(),
                color: ManaColor::Blue,
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(2),
            },
            key: tri_key(),
        };
        for it in 0..3 {
            let got = resolve(&template, it, &state).expect("copy-through never fails");
            assert_eq!(got.len(), 1, "iteration {it}: one resolved decision");
            assert_eq!(
                got[0],
                ConcreteDecision::ManaColor {
                    slot: slot.clone(),
                    color: ManaColor::Blue,
                },
                "iteration {it}: the latched Blue is copied through unchanged"
            );
        }
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
                    (0, obj_rank(this_obj(20, None))),
                    (2, obj_rank(this_obj(21, None))),
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
                    obj_rank(this_obj(20, None)),
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

    /// CR 800.4 + CR 102.1 + CR 608.2b: a `TargetPin::Player` aimed at a seat that has LEFT
    /// THE GAME is no longer one of the people in the game, so it is not choosable and the
    /// per-iteration re-check must raise `IllegalTarget{pin}`. Both this seam and
    /// `game::targeting`'s legal-set enumeration now SHARE that existence authority —
    /// `game::players::player_exists_for_choice`, reached here directly and there through
    /// `targeting::player_is_legal_target` — so this is one authority consulted twice, not
    /// two implementations mirroring each other. (NOT CR 800.4a, which governs a departed
    /// player's objects, control effects and priority rather than choice legality.)
    /// This row is the sole owner of `IllegalTarget{pin}` for the `Player` kind, and it is
    /// reachable by construction.
    ///
    /// MATCHED PAIR, one variable (`is_eliminated`): the LIVE half resolves, the DEAD half
    /// fails. Only the ABSOLUTE expectations discriminate — a parity assertion against the
    /// targeting side would be vacuous now that both sides call one function, since
    /// deleting a conjunct moves both together. REVERT-PROBE: drop the `!is_eliminated`
    /// conjunct inside `player_exists_for_choice` (via `is_alive`) ⇒ the dead half
    /// resolves ⇒ FAILS.
    #[test]
    fn a_dead_player_pin_is_illegal() {
        let pin_slot = DecisionSlot {
            source: this_obj(70, Some(0)),
            index: 0,
        };
        let template = |victim: u8| DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Targets {
                slot: pin_slot.clone(),
                targets: vec![TargetPin::Player(PlayerId(victim))],
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(1),
            },
            key: tri_key(),
        };

        // LIVE half — the positive reach-guard: nothing upstream of the liveness check
        // rejects this pin, so the dead half's failure is attributable to liveness alone.
        let live = GameState::new_two_player(7);
        assert!(!live.players[1].is_eliminated, "fixture: P1 starts alive");
        assert_eq!(
            resolve(&template(1), 0, &live).unwrap(),
            vec![ConcreteDecision::Targets {
                slot: pin_slot.clone(),
                targets: vec![ConcreteTarget::Player(PlayerId(1))],
            }],
            "a live player pin resolves unchanged"
        );

        // DEAD half — the identical board with exactly one field flipped.
        let mut dead = live.clone();
        dead.players[1].is_eliminated = true;
        let err = resolve(&template(1), 0, &dead).unwrap_err();
        assert_eq!(
            err,
            ReplayFailure::IllegalTarget {
                slot: pin_slot,
                pin: TargetPin::Player(PlayerId(1)),
            },
            "CR 800.4 + CR 102.1: a departed seat is no longer one of the people in the \
             game, so it is not choosable — and the failure NAMES the pin, which a \
             `source: DecisionSource` payload could not express"
        );
    }

    /// R1 — CR 115.10a + the CR 702.26b MIRROR: a `TargetPin::Player` aimed at a
    /// PHASED-OUT seat also fails the per-iteration re-check. At HEAD this seam checked
    /// `is_alive` only, so a phased-out seat resolved here and was killed (or not) further
    /// downstream; routing it through `game::players::player_exists_for_choice` makes the
    /// EXISTENCE half one authority for both halves of "no longer there".
    ///
    /// MATCHED PAIR, one variable (the phasing transition): the PHASED-IN half resolves,
    /// the PHASED-OUT half fails. The phased-in half is the positive reach-guard — it
    /// proves nothing upstream of the existence check rejects this pin, so the other
    /// half's failure is attributable to phasing alone. The transition itself is asserted
    /// (production API return value + the flag) because a setup that silently no-opped
    /// would make the second half pass for no reason at all.
    ///
    /// R1 and `a_dead_player_pin_is_illegal` are the two behaviour changes of one
    /// conjunct pair, deliberately kept as separate rows: elimination was already enforced
    /// here, phasing was not.
    ///
    /// REVERT-PROBE: drop the `is_phased_out` conjunct in `player_exists_for_choice` ⇒ the
    /// phased-out half resolves ⇒ FAILS (and `a_dead_player_pin_is_illegal` does not move,
    /// which is what attributes this row to the phasing conjunct specifically).
    #[test]
    fn a_phased_out_player_pin_is_illegal() {
        let pin_slot = DecisionSlot {
            source: this_obj(71, Some(0)),
            index: 0,
        };
        let template = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Targets {
                slot: pin_slot.clone(),
                targets: vec![TargetPin::Player(PlayerId(1))],
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(1),
            },
            key: tri_key(),
        };

        // PHASED-IN half — the positive reach-guard.
        let phased_in = GameState::new_two_player(7);
        assert!(
            !phased_in.players[1].is_phased_out(),
            "fixture: P1 starts phased in"
        );
        assert_eq!(
            resolve(&template, 0, &phased_in).unwrap(),
            vec![ConcreteDecision::Targets {
                slot: pin_slot.clone(),
                targets: vec![ConcreteTarget::Player(PlayerId(1))],
            }],
            "a phased-in player pin resolves unchanged"
        );

        // PHASED-OUT half — the same board, transitioned through the PRODUCTION API.
        let mut phased_out = phased_in.clone();
        let mut events = Vec::new();
        let transitioned =
            crate::game::phasing::phase_out_player(&mut phased_out, PlayerId(1), &mut events);
        assert_eq!(
            transitioned,
            vec![PlayerId(1)],
            "setup anti-vacuity: phase_out_player must report the seat it transitioned"
        );
        assert!(
            phased_out.players[1].is_phased_out(),
            "setup anti-vacuity: P1 must read as phased out"
        );
        assert!(
            !phased_out.players[1].is_eliminated,
            "the ONLY variable is phasing — P1 must still be un-eliminated, or this row \
             would be a second copy of `a_dead_player_pin_is_illegal`"
        );

        assert_eq!(
            resolve(&template, 0, &phased_out).unwrap_err(),
            ReplayFailure::IllegalTarget {
                slot: pin_slot,
                pin: TargetPin::Player(PlayerId(1)),
            },
            "CR 702.26b MIRROR: a phased-out seat is treated as though it does not exist, \
             so it is not choosable here either"
        );
    }

    /// R2 — CR 115.10a is a BOUNDARY, and this row is what stops a future "fix" from
    /// erasing it: a targeting-only exclusion gates the TARGET seam and must NOT gate the
    /// CHOICE seam.
    ///
    /// One board, one SHROUDED seat (CR 702.18a — shroud blocks every source, including
    /// the shrouded player's own, so the assertion does not depend on who the source
    /// controller is), and two assertions at two different seams:
    ///
    /// 1. the EXCLUDE half — `targeting::find_legal_targets` drops the seat. This is the
    ///    paired POSITIVE: it proves the shroud grant actually took effect, so assertion 2
    ///    is about the seam boundary and not about a setup that silently did nothing.
    /// 2. the ADMIT half — `resolve_target`'s `TargetPin::Player` arm still resolves it,
    ///    because a proliferate choice (CR 701.34a) is not a target and refusing it here
    ///    is the over-veto class this phase exists to remove.
    ///
    /// REVERT-PROBE: route the `TargetPin::Player` arm through
    /// `targeting::player_is_legal_target` instead of `players::player_exists_for_choice`
    /// ⇒ assertion 2 FAILS while assertion 1 still passes.
    #[test]
    fn a_shrouded_seat_is_untargetable_yet_still_choosable_at_the_pin_recheck() {
        use crate::types::ability::{
            ControllerRef, StaticDefinition, TargetFilter, TargetRef, TypedFilter,
        };
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);

        // P1 gains shroud from a permanent they control ("You have shroud"). Built with
        // the production `zones::create_object`, not a raw `objects.insert`: a raw insert
        // never joins `state.battlefield`, so `game_functioning_statics` would not see the
        // grantor and the shroud would silently never apply.
        let grantor = crate::game::zones::create_object(
            &mut state,
            CardId(90),
            PlayerId(1),
            "You Have Shroud Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&grantor).unwrap().static_definitions =
            vec![
                StaticDefinition::new(StaticMode::Shroud).affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            ]
            .into();
        crate::game::layers::flush_layers(&mut state);

        // 1. EXCLUDE half, and the setup's positive control: the TARGET seam drops P1.
        let source = crate::game::zones::create_object(
            &mut state,
            CardId(91),
            PlayerId(0),
            "Targeting Spell".to_string(),
            Zone::Battlefield,
        );
        let targets = crate::game::targeting::find_legal_targets(
            &state,
            &TargetFilter::Any,
            PlayerId(0),
            source,
        );
        assert!(
            !targets.contains(&TargetRef::Player(PlayerId(1))),
            "CR 702.18a: the shroud grant must actually bite at the TARGET seam — if it \
             does not, assertion 2 below proves nothing. Got {targets:?}"
        );
        assert!(
            targets.contains(&TargetRef::Player(PlayerId(0))),
            "the un-shrouded seat is still targetable, so the exclusion above is shroud \
             and not an empty legal set"
        );

        // 2. ADMIT half: the CHOICE seam still resolves the same seat.
        let pin_slot = DecisionSlot {
            source: this_obj(92, None),
            index: 0,
        };
        let template = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Targets {
                slot: pin_slot.clone(),
                targets: vec![TargetPin::Player(PlayerId(1))],
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(1),
            },
            key: tri_key(),
        };
        assert_eq!(
            resolve(&template, 0, &state).unwrap(),
            vec![ConcreteDecision::Targets {
                slot: pin_slot,
                targets: vec![ConcreteTarget::Player(PlayerId(1))],
            }],
            "CR 115.10a: a CHOSEN seat is not a TARGETED seat — the targeting-only \
             exclusions must not reach this seam"
        );
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
            TargetSchedule::Constant(obj_rank(this_obj(1, None))),
            TargetSchedule::RoundRobin(vec![obj_rank(this_obj(1, None))]),
            TargetSchedule::Piecewise(vec![(0, obj_rank(this_obj(1, None)))]),
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
    /// green creatures ⇒ `ConcreteDecision::ConvokeTaps` with the minimal set (both nontoken,
    /// so `DetectionFodderFirst`'s tie-break collapses to lowest-id here).
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

    // ── item-4 R1 — the parameterized announcement subject (`Ranking`) ──

    /// Insert an object into an arbitrary zone. `bf_object` above is battlefield-only, and
    /// rows R1-h/i need the CR 114.4 command zone and the graveyard.
    fn zoned_object(state: &mut GameState, id: u64, zone: Zone) -> ObjectId {
        let oid = ObjectId(id);
        let mut o = GameObject::new(
            oid,
            CardId(id),
            PlayerId(0),
            "Ability Source".to_string(),
            zone,
        );
        o.incarnation = 3;
        state.objects.insert(oid, o);
        oid
    }

    /// CR 702.11c: give `player` hexproof through the transient-grant path
    /// `static_abilities::player_has_hexproof` already reads
    /// (`transient_grants_static_mode_to_player`). No layer pass is needed — that reader
    /// scans `transient_continuous_effects` directly.
    fn grant_player_hexproof(state: &mut GameState, player: PlayerId) {
        use crate::types::ability::{ContinuousModification, Duration, TargetFilter};
        use crate::types::statics::StaticMode;
        state.add_transient_continuous_effect(
            ObjectId(9001),
            player,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: player },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::Hexproof,
            }],
            None,
        );
    }

    /// One `Targets` pin carrying one `Scheduled` pin, slotted on `slot_source`.
    fn ranked_template(slot_source: DecisionSource, sched: TargetSchedule) -> DecisionTemplate {
        DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Targets {
                slot: DecisionSlot {
                    source: slot_source,
                    index: 0,
                },
                targets: vec![TargetPin::Scheduled(sched)],
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(1),
            },
            key: tri_key(),
        }
    }

    fn sole_target(out: &[ConcreteDecision]) -> ConcreteTarget {
        match &out[0] {
            ConcreteDecision::Targets { targets, .. } => targets[0],
            other => panic!("expected Targets, got {other:?}"),
        }
    }

    /// **Row R1-a — the maintained-invariant equality row.** A one-element
    /// `Constant(Ranking::one(Object(src)))` is behaviour-identical to the pre-parameterization
    /// `Constant(src)`, and the HEAD is what every variant resolves.
    ///
    /// # Non-vacuity / discrimination
    ///
    /// The one-element half alone is satisfied by ANY entry-selection rule — first, last,
    /// random — because on a one-element list they coincide. The paired reach-guard is
    /// therefore a TWO-entry ranking whose head and tail are BOTH legal live objects: only a
    /// head-selecting reader answers the head there.
    ///
    /// REVERT-PROBE: make `Ranking::head` return the LAST entry (`self.0.last().unwrap()`) ⇒
    /// every two-entry assertion below flips to ObjectId(21) ⇒ FAILS, on all three
    /// `TargetSchedule` variants. The one-element assertions stay green under that mutation,
    /// which is exactly why they are not the discriminator.
    #[test]
    fn r1a_a_one_element_ranking_is_the_old_constant_and_every_variant_resolves_its_head() {
        let mut state = GameState::new_two_player(7);
        bf_object(&mut state, 20, 20, 0);
        bf_object(&mut state, 21, 21, 0);
        let (a, b) = (this_obj(20, None), this_obj(21, None));
        let slot_src = this_obj(99, None);

        // ── the equality half: one element behaves as the old constant subject ──
        let one = ranked_template(
            slot_src.clone(),
            TargetSchedule::Constant(obj_rank(a.clone())),
        );
        assert_eq!(
            sole_target(&resolve(&one, 0, &state).expect("a live battlefield head resolves")),
            ConcreteTarget::Object(ObjectId(20)),
            "a one-element ranking resolves exactly what `Constant(src)` resolved"
        );

        // ── the reach-guard: BOTH entries live, so only head-selection answers 20 ──
        let two = Ranking::new(vec![
            AnnouncementSubject::Object(a.clone()),
            AnnouncementSubject::Object(b.clone()),
        ])
        .expect("two distinct subjects are a legal ranking");
        for (label, sched) in [
            ("Constant", TargetSchedule::Constant(two.clone())),
            ("RoundRobin", TargetSchedule::RoundRobin(vec![two.clone()])),
            (
                "Piecewise",
                TargetSchedule::Piecewise(vec![(0, two.clone())]),
            ),
        ] {
            let template = ranked_template(slot_src.clone(), sched);
            let out = resolve(&template, 0, &state).expect("the head is a live object");
            assert_eq!(
                sole_target(&out),
                ConcreteTarget::Object(ObjectId(20)),
                "{label}: with BOTH entries legal, the step resolves its ranking's HEAD — a \
                 last-entry (or any-entry) reader answers 21 here"
            );
        }

        // Attribution control: the tail IS reachable as a head, so 21 is not simply
        // unresolvable on this board.
        let tail_first = ranked_template(slot_src, TargetSchedule::Constant(obj_rank(b)));
        assert_eq!(
            sole_target(&resolve(&tail_first, 0, &state).expect("21 is live too")),
            ConcreteTarget::Object(ObjectId(21)),
            "the tail entry resolves fine when it IS the head — the row measures POSITION, not \
             a dead object"
        );
    }

    /// **Row R1-b — the head-only discriminator (CR 732.2a).** An illegal head is
    /// `IllegalTarget` even when a later entry is perfectly legal. Skipping to that later
    /// entry would be the conditional action CR 732.2a bars ("the outcome of a game event
    /// determines the next action a player takes"), and it is the load-bearing guard for the
    /// whole cross-episode consumption model: a ranking advances only at an episode boundary.
    ///
    /// Both subject arms are exercised, because they fail through DIFFERENT predicates:
    /// `Object` through `resolve_source`'s `None`, `Seat` through `player_is_legal_target`'s
    /// `false` (CR 702.11c hexproof — an existence-only check would let it through).
    ///
    /// # Non-vacuity / discrimination
    ///
    /// An `IllegalTarget` is also what a wholly broken resolver returns, so each arm pairs
    /// with the SAME ranking reordered to put the legal entry first, which must RESOLVE.
    ///
    /// REVERT-PROBE: implement first-legal-wins in `evaluate_schedule`
    /// (`ranking.iter().find_map(..)` instead of `head()`) ⇒ both refusals below resolve to
    /// the second entry ⇒ FAILS twice, while both positives stay green.
    #[test]
    fn r1b_an_illegal_head_refuses_even_when_a_later_entry_is_legal() {
        let mut state = GameState::new_two_player(7);
        bf_object(&mut state, 20, 20, 0); // the live object
        let live_src = this_obj(20, None);
        let absent_src = this_obj(u64::MAX, None); // never inserted ⇒ resolve_source None
        let slot_src = this_obj(20, None); // a live battlefield ability instance

        // ── the OBJECT arm ──
        let head_dead = Ranking::new(vec![
            AnnouncementSubject::Object(absent_src.clone()),
            AnnouncementSubject::Object(live_src.clone()),
        ])
        .expect("legal ranking");
        assert!(
            matches!(
                resolve(
                    &ranked_template(slot_src.clone(), TargetSchedule::Constant(head_dead)),
                    0,
                    &state
                ),
                Err(ReplayFailure::IllegalTarget { .. })
            ),
            "CR 732.2a: a dead HEAD refuses — it must NOT skip to the live tail"
        );
        let head_live = Ranking::new(vec![
            AnnouncementSubject::Object(live_src),
            AnnouncementSubject::Object(absent_src.clone()),
        ])
        .expect("legal ranking");
        assert_eq!(
            sole_target(
                &resolve(
                    &ranked_template(slot_src.clone(), TargetSchedule::Constant(head_live)),
                    0,
                    &state
                )
                .expect("the SAME two subjects, legal one first, resolve")
            ),
            ConcreteTarget::Object(ObjectId(20)),
            "paired positive: the identical pair with the LIVE entry first resolves — the \
             refusal above is caused by POSITION, not by the resolver being broken"
        );

        // ── the SEAT arm: hexproof, so the head is illegal as a TARGET while existing ──
        grant_player_hexproof(&mut state, PlayerId(1));
        let head_hexproofed = Ranking::new(vec![
            AnnouncementSubject::Seat(PlayerId(1)),
            AnnouncementSubject::Seat(PlayerId(0)),
        ])
        .expect("legal ranking");
        assert!(
            matches!(
                resolve(
                    &ranked_template(slot_src.clone(), TargetSchedule::Constant(head_hexproofed)),
                    0,
                    &state
                ),
                Err(ReplayFailure::IllegalTarget { .. })
            ),
            "CR 702.11c: a hexproofed HEAD refuses — and it EXISTS, so an existence-only \
             authority (`player_exists_for_choice`) would have resolved it"
        );
        let head_legal_seat = Ranking::new(vec![
            AnnouncementSubject::Seat(PlayerId(0)),
            AnnouncementSubject::Seat(PlayerId(1)),
        ])
        .expect("legal ranking");
        assert_eq!(
            sole_target(
                &resolve(
                    &ranked_template(slot_src, TargetSchedule::Constant(head_legal_seat)),
                    0,
                    &state
                )
                .expect("the SAME two seats, legal one first, resolve")
            ),
            ConcreteTarget::Player(PlayerId(0)),
            "paired positive: seats DO resolve on this board (the source's own controller is \
             not an opponent, so CR 702.11c does not bite) — so the refusal above is the \
             hexproof, not a seat arm that never resolves"
        );
    }

    /// **Row R1-d — multi-authority.** Two ranked slots on ONE source are resolved
    /// INDEPENDENTLY; neither inherits the other's answer.
    ///
    /// # Non-vacuity / discrimination
    ///
    /// Arm A gives the two slots DIFFERENT legal seats, so a resolver that answered once and
    /// reused the answer produces two identical targets and fails the vector comparison. Arm B
    /// makes slot 1's seat hexproofed while slot 0's stays legal: the whole-template resolve
    /// must fail NAMING SLOT 1, and slot 0 alone must still resolve — a copied answer would
    /// have made slot 1 succeed.
    ///
    /// REVERT-PROBE: resolve once and reuse across slots ⇒ arm A's two answers agree ⇒ FAILS,
    /// and arm B stops refusing ⇒ FAILS.
    #[test]
    fn r1d_two_ranked_slots_on_one_source_do_not_inherit_each_others_answer() {
        // A 3-seat board so slot 0 and slot 1 name two DIFFERENT seats, neither of them the
        // source's own controller — the shape the plan's fixture specifies.
        let mut state = crate::game::scenario::GameScenario::new_n_player(3, 7)
            .build()
            .state()
            .clone();
        bf_object(&mut state, 20, 20, 0);
        let src = this_obj(20, None);
        let slot_at = |index: u8| DecisionSlot {
            source: src.clone(),
            index,
        };
        let pin_at = |index: u8, seat: PlayerId| PinnedDecision::Targets {
            slot: slot_at(index),
            targets: vec![TargetPin::Scheduled(TargetSchedule::Constant(seat_rank(
                seat,
            )))],
        };
        let template_of = |pins: Vec<PinnedDecision>| DecisionTemplate {
            owner: PlayerId(0),
            decisions: pins,
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(1),
            },
            key: tri_key(),
        };

        // ── arm A: two DIFFERENT legal seats ⇒ two DIFFERENT answers ──
        let both = template_of(vec![pin_at(0, PlayerId(1)), pin_at(1, PlayerId(2))]);
        let out = resolve(&both, 0, &state).expect("both seats are legal targets here");
        let answers: Vec<ConcreteTarget> = out
            .iter()
            .map(|d| match d {
                ConcreteDecision::Targets { targets, .. } => targets[0],
                other => panic!("expected Targets, got {other:?}"),
            })
            .collect();
        assert_eq!(
            answers,
            vec![
                ConcreteTarget::Player(PlayerId(1)),
                ConcreteTarget::Player(PlayerId(2))
            ],
            "each slot resolves its OWN ranking's head — a resolve-once-and-reuse \
             implementation answers PlayerId(1) twice"
        );

        // ── arm B: slot 1's seat is hexproofed; slot 0's stays legal ──
        grant_player_hexproof(&mut state, PlayerId(2));
        let err = resolve(&both, 0, &state)
            .expect_err("slot 1's seat is now an illegal TARGET (CR 702.11c)");
        match err {
            ReplayFailure::IllegalTarget { slot, .. } => assert_eq!(
                slot,
                slot_at(1),
                "the failure names SLOT 1 — slot 0's success was not copied onto it"
            ),
            other => panic!("expected IllegalTarget, got {other:?}"),
        }
        // Paired positives: each slot ALONE answers the way the combined run says it does.
        assert_eq!(
            sole_target(
                &resolve(&template_of(vec![pin_at(0, PlayerId(1))]), 0, &state)
                    .expect("slot 0 alone still resolves")
            ),
            ConcreteTarget::Player(PlayerId(1)),
            "slot 0 is unaffected by slot 1's refusal"
        );
        assert!(
            matches!(
                resolve(&template_of(vec![pin_at(1, PlayerId(2))]), 0, &state),
                Err(ReplayFailure::IllegalTarget { .. })
            ),
            "slot 1 alone refuses — so the combined refusal is slot 1's own verdict"
        );
    }

    /// **Row R1-f — the LOAD-seam invariant.** A wire-supplied empty or duplicated ranking
    /// fails deserialization, which is what makes [`Ranking::head`] infallible: no `Option`
    /// and no panic path leak into the resolver. Same class as the wire-sourced
    /// `max_iterations` defect `reject_zero_bound_shortcut_offer` closes.
    ///
    /// # Non-vacuity / discrimination
    ///
    /// The paired positive is a VALID two-entry ranking round-tripping equal — without it a
    /// `Deserialize` impl that rejected everything would satisfy both negatives.
    ///
    /// REVERT-PROBE: drop `#[serde(try_from = "Vec<AnnouncementSubject>")]` from `Ranking` ⇒
    /// `[]` deserializes into `Ranking(vec![])` ⇒ this row's `is_err()` FAILS (and `head()`
    /// on that value would panic in production rather than refuse).
    #[test]
    fn r1f_an_empty_or_duplicated_ranking_fails_the_load() {
        let seat = |p: u8| AnnouncementSubject::Seat(PlayerId(p));

        assert!(
            serde_json::from_str::<Ranking>("[]").is_err(),
            "an empty ranking names nobody — it must not survive the load seam"
        );

        let duplicated =
            serde_json::to_string(&vec![seat(1), seat(1)]).expect("the raw list serializes");
        assert!(
            serde_json::from_str::<Ranking>(&duplicated).is_err(),
            "CR 601.2c: a repeated subject is the same declaration twice, not an ordering"
        );

        // Paired positive: a legal ranking round-trips equal, so the refusals above are the
        // invariant and not a broken codec.
        let valid = Ranking::new(vec![seat(1), seat(0)]).expect("distinct subjects");
        let json = serde_json::to_string(&valid).expect("serialize");
        assert_eq!(
            serde_json::from_str::<Ranking>(&json).expect("a legal ranking round-trips"),
            valid,
            "the newtype serializes as its inner list and reloads through the checked TryFrom"
        );
        assert_eq!(
            json, r#"[{"Seat":1},{"Seat":0}]"#,
            "and the wire shape IS the bare list — the `try_from` shim adds no envelope"
        );

        // The constructor's own two clauses, named (the `TryFrom` above delegates here).
        assert_eq!(Ranking::new(vec![]).unwrap_err(), RankingError::Empty);
        assert_eq!(
            Ranking::new(vec![seat(1), seat(1)]).unwrap_err(),
            RankingError::DuplicateSubject
        );
    }

    /// **Rows R1-g / R1-h / R1-i — the `Seat` arm's SOURCE, one instrument, three zones.**
    ///
    /// A ranked `Seat` is a TARGET (CR 601.2c), so `player_is_legal_target` needs the ABILITY
    /// INSTANCE that would name it (CR 702.11c is source-controller-relative; CR 702.16b reads
    /// the source's characteristics). That is `resolve_ability_instance`, NOT `resolve_source`:
    ///
    /// * **R1-g, battlefield** — resolves. The control arm; the only one of the three that can
    ///   fail for a boring reason (a dead harness).
    /// * **R1-h, command zone** — resolves. CR 114.2 puts an emblem — "both owned and
    ///   controlled by that player" — there, and CR 114.4 is why its abilities function from
    ///   there (CR 113.6p for the plane / scheme / conspiracy members of the same class).
    ///   `crates/engine/tests/fixtures/dellian_emblem_conqueror_4p.json.gz` is a real board
    ///   whose published `Targets` point names exactly such a source.
    /// * **R1-i, graveyard** — refuses. With no live ability instance the engine cannot certify
    ///   that the object it would ask the CR 702.11c question about still IS that instance
    ///   (CR 400.7 / CR 608.2b). The seat still EXISTS and a graveyard object still carries a
    ///   `controller`, so the question is answerable — what is missing is the certification,
    ///   and CR 732.1 + CR 732.2a make refusing free — "may suggest" is a permission, not an
    ///   obligation (no declaration ⇒ the table plays it out manually).
    ///
    /// # Non-vacuity / discrimination
    ///
    /// R1-g and R1-i must come out the OTHER way on the SAME instrument in the same run: one
    /// resolving and one refusing is what proves the harness reports both values. R1-h is the
    /// row that discriminates this specification from plain fail-closed-on-`resolve_source`.
    ///
    /// REVERT-PROBES: (a) derive the arm from `resolve_source` alone ⇒ the command-zone case
    /// resolves `None` ⇒ **R1-h FAILS**; (b) break the battlefield disjunct ⇒ R1-g FAILS;
    /// (c) fall back to `player_exists_for_choice` when the accessor is `None` ⇒ the graveyard
    /// seat resolves ⇒ **R1-i FAILS**.
    ///
    /// These three SAMPLE the zone space; they do not enumerate it. Exile, a stale CR 400.7
    /// incarnation and a different object are pinned on one board by the shipped row
    /// `game::engine::command_zone_sourced_slot_matches_and_graveyard_still_aborts` (row R1-l),
    /// whose subject `slot_source_prompted` now delegates to `resolve_ability_instance` — so
    /// that coverage transfers to this accessor rather than being skipped.
    #[test]
    fn r1ghi_a_ranked_seat_resolves_from_battlefield_and_command_but_fails_closed_elsewhere() {
        let mut state = GameState::new_two_player(7);
        let battlefield = zoned_object(&mut state, 900, Zone::Battlefield);
        let emblem = zoned_object(&mut state, 901, Zone::Command);
        let graveyard = zoned_object(&mut state, 902, Zone::Graveyard);

        let seat = PlayerId(1);
        let resolve_from = |src: DecisionSource, state: &GameState| {
            resolve(
                &ranked_template(src, TargetSchedule::Constant(seat_rank(seat))),
                0,
                state,
            )
        };

        // R1-g: battlefield ⇒ resolves.
        assert_eq!(
            sole_target(
                &resolve_from(this_obj(battlefield.0, Some(3)), &state)
                    .expect("a live battlefield ability instance certifies the seat")
            ),
            ConcreteTarget::Player(seat),
            "R1-g: the control arm resolves — the instrument can return a target"
        );

        // R1-h: CR 114.4 / CR 113.6p command zone ⇒ resolves.
        assert_eq!(
            sole_target(
                &resolve_from(this_obj(emblem.0, Some(3)), &state)
                    .expect("CR 114.4: an emblem's abilities function in the command zone")
            ),
            ConcreteTarget::Player(seat),
            "R1-h: a `resolve_source`-derived arm answers None here and would refuse the \
             emblem loop the drive built a CR 114.4 / CR 113.6p disjunct FOR"
        );

        // R1-i: graveyard ⇒ fails closed.
        assert!(
            matches!(
                resolve_from(this_obj(graveyard.0, Some(3)), &state),
                Err(ReplayFailure::IllegalTarget { .. })
            ),
            "R1-i: no live ability instance ⇒ refuse. The SEAT is fine — R1-g resolved it one \
             assertion ago on this same board — so the refusal is caused by the ZONE"
        );

        // Sibling agreement: an OBJECT head on that same dead source refuses too, so the two
        // subject arms say the same thing about a source that is gone.
        assert!(
            matches!(
                resolve(
                    &ranked_template(
                        this_obj(graveyard.0, Some(3)),
                        TargetSchedule::Constant(obj_rank(this_obj(graveyard.0, Some(3))))
                    ),
                    0,
                    &state
                ),
                Err(ReplayFailure::IllegalTarget { .. })
            ),
            "the `Object` arm refuses a graveyard subject too (CR 608.2b) — the two arms agree"
        );

        // CR 400.7, so the command disjunct is not a blanket zone exemption: a stale
        // incarnation on the SAME emblem refuses.
        assert!(
            matches!(
                resolve_from(this_obj(emblem.0, Some(2)), &state),
                Err(ReplayFailure::IllegalTarget { .. })
            ),
            "CR 400.7: the command arm re-binds ONE incarnation, exactly like the battlefield \
             arm — a re-created emblem does not certify the old pin"
        );
    }

    /// **T4 — an `Order` pin resolves from the command zone through the PUBLIC [`resolve`],
    /// and still fails closed everywhere else.**
    ///
    /// CR 603.3b is the pin's framing (replay this source's trigger at its pinned ordering
    /// position); what changed is that "this source" is now re-bound by
    /// [`resolve_ability_instance`] — same identity, same CR 400.7 incarnation, present in a
    /// zone it admits (`Battlefield`, plus `Command` for a `ThisObject` source) — rather than
    /// by `resolve_source`'s battlefield-only filter.
    ///
    /// **Resolving an `Order` pin grants NO capability.** No production consumer of
    /// `resolve`'s output reads an `Order` element's payload: four element readers
    /// discriminate on the variant and skip it, the `ManaPayment` beat aborts on its mere
    /// presence, and the per-cycle re-check reads only the `is_err()` verdict. What the
    /// re-bind buys is that a command-zone `Order` pin stops discarding every OTHER pin's
    /// answer in the same template — `resolve` is a per-pin `Result` collect. That payoff and
    /// its bound are pinned by
    /// `game::engine::stage2_injector_tests::a_command_zone_order_pin_stops_poisoning_the_template_without_gaining_capability`,
    /// whose five rows are the measurement; this row measures only the re-bind itself.
    ///
    /// # Non-vacuity / discrimination
    ///
    /// Rows a and b are one field apart (the zone) and both `Ok`; rows b/c and b/d are one
    /// field apart and OPPOSITE; rows e and f are one field apart — the SAME object, moved —
    /// and OPPOSITE. Each negative row asserts its subject exists in the intended state
    /// before the negative assertion.
    ///
    /// REVERT-PROBES: revert the `Order` arm to `resolve_source` ⇒ row **b** fails alone;
    /// widen the accessor's command disjunct to any zone ⇒ row **c** fails; drop the CR 400.7
    /// incarnation conjunct ⇒ row **d** fails; widen the `AllCopies` arm to the command zone
    /// ⇒ row **f** fails.
    #[test]
    fn an_order_pin_resolves_from_the_command_zone_and_still_fails_closed_elsewhere() {
        let mut state = GameState::new_two_player(7);
        let battlefield = zoned_object(&mut state, 900, Zone::Battlefield);
        let command = zoned_object(&mut state, 901, Zone::Command);
        let graveyard = zoned_object(&mut state, 902, Zone::Graveyard);
        let copy = zoned_object(&mut state, 910, Zone::Battlefield);

        let template = |source: DecisionSource| DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![PinnedDecision::Order { source, pos: 0 }],
            replay: ReplayMode::Static,
            key: tri_key(),
        };
        let order_source = |out: &[ConcreteDecision]| match out {
            [ConcreteDecision::Order { source, .. }] => *source,
            other => panic!("expected exactly one Order decision, got {other:?}"),
        };

        // row a — the shipped battlefield arm, and the control that `resolve` can answer Ok.
        assert_eq!(
            order_source(
                &resolve(&template(this_obj(battlefield.0, Some(3))), 0, &state)
                    .expect("row a: a live battlefield source still re-binds")
            ),
            battlefield,
            "row a: control"
        );

        // row b — THE FIX. One field from a: the source's zone.
        assert_eq!(
            order_source(
                &resolve(&template(this_obj(command.0, Some(3))), 0, &state).expect(
                    "row b: CR 114.4 — an ability functioning in the command zone \
                             re-binds there"
                )
            ),
            command,
            "row b: the CR 603.3b ordering pin no longer needs its source on the battlefield"
        );

        // row c — one field from b: the zone again, the other way.
        assert_eq!(
            state
                .objects
                .get(&graveyard)
                .expect("reach-guard: row c's source object was built")
                .zone,
            Zone::Graveyard,
            "reach-guard: row c's source exists and is in the graveyard, so its failure is \
             about the ZONE and not about an absent object"
        );
        assert!(
            matches!(
                resolve(&template(this_obj(graveyard.0, Some(3))), 0, &state),
                Err(ReplayFailure::MissingSource { .. })
            ),
            "row c: the zone set is {{Battlefield, Command}} and nothing else"
        );

        // row d — one field from b: the pinned CR 400.7 incarnation.
        assert_ne!(
            state
                .objects
                .get(&command)
                .expect("reach-guard: row d's source object was built")
                .incarnation,
            2,
            "reach-guard: the LIVE incarnation differs from the pinned one"
        );
        assert!(
            matches!(
                resolve(&template(this_obj(command.0, Some(2))), 0, &state),
                Err(ReplayFailure::MissingSource { .. })
            ),
            "row d: CR 400.7 — a re-created source is a new object with no relation to the \
             pinned one, in the command zone exactly as on the battlefield"
        );

        // rows e/f — ONE object, moved. The widening is `ThisObject`-only, so a
        // card-identity-spelled command-zone source still fails closed: the disclosed
        // residual, in executable form.
        let by_card = YieldTarget::AllCopies {
            card_id: CardId(910),
            trigger_description: None,
        };
        assert_eq!(
            order_source(
                &resolve(&template(by_card.clone()), 0, &state)
                    .expect("row e: the card's only copy is on the battlefield")
            ),
            copy,
            "row e: the `AllCopies` arm's control positive"
        );
        state
            .objects
            .get_mut(&copy)
            .expect("reach-guard: row f moves the SAME object row e just resolved")
            .zone = Zone::Command;
        assert_eq!(
            state
                .objects
                .get(&copy)
                .expect("reach-guard: the object still exists after the move")
                .card_id,
            CardId(910),
            "reach-guard: row f differs from row e in ZONE ONLY — same object, same card id"
        );
        assert!(
            matches!(
                resolve(&template(by_card), 0, &state),
                Err(ReplayFailure::MissingSource { .. })
            ),
            "row f: DISCLOSED RESIDUAL — the command disjunct is `ThisObject`-only, so a \
             command-zone source named by CARD identity (a conspiracy, an Eminence \
             commander) still fails closed. Disclosed, not closed."
        );
    }
}
