//! Candidate validation pipeline.
//!
//! A [`CandidateFilter`] rejects [`CandidateAction`]s that cannot legally be
//! performed in the current [`GameState`]. Filters are ordered cheapest-first:
//! trivial structural checks (zone legality, choice-index bounds) run before
//! the expensive catch-all that simulates the action via
//! `apply_as_current` and clones the full state.
//!
//! # Invariant — `cheap ⊆ simulate`
//!
//! Every cheap filter MUST be a subset of what [`SimulationFilter`] rejects:
//!
//! ```text
//! cheap.accept(state, candidate) == false
//!   ⇒
//! SimulationFilter.accept(state, candidate) == false
//! ```
//!
//! Without this property, a cheap filter could silently drop a candidate that
//! the simulation would accept — a correctness bug that surfaces as the AI
//! refusing to take legal actions. The property is enforced by the proptest
//! in the tests module.
//!
//! # Scope guardrail
//!
//! `CandidateFilter` validates candidate **actions** — it does not validate
//! target legality (that belongs in `game::targeting`) or replacement-effect
//! selection (that belongs in `game::replacement`). Adding game-rule verbs
//! ("resolve_trigger", "apply_damage") here is out of scope; this is a
//! structural legality pipeline, not a rules engine.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::game::combat::AttackTarget;
use crate::game::engine::{apply_as_current_for_legality, SimulationProbeGuard};
use crate::game::functioning_abilities::game_functioning_statics;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, FilterProp, ParitySource,
    ParsedCondition, QuantityExpr, ReplacementDefinition, ResolvedAbility, StaticDefinition,
    TargetFilter, TargetRef, TriggerDefinition,
};
use crate::types::actions::GameAction;
use crate::types::card_type::CardType;
use crate::types::counter::CounterType;
use crate::types::definitions::Definitions;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use crate::game::game_object::{PhaseStatus, RoomUnlockState};

use super::CandidateAction;

/// A filter's approximate computational cost. The pipeline runs filters in
/// ascending cost order so the cheapest rejection wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FilterCost {
    /// Constant-time structural check (e.g., object-id existence, index bounds).
    Trivial,
    /// Bounded lookup (e.g., iterate known-small lists).
    Cheap,
    /// Requires cloning `GameState` and running `apply_as_current`.
    Expensive,
}

/// Rejects candidates that can't legally be performed in the current state.
pub trait CandidateFilter {
    /// Human-readable filter name for tracing/instrumentation.
    fn name(&self) -> &'static str;

    /// Approximate cost. Used to order the pipeline; cheaper first.
    fn cost(&self) -> FilterCost;

    /// Return `true` to accept the candidate, `false` to reject.
    fn accept(&self, state: &GameState, candidate: &CandidateAction) -> bool;
}

/// Structural legality check wrapping [`super::cheap_reject_candidate`].
///
/// Covers:
/// - CR 117.1: priority ownership (only the player with priority may act).
/// - CR 400: zone presence (the acting object/card must still exist in its
///   expected zone; a permanent removed mid-resolution cannot be activated).
/// - CR 601.2: casting/activation announcement steps (mode count, phyrexian
///   shard choices, modal bounds, target-set bounds).
/// - CR 508/509: combat declarations (attackers/blockers must be valid).
///
/// This is the current workhorse. A future follow-up should decompose it into
/// the four named sub-filters originally planned: `ManaAvailabilityFilter`,
/// `ZoneLegalityFilter`, `TargetCountFilter`, `RestrictionFilter`. The
/// `cheap ⊆ sim` invariant guards the decomposition — any split that
/// over-rejects a candidate the simulation accepts will fail the proptest.
pub struct BasicLegalityFilter;

impl CandidateFilter for BasicLegalityFilter {
    fn name(&self) -> &'static str {
        "BasicLegality"
    }

    fn cost(&self) -> FilterCost {
        FilterCost::Cheap
    }

    fn accept(&self, state: &GameState, candidate: &CandidateAction) -> bool {
        !super::cheap_reject_candidate(state, &candidate.action)
    }
}

/// Catch-all fallback: clones the state and runs `apply_as_current`, accepting
/// candidates that produce no error. This is the authoritative oracle — every
/// cheap filter must be a subset of what this rejects. Only reached when all
/// cheap filters accept.
pub struct SimulationFilter;

impl CandidateFilter for SimulationFilter {
    fn name(&self) -> &'static str {
        "Simulation"
    }

    fn cost(&self) -> FilterCost {
        FilterCost::Expensive
    }

    fn accept(&self, state: &GameState, candidate: &CandidateAction) -> bool {
        if super::structurally_valid_tap_for_convoke_payment(state, &candidate.action) {
            return true;
        }
        crate::game::perf_counters::record_state_clone_for_legality();
        let mut sim = state.clone();
        // PR-3 Defect-2: mark the entire nested clone-and-apply as a legality probe so
        // the top-level-only loop-shortcut detection (`reconcile_terminal_result` §3)
        // and ring accumulation (`pass_priority_once_with_pipeline` §2) are suppressed
        // inside it. The guard restores the previous flag on drop (panic-safe, nesting-
        // correct), terminating the §3→§9→legal_actions→SimulationFilter recursion.
        let _probe = SimulationProbeGuard::enter();
        // Legality-only probe (#4479): `sim` is discarded, so skip display derivation
        // (the O(N^2) mana-availability board sweep on go-wide token boards).
        apply_as_current_for_legality(&mut sim, candidate.action.clone()).is_ok()
    }
}

/// A pipeline of filters run in the order they're registered. Candidates pass
/// only if every filter in the pipeline accepts them; first rejection wins.
///
/// The default pipeline is the right choice for all current callers. Custom
/// pipelines are a future extension point (e.g., MCTS rollouts that want to
/// skip `SimulationFilter` and accept optimistic candidates).
pub struct FilterPipeline {
    filters: Vec<Box<dyn CandidateFilter + Send + Sync>>,
}

impl FilterPipeline {
    pub fn new(filters: Vec<Box<dyn CandidateFilter + Send + Sync>>) -> Self {
        Self { filters }
    }

    /// Default pipeline: `BasicLegalityFilter` → `SimulationFilter`.
    pub fn default_pipeline() -> Self {
        Self::new(vec![
            Box::new(BasicLegalityFilter),
            Box::new(SimulationFilter),
        ])
    }

    pub fn accepts(&self, state: &GameState, candidate: &CandidateAction) -> bool {
        self.filters.iter().all(|f| f.accept(state, candidate))
    }

    /// Apply the pipeline to an iterator of candidates.
    ///
    /// Equivalence-class legality memoization: the cheap filters run unchanged
    /// per candidate, but the [`FilterCost::Expensive`] verdict (the
    /// state-cloning [`SimulationFilter`]) is memoized by a content-equivalence
    /// key ([`LegalityKey`]). Content-identical candidates that map to the same
    /// key (e.g. one of ~700 identical Squirrel tokens declaring one attack)
    /// share a single clone-and-apply instead of cloning ~10MB of `GameState`
    /// per candidate. Candidates whose legality is not provably fp-determined
    /// (`legality_equivalence_key` returns `None`, or a conservative poison gate
    /// fires) fall back to the fresh per-candidate path — identical to today's
    /// behavior. The memoized verdict is the AND of every expensive filter, so
    /// `accepts()` semantics are preserved exactly.
    pub fn apply<I>(&self, state: &GameState, candidates: I) -> Vec<CandidateAction>
    where
        I: IntoIterator<Item = CandidateAction>,
    {
        let poison = LegalityPoisonGates::compute(state);
        let mut memo: HashMap<LegalityKey, bool> = HashMap::new();
        let mut interner = FingerprintInterner::default();
        candidates
            .into_iter()
            .filter(|c| {
                if !self.cheap_filters_accept(state, c) {
                    return false;
                }
                match legality_equivalence_key(state, &c.action, &poison, &mut interner) {
                    Some(key) => *memo
                        .entry(key)
                        .or_insert_with(|| self.expensive_verdict(state, c)),
                    None => self.expensive_verdict(state, c),
                }
            })
            .collect()
    }

    /// Run every non-`Expensive` filter (the cheap, per-candidate checks).
    /// First rejection wins, mirroring `accepts()`.
    fn cheap_filters_accept(&self, state: &GameState, candidate: &CandidateAction) -> bool {
        self.filters
            .iter()
            .filter(|f| f.cost() != FilterCost::Expensive)
            .all(|f| f.accept(state, candidate))
    }

    /// The memoizable verdict: the AND of every `Expensive` filter. For the
    /// default pipeline this is exactly `SimulationFilter::accept`.
    fn expensive_verdict(&self, state: &GameState, candidate: &CandidateAction) -> bool {
        self.filters
            .iter()
            .filter(|f| f.cost() == FilterCost::Expensive)
            .all(|f| f.accept(state, candidate))
    }
}

// ---------------------------------------------------------------------------
// Equivalence-class legality memoization
// ---------------------------------------------------------------------------

/// Content fingerprint of a single `GameObject`, used as the memo key's object
/// identity. Two objects with equal fingerprints are interchangeable for the
/// purpose of the expensive legality simulation: every field the conservative
/// SAFE classifiers read is captured here, while pure identity/ordering fields
/// (`id`, `incarnation`, `timestamp`) and display-derived fields (recomputed
/// under `PublicFinalizeMode::DeferredDisplay`, which `apply_as_current_for_legality`
/// uses) are excluded.
///
/// CR 400.7: object identity (`id`/`incarnation`) is intentionally NOT part of
/// the fingerprint — the whole point is to collapse distinct-id, content-identical
/// objects into one equivalence class.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ObjectFingerprint {
    controller: PlayerId,
    owner: PlayerId,
    is_token: bool,
    tapped: bool,
    keywords: Vec<Keyword>,
    color: Vec<ManaColor>,
    card_types: CardType,
    mana_cost: ManaCost,
    cost_x_paid: Option<u32>,
    zone: Zone,
    counters: HashMap<CounterType, u32>,
    power: Option<i32>,
    toughness: Option<i32>,
    base_power: Option<i32>,
    name: String,
    foretold: bool,
    is_saddled: bool,
    is_suspected: bool,
    is_renowned: bool,
    is_commander: bool,
    loyalty: Option<u32>,
    loyalty_activations_this_turn: u32,
    abilities: Arc<Vec<AbilityDefinition>>,
    trigger_definitions: Definitions<TriggerDefinition>,
    replacement_definitions: Definitions<ReplacementDefinition>,
    static_definitions: Definitions<StaticDefinition>,
    played_from_zone: Option<Zone>,
    cast_from_zone: Option<Zone>,
    goaded_by: HashSet<PlayerId>,
    detained_by: HashSet<PlayerId>,
    damage_marked: u32,
    dealt_deathtouch_damage: bool,
    summoning_sick: bool,
    entered_battlefield_turn: Option<u32>,
    phase_status: PhaseStatus,
    face_down: bool,
    flipped: bool,
    transformed: bool,
    defense: Option<u32>,
    class_level: Option<u8>,
    room_unlocks: Option<RoomUnlockState>,
    /// CR 719.3c: `CaseState` derives neither `PartialEq` nor `Hash`, and the
    /// ONLY `case_state` read on a covered legality path is the
    /// `ActivationRestriction::IsSolved` arm (`game::restrictions`
    /// `check_activation_restrictions`, which reads `cs.is_solved` alone). That
    /// is a faithful projection FOR LEGALITY (not for general equality), so the
    /// fingerprint captures just the `is_solved` bool rather than the whole
    /// struct. The other `case_state` reader, `TriggerCondition::SolveConditionMet`
    /// (`game::triggers`), is a resolution-time TRIGGER condition — it does not
    /// gate the `apply().is_ok()` legality verdict, and `solve_condition` is
    /// card-identical across same-fingerprint objects regardless. If a future
    /// edit makes `solve_condition` gate activation/target/combat legality, this
    /// proxy becomes unsound and must capture the full state.
    case_solved: Option<bool>,
    harnessed: bool,
    monstrous: bool,
    echo_due: bool,
}

/// XOR-fold per-element hashes so an unordered collection hashes
/// order-independently (and consistently with `PartialEq`).
fn fold_unordered<T: Hash>(items: impl Iterator<Item = T>) -> u64 {
    let mut acc = 0u64;
    for item in items {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        item.hash(&mut h);
        acc ^= h.finish();
    }
    acc
}

impl Hash for ObjectFingerprint {
    fn hash<H: Hasher>(&self, h: &mut H) {
        // Provable SUBSET of Eq: every hashed value is part of `PartialEq`, so
        // equal fingerprints necessarily agree on all of them. Unordered
        // collections are XOR-folded; definition lists hash by length only
        // (their element types are Eq but not Hash). Hash collisions are sound —
        // `Eq` is the final arbiter on a memo bucket.
        self.controller.hash(h);
        self.owner.hash(h);
        self.is_token.hash(h);
        self.tapped.hash(h);
        self.zone.hash(h);
        self.name.hash(h);
        self.power.hash(h);
        self.toughness.hash(h);
        self.base_power.hash(h);
        self.mana_cost.mana_value().hash(h);
        self.cost_x_paid.hash(h);
        self.damage_marked.hash(h);
        self.loyalty.hash(h);
        self.phase_status.hash(h);
        h.write_u64(fold_unordered(self.counters.iter()));
        h.write_u64(fold_unordered(self.goaded_by.iter()));
        h.write_u64(fold_unordered(self.detained_by.iter()));
        self.abilities.len().hash(h);
        self.trigger_definitions.len().hash(h);
        self.replacement_definitions.len().hash(h);
        self.static_definitions.len().hash(h);
    }
}

/// Compute the content fingerprint of an object, or `None` when the object
/// carries any per-object relational state the fingerprint cannot represent —
/// in which case the object must never be memoized (FORCES-None).
fn object_fingerprint(state: &GameState, id: ObjectId) -> Option<ObjectFingerprint> {
    let obj = state.objects.get(&id)?;
    // FORCES-None: relational / merge / attachment / cast-link state whose
    // legality depends on a SPECIFIC other object or pairing. None of these is
    // captured by the fingerprint, so any non-default value disqualifies memo.
    if obj.attached_to.is_some()
        || !obj.attachments.is_empty()
        || obj.paired_with.is_some()
        || obj.pair_controller.is_some()
        || !obj.convoked_creatures.is_empty()
        || !obj.merged_components.is_empty()
        || obj.merge_kind.is_some()
        || obj.merge_layer_effect_id.is_some()
        || obj.split_from_merge_survivor.is_some()
        || obj.pre_merge_is_token.is_some()
        || !obj.saddled_by.is_empty()
        || obj.entered_via_ability_source.is_some()
        || obj.exile_from_stack_linked_source.is_some()
        || obj.cast_cost_paid_object.is_some()
        || obj.signature_spell.is_some()
        || obj.emblem_source.is_some()
    {
        return None;
    }
    Some(ObjectFingerprint {
        controller: obj.controller,
        owner: obj.owner,
        is_token: obj.is_token,
        tapped: obj.tapped,
        keywords: obj.keywords.clone(),
        color: obj.color.clone(),
        card_types: obj.card_types.clone(),
        mana_cost: obj.mana_cost.clone(),
        cost_x_paid: obj.cost_x_paid,
        zone: obj.zone,
        counters: obj.counters.clone(),
        power: obj.power,
        toughness: obj.toughness,
        base_power: obj.base_power,
        name: obj.name.clone(),
        foretold: obj.foretold,
        is_saddled: obj.is_saddled,
        is_suspected: obj.is_suspected,
        is_renowned: obj.is_renowned,
        is_commander: obj.is_commander,
        loyalty: obj.loyalty,
        loyalty_activations_this_turn: obj.loyalty_activations_this_turn,
        abilities: obj.abilities.clone(),
        trigger_definitions: obj.trigger_definitions.clone(),
        replacement_definitions: obj.replacement_definitions.clone(),
        static_definitions: obj.static_definitions.clone(),
        played_from_zone: obj.played_from_zone,
        cast_from_zone: obj.cast_from_zone,
        goaded_by: obj.goaded_by.clone(),
        detained_by: obj.detained_by.clone(),
        damage_marked: obj.damage_marked,
        dealt_deathtouch_damage: obj.dealt_deathtouch_damage,
        summoning_sick: obj.summoning_sick,
        entered_battlefield_turn: obj.entered_battlefield_turn,
        phase_status: obj.phase_status,
        face_down: obj.face_down,
        flipped: obj.flipped,
        transformed: obj.transformed,
        defense: obj.defense,
        class_level: obj.class_level,
        room_unlocks: obj.room_unlocks,
        case_solved: obj.case_state.as_ref().map(|c| c.is_solved),
        harnessed: obj.harnessed,
        monstrous: obj.monstrous,
        echo_due: obj.echo_due,
    })
}

/// The legality "verb" a memo key belongs to. Keeps verdicts from two different
/// action classes that happen to share a fingerprint from colliding.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum LegalityClass {
    DeclareAttacker,
    DeclareBlocker,
    ActivateAbility,
    ManaTap,
    ChooseTarget,
}

/// Per-[`FilterPipeline::apply`] fingerprint interner. Each distinct object is
/// fingerprinted at most once; the memo key then hashes a small dense `u32`
/// class id instead of re-constructing and re-hashing a heavy
/// [`ObjectFingerprint`] per candidate. This is a PURE PERFORMANCE transform:
/// equal fingerprints map to equal ids (the `classes` map keys on
/// `ObjectFingerprint`'s `Eq`/`Hash`), so the set of equivalence classes — and
/// thus every memo verdict — is identical to fingerprinting per candidate.
#[derive(Default)]
struct FingerprintInterner {
    /// Object id → interned class id (`None` = FORCES-None / not memoizable).
    by_id: HashMap<ObjectId, Option<u32>>,
    /// Canonical fingerprint → dense class id (next id assigned on miss).
    classes: HashMap<ObjectFingerprint, u32>,
}

impl FingerprintInterner {
    /// Intern an object's fingerprint to a dense class id, computing the
    /// fingerprint at most once per object id. A `None` result propagates the
    /// FORCES-None disqualification exactly as a `None` fingerprint did before.
    fn intern(&mut self, state: &GameState, id: ObjectId) -> Option<u32> {
        if let Some(&cached) = self.by_id.get(&id) {
            return cached;
        }
        let class = object_fingerprint(state, id).map(|fp| {
            let next = self.classes.len() as u32;
            *self.classes.entry(fp).or_insert(next)
        });
        self.by_id.insert(id, class);
        class
    }
}

/// Owned (no borrows) equivalence key for one candidate's expensive verdict.
///
/// `fp`/`counterpart` hold interned class ids (see [`FingerprintInterner`]), not
/// the heavy [`ObjectFingerprint`]s themselves — so `Hash`/`Eq` are over cheap
/// scalar fields while preserving the exact equivalence classes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LegalityKey {
    class: LegalityClass,
    fp: u32,
    ability_index: Option<usize>,
    activations_this_turn: u32,
    activations_this_game: u32,
    source_attacked_this_turn: bool,
    /// SORTED mode indices chosen this turn for this source (CR 700.2).
    chosen_modes_turn: Vec<usize>,
    /// SORTED mode indices chosen this game for this source (CR 700.2).
    chosen_modes_game: Vec<usize>,
    is_attacking: bool,
    is_blocking: bool,
    is_blocked: bool,
    /// The declared `AttackTarget` for a single-attacker declaration; `None`
    /// for non-attack classes.
    attack_target: Option<AttackTarget>,
    /// DeclareBlocker only: the blocked attacker's interned fingerprint id plus
    /// its ring-bearer designation (CR 701.54c/e). `None` for other classes.
    counterpart: Option<(u32, bool)>,
}

impl LegalityKey {
    fn new(class: LegalityClass, fp: u32) -> Self {
        Self {
            class,
            fp,
            ability_index: None,
            activations_this_turn: 0,
            activations_this_game: 0,
            source_attacked_this_turn: false,
            chosen_modes_turn: Vec::new(),
            chosen_modes_game: Vec::new(),
            is_attacking: false,
            is_blocking: false,
            is_blocked: false,
            attack_target: None,
            counterpart: None,
        }
    }
}

/// SORTED mode indices recorded for `id` in a `(ObjectId, mode_index)` set.
fn sorted_modes(set: &HashSet<(ObjectId, usize)>, id: ObjectId) -> Vec<usize> {
    let mut v: Vec<usize> = set
        .iter()
        .filter(|(oid, _)| *oid == id)
        .map(|(_, m)| *m)
        .collect();
    v.sort_unstable();
    v
}

/// CR 107: A `QuantityExpr` reads only memo-safe state (fingerprint source
/// fields / apply()-constant state) when it never resolves a dynamic
/// `QuantityRef`. Exhaustive over all 11 variants — NO wildcard — so a new
/// arithmetic combinator forces a classification decision (E0004).
fn quantity_reads_only_memo_safe_state(q: &QuantityExpr) -> bool {
    match q {
        // A dynamic game-state lookup. Presence-of-`Ref` is the conservative
        // gate; we do not enumerate the ~80 `QuantityRef` variants.
        QuantityExpr::Ref { .. } => false,
        QuantityExpr::Fixed { .. } => true,
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_reads_only_memo_safe_state(inner),
        QuantityExpr::UpTo { max } => quantity_reads_only_memo_safe_state(max),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().all(quantity_reads_only_memo_safe_state)
        }
        // CR 107.3: base is an `i32` literal; only the exponent can be dynamic.
        QuantityExpr::Power { exponent, .. } => quantity_reads_only_memo_safe_state(exponent),
        QuantityExpr::Difference { left, right } => {
            quantity_reads_only_memo_safe_state(left) && quantity_reads_only_memo_safe_state(right)
        }
    }
}

/// SAFE-allowlist classifier (default POISON) over all 83 `FilterProp`
/// variants — NO wildcard, so a new prop forces a classification decision
/// (E0004). SAFE = reads only the candidate object's own fingerprint fields or
/// apply()-constant state. POISON = reads another object by id, an
/// object-id-keyed side-table, board-global continuous-effect resolution with
/// no FORCES-None backstop, or is identity/combat/history-relative.
fn filterprop_reads_only_candidate_fp(p: &FilterProp) -> bool {
    match p {
        // SAFE — read only candidate fingerprint fields.
        FilterProp::Token
        | FilterProp::NonToken
        | FilterProp::WasPlayed
        | FilterProp::Tapped
        | FilterProp::Untapped
        | FilterProp::IsSaddled
        | FilterProp::WithKeyword { .. }
        | FilterProp::WithoutKeyword { .. }
        | FilterProp::ManaCostIn { .. }
        | FilterProp::InZone { .. }
        | FilterProp::InAnyZone { .. }
        | FilterProp::Owned { .. }
        | FilterProp::Foretold
        | FilterProp::HasColor { .. }
        | FilterProp::NotColor { .. }
        | FilterProp::ColorCount { .. }
        | FilterProp::ManaSymbolCount { .. }
        | FilterProp::HasSupertype { .. }
        | FilterProp::NotSupertype { .. }
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::Suspected
        | FilterProp::Renowned
        | FilterProp::Modified
        | FilterProp::Historic
        | FilterProp::NotHistoric
        | FilterProp::FaceDown
        | FilterProp::HasXInManaCost
        | FilterProp::HasManaAbility
        | FilterProp::HasNoAbilities
        | FilterProp::Named { .. }
        | FilterProp::IsCommander => true,

        // SAFE only when the embedded `QuantityExpr` is memo-safe.
        FilterProp::Counters { count, .. } => quantity_reads_only_memo_safe_state(count),
        FilterProp::Cmc { value, .. } => quantity_reads_only_memo_safe_state(value),
        FilterProp::PtComparison { value, .. } => quantity_reads_only_memo_safe_state(value),

        // SAFE only for a fixed parity; `LastNamedChoice` reads a resolution choice.
        FilterProp::ManaValueParity { parity } => matches!(parity, ParitySource::Fixed(_)),

        // Combinators: SAFE iff ALL children SAFE.
        FilterProp::AnyOf { props } => props.iter().all(filterprop_reads_only_candidate_fp),
        FilterProp::Not { prop } => filterprop_reads_only_candidate_fp(prop),

        // POISON — read another object / side-table / combat / history / identity.
        FilterProp::Attacking { .. }
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::CombatRelation { .. }
        | FilterProp::Unblocked
        | FilterProp::AttackingAlone
        | FilterProp::BlockingAlone
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ZoneChangedThisTurn { .. }
        | FilterProp::AttackedThisTurn
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::Another
        | FilterProp::OtherThanTriggerObject
        | FilterProp::SaddledSource
        | FilterProp::ConvokedSource
        | FilterProp::PowerGTSource
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::HasAttachment { .. }
        | FilterProp::HasAnyAttachmentOf { .. }
        | FilterProp::HasKeywordKind { .. }
        | FilterProp::WithoutKeywordKind { .. }
        | FilterProp::Targets { .. }
        | FilterProp::TargetsOnly { .. }
        | FilterProp::HasSingleTarget
        | FilterProp::HasXInActivationCost
        | FilterProp::WasKicked
        | FilterProp::SameName
        | FilterProp::SameNameAsParentTarget
        | FilterProp::NameMatchesAnyPermanent { .. }
        | FilterProp::DifferentNameFrom { .. }
        | FilterProp::SharesQuality { .. }
        | FilterProp::CanEnchant { .. }
        | FilterProp::IsChosenCreatureType
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::IsChosenLandOrNonlandKind
        | FilterProp::MostPrevalentCreatureTypeIn { .. }
        | FilterProp::ProtectorMatches { .. }
        | FilterProp::HasHasteOrControlledSinceTurnBegan
        | FilterProp::Unpaired
        | FilterProp::Other { .. } => false,
    }
}

/// CR 601.3 / CR 602.5: A `ParsedCondition` reads only memo-safe state.
/// Exhaustive over all ~65 variants — NO wildcard. SAFE = reads only the
/// source object's fingerprint fields (or fields folded into the key, e.g.
/// SourceIsAttacking / SourceAttackedThisTurn), controller-scoped, or global
/// apply()-constant state. UNSAFE = reaches `resolve_quantity_scoped(source_id)`
/// into combat / damage history / pending-cast (`QuantityComparison`,
/// `QuantityVsEachOpponent`, `SpellTargetsFilter`).
fn condition_reads_only_memo_safe_state(c: &ParsedCondition) -> bool {
    match c {
        ParsedCondition::QuantityComparison { .. }
        | ParsedCondition::QuantityVsEachOpponent { .. }
        | ParsedCondition::SpellTargetsFilter { .. } => false,

        // Combinators read the union of their children: SAFE iff all children SAFE.
        ParsedCondition::And { conditions } | ParsedCondition::Or { conditions } => {
            conditions.iter().all(condition_reads_only_memo_safe_state)
        }
        ParsedCondition::Not { condition } => condition_reads_only_memo_safe_state(condition),

        // SAFE: source-field reads (in fp, or attached_to ⇒ FORCES-None),
        // key-folded combat/attack facts, controller-scoped, and global state.
        ParsedCondition::SourceInZone { .. }
        | ParsedCondition::SourceIsAttacking
        | ParsedCondition::SourceIsAttackingOrBlocking
        | ParsedCondition::SourceIsBlocked
        | ParsedCondition::SourcePowerAtLeast { .. }
        | ParsedCondition::SourceHasCounterAtLeast { .. }
        | ParsedCondition::SourceHasNoCounter { .. }
        | ParsedCondition::SourceEnteredThisTurn
        | ParsedCondition::SourceAttackedThisTurn
        | ParsedCondition::SourceIsCreature
        | ParsedCondition::SourceAttachedTo { .. }
        | ParsedCondition::SourceUntappedAttachedTo { .. }
        | ParsedCondition::SourceLacksKeyword { .. }
        | ParsedCondition::SourceIsColor { .. }
        | ParsedCondition::FirstSpellThisGame
        | ParsedCondition::OpponentSearchedLibraryThisTurn
        | ParsedCondition::BeenAttackedThisStep
        | ParsedCondition::ZoneCardCountAtLeast { .. }
        | ParsedCondition::ZoneCardTypeCountAtLeast { .. }
        | ParsedCondition::ZoneCoreTypeCardCountAtLeast { .. }
        | ParsedCondition::ZoneSubtypeCardCountAtLeast { .. }
        | ParsedCondition::OpponentPoisonAtLeast { .. }
        | ParsedCondition::HandSizeExact { .. }
        | ParsedCondition::HandSizeOneOf { .. }
        | ParsedCondition::CreaturesYouControlTotalPowerAtLeast { .. }
        | ParsedCondition::YouControlLandSubtypeAny { .. }
        | ParsedCondition::YouControlSubtypeCountAtLeast { .. }
        | ParsedCondition::YouControlCoreTypeCountAtLeast { .. }
        | ParsedCondition::YouControlColorPermanentCountAtLeast { .. }
        | ParsedCondition::YouControlSubtypeOrGraveyardCardSubtype { .. }
        | ParsedCondition::YouControlLegendaryCreature
        | ParsedCondition::YouControlNamedPlaneswalker { .. }
        | ParsedCondition::ControlsCreatureWithKeyword { .. }
        | ParsedCondition::YouControlCreatureWithPowerAtLeast { .. }
        | ParsedCondition::YouControlCreatureWithPt { .. }
        | ParsedCondition::YouControlAnotherColorlessCreature
        | ParsedCondition::YouControlSnowPermanentCountAtLeast { .. }
        | ParsedCondition::YouControlDifferentPowerCreatureCountAtLeast { .. }
        | ParsedCondition::YouControlLandsWithSameNameAtLeast { .. }
        | ParsedCondition::YouControlNoCreatures
        | ParsedCondition::YouAttackedThisTurn
        | ParsedCondition::YouAttackedSourceControllerThisTurn
        | ParsedCondition::YouAttackedWithAtLeast { .. }
        | ParsedCondition::YouPlayedLandThisTurn
        | ParsedCondition::YouCastSpellThisTurn { .. }
        | ParsedCondition::YouCastNoncreatureSpellThisTurn
        | ParsedCondition::YouCastSpellCountAtLeast { .. }
        | ParsedCondition::YouGainedLifeThisTurn
        | ParsedCondition::YouCreatedTokenThisTurn
        | ParsedCondition::YouDiscardedCardThisTurn
        | ParsedCondition::YouSacrificedArtifactThisTurn
        | ParsedCondition::CreatureDiedThisTurn
        | ParsedCondition::YouHadCreatureEnterThisTurn
        | ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn
        | ParsedCondition::YouHadArtifactEnterThisTurn
        | ParsedCondition::BattlefieldEntriesThisTurn { .. }
        | ParsedCondition::CardsLeftYourGraveyardThisTurnAtLeast { .. }
        | ParsedCondition::PlayerCountAtLeast { .. }
        | ParsedCondition::HasCityBlessing
        | ParsedCondition::IsYourTurn => true,
    }
}

/// CR 118: descend `Composite`/`OneOf` and report whether any leaf cost has a
/// dynamic (non-memo-safe) amount. A dynamic life/discard/energy/speed cost can
/// make two same-fingerprint sources differ in affordability across candidates.
fn cost_has_dynamic_amount(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::PayLife { amount }
        | AbilityCost::PayEnergy { amount }
        | AbilityCost::PaySpeed { amount } => !quantity_reads_only_memo_safe_state(amount),
        AbilityCost::Discard { count, .. } => !quantity_reads_only_memo_safe_state(count),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            costs.iter().any(cost_has_dynamic_amount)
        }
        _ => false,
    }
}

/// Whether every `FilterProp` reachable through a `TargetFilter` is SAFE.
/// Conservatively POISON for any filter shape that is not a plain typed/boolean
/// composition (e.g. `SpecificObject`, tracked-set, exile-link references).
fn target_filter_all_props_safe(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().all(filterprop_reads_only_candidate_fp),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().all(target_filter_all_props_safe)
        }
        TargetFilter::Not { filter } => target_filter_all_props_safe(filter),
        // Type/zone-only and player filters carry no candidate-fp-relative props.
        TargetFilter::None | TargetFilter::Any | TargetFilter::Player => true,
        // Everything else (SpecificObject, StackAbility, tracked sets, exile
        // links, anaphoric refs, …) is identity-relative or unhandled: POISON.
        _ => false,
    }
}

/// Whether the active pending target-selection filter contains any POISON
/// `FilterProp` (so `ChooseTarget { Object(_) }` candidates must NOT be
/// memoized). Conservatively POISON for every waiting state other than a
/// spell/ability `TargetSelection` whose filters are provably all-SAFE.
fn pending_target_filter_is_poison(state: &GameState) -> bool {
    match &state.waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => {
            !resolved_ability_target_filters_safe(&pending_cast.ability)
        }
        _ => true,
    }
}

/// Walk a resolved ability's effect chain, returning `false` if any effect's
/// target filter contains a POISON `FilterProp`.
fn resolved_ability_target_filters_safe(ability: &ResolvedAbility) -> bool {
    if let Some(f) = ability.effect.target_filter() {
        if !target_filter_all_props_safe(f) {
            return false;
        }
    }
    if let Some(sub) = &ability.sub_ability {
        if !resolved_ability_target_filters_safe(sub) {
            return false;
        }
    }
    if let Some(els) = &ability.else_ability {
        if !resolved_ability_target_filters_safe(els) {
            return false;
        }
    }
    true
}

/// Conservative per-`FilterPipeline::apply` poison gates, computed ONCE via a
/// single `game_functioning_statics` sweep plus a single battlefield scan. When
/// a gate is set, every candidate of that class falls back to the fresh
/// per-candidate simulation (no memoization) — sound by construction.
struct LegalityPoisonGates {
    has_declare_attacker: bool,
    has_declare_blocker: bool,
    has_activation: bool,
    has_choose_target: bool,
}

impl LegalityPoisonGates {
    fn compute(state: &GameState) -> Self {
        let mut g = LegalityPoisonGates {
            has_declare_attacker: false,
            has_declare_blocker: false,
            has_activation: false,
            has_choose_target: false,
        };

        for (_, def) in game_functioning_statics(state) {
            // SpecificObject is the only ObjectId-bearing TargetFilter variant;
            // a functioning static affecting one specific object can make
            // otherwise-identical objects legally distinct — poison all classes.
            if matches!(def.affected, Some(TargetFilter::SpecificObject { .. })) {
                g.has_declare_attacker = true;
                g.has_declare_blocker = true;
                g.has_activation = true;
                g.has_choose_target = true;
            }
            // CR 508.1d: declare-attacker restrictions / requirements + remote
            // id-discrimination (Goaded).
            if matches!(
                def.mode,
                StaticMode::CantAttack
                    | StaticMode::CantAttackOrBlock
                    | StaticMode::MustAttack
                    | StaticMode::MustAttackPlayer { .. }
                    | StaticMode::Goaded
                    | StaticMode::CanAttackWithDefender
                    | StaticMode::MaxAttackersEachCombat { .. }
                    | StaticMode::CombatAlone { .. }
            ) {
                g.has_declare_attacker = true;
            }
            // CR 509.1: declare-blocker restrictions / requirements.
            if matches!(
                def.mode,
                StaticMode::CantBlock
                    | StaticMode::CantAttackOrBlock
                    | StaticMode::CantBeBlocked
                    | StaticMode::CantBeBlockedBy { .. }
                    | StaticMode::CantBeBlockedExceptBy { .. }
                    | StaticMode::CantBeBlockedByMoreThan { .. }
                    | StaticMode::BlockRestriction { .. }
                    | StaticMode::MustBlock
                    | StaticMode::MustBlockAttacker { .. }
                    | StaticMode::MustBeBlocked
                    | StaticMode::MustBeBlockedByAll
                    | StaticMode::MaxBlockersEachCombat { .. }
                    | StaticMode::ExtraBlockers { .. }
                    | StaticMode::CanBlockShadow
                    | StaticMode::IgnoreLandwalkForBlocking { .. }
                    | StaticMode::Menace
            ) {
                g.has_declare_blocker = true;
            }
            // CR 602.5 / CR 117.1b: activation prohibitions and per-turn limit
            // modifiers (the latter make the folded activation count insufficient).
            if matches!(
                def.mode,
                StaticMode::CantBeActivated { .. }
                    | StaticMode::CantActivateDuring { .. }
                    | StaticMode::CantTap
                    | StaticMode::CantUntap
                    | StaticMode::CantPayCost { .. }
                    | StaticMode::ModifyActivationLimit { .. }
            ) {
                g.has_activation = true;
            }
        }

        // One battlefield scan: goaded creatures (CR 508.1d remote
        // discrimination) and activatable abilities whose condition / cost reads
        // non-memo-safe per-object state.
        for &id in &state.battlefield {
            let Some(obj) = state.objects.get(&id) else {
                continue;
            };
            if !obj.goaded_by.is_empty() {
                g.has_declare_attacker = true;
            }
            for ability in obj.abilities.iter() {
                if ability.kind != AbilityKind::Activated {
                    continue;
                }
                for restriction in &ability.activation_restrictions {
                    if let ActivationRestriction::RequiresCondition {
                        condition: Some(cond),
                    } = restriction
                    {
                        if !condition_reads_only_memo_safe_state(cond) {
                            g.has_activation = true;
                        }
                    }
                }
                if let Some(cost) = &ability.cost {
                    if cost_has_dynamic_amount(cost) {
                        g.has_activation = true;
                    }
                }
            }
        }

        if !g.has_choose_target {
            g.has_choose_target = pending_target_filter_is_poison(state);
        }

        g
    }
}

/// Build the equivalence key for a candidate action, or `None` to fall back to
/// the fresh per-candidate simulation (uncovered shapes, fingerprint FORCES-None,
/// or a fired poison gate). Activation/mode counters are folded at the
/// candidate's own `(source_id, ability_index)` — never the id alone.
///
/// MEMO-AUDIT — hand-coded-validator maintenance contract (§9.10):
/// the soundness of this memoization depends on the legality verdict for the
/// covered classes being a pure function of `(LegalityClass, ObjectFingerprint,
/// folded key fields, poison gates)`. That holds only because the hand-coded
/// validators that decide those verdicts read no per-object identity / combat /
/// history / object-id-keyed side-table state that the key does not already
/// capture. The load-bearing validators are:
///   - `game::combat::can_block_pair` / `can_block_pair_with_precomputed`
///     (DeclareBlocker legality),
///   - `game::combat::validate_attackers` (DeclareAttacker legality),
///   - `game::targeting::can_target` (ChooseTarget legality).
///
/// Any future edit that makes one of these read a NEW per-object identity,
/// combat-role, turn-history, or object-id-keyed side-table value MUST be
/// re-classified against this module's contract: add it to `ObjectFingerprint`,
/// fold it into `LegalityKey`, or poison the corresponding class in
/// `LegalityPoisonGates`. Skipping that step silently collapses content-distinct
/// candidates onto one shared (wrong) verdict.
fn legality_equivalence_key(
    state: &GameState,
    action: &GameAction,
    poison: &LegalityPoisonGates,
    interner: &mut FingerprintInterner,
) -> Option<LegalityKey> {
    match action {
        GameAction::DeclareAttackers { attacks, bands } => {
            if poison.has_declare_attacker || attacks.len() != 1 || !bands.is_empty() {
                return None;
            }
            let (attacker_id, attack_target) = &attacks[0];
            let fp = interner.intern(state, *attacker_id)?;
            let mut key = LegalityKey::new(LegalityClass::DeclareAttacker, fp);
            key.attack_target = Some(*attack_target);
            Some(key)
        }
        GameAction::DeclareBlockers { assignments } => {
            if poison.has_declare_blocker || assignments.len() != 1 {
                return None;
            }
            let (blocker_id, attacker_id) = assignments[0];
            let fp = interner.intern(state, blocker_id)?;
            let attacker_fp = interner.intern(state, attacker_id)?;
            let attacker_controller = state.objects.get(&attacker_id)?.controller;
            // CR 701.54c/e: a blocked Ring-bearer carries combat-relevant
            // designation the blocker's legality may depend on.
            let ring_bearer = crate::game::effects::ring::is_current_ring_bearer(
                state,
                attacker_controller,
                attacker_id,
            );
            let mut key = LegalityKey::new(LegalityClass::DeclareBlocker, fp);
            key.counterpart = Some((attacker_fp, ring_bearer));
            Some(key)
        }
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => {
            if poison.has_activation {
                return None;
            }
            let fp = interner.intern(state, *source_id)?;
            let mut key = LegalityKey::new(LegalityClass::ActivateAbility, fp);
            key.ability_index = Some(*ability_index);
            key.activations_this_turn = state
                .activated_abilities_this_turn
                .get(&(*source_id, *ability_index))
                .copied()
                .unwrap_or(0);
            key.activations_this_game = state
                .activated_abilities_this_game
                .get(&(*source_id, *ability_index))
                .copied()
                .unwrap_or(0);
            key.source_attacked_this_turn = state.creatures_attacked_this_turn.contains(source_id);
            key.chosen_modes_turn = sorted_modes(&state.modal_modes_chosen_this_turn, *source_id);
            key.chosen_modes_game = sorted_modes(&state.modal_modes_chosen_this_game, *source_id);
            key.is_attacking = crate::game::restrictions::is_source_attacking(state, *source_id);
            key.is_blocking = crate::game::restrictions::is_source_blocking(state, *source_id);
            key.is_blocked = crate::game::restrictions::is_source_blocked(state, *source_id);
            Some(key)
        }
        GameAction::TapLandForMana { object_id } | GameAction::UntapLandForMana { object_id } => {
            if poison.has_activation {
                return None;
            }
            let controller = state.objects.get(object_id)?.controller;
            let options = crate::game::mana_sources::activatable_land_mana_options(
                state, *object_id, controller,
            );
            // Exactly one option ⇒ that ability index (or `None` for a legacy
            // subtype-only record, folded at index 0). Zero or many ⇒ the
            // verdict is not a single-ability legality question — don't memoize.
            if options.len() != 1 {
                return None;
            }
            let fp = interner.intern(state, *object_id)?;
            let mut key = LegalityKey::new(LegalityClass::ManaTap, fp);
            let ability_index = options[0].ability_index;
            key.ability_index = ability_index;
            let lookup_index = ability_index.unwrap_or(0);
            key.activations_this_turn = state
                .activated_abilities_this_turn
                .get(&(*object_id, lookup_index))
                .copied()
                .unwrap_or(0);
            key.activations_this_game = state
                .activated_abilities_this_game
                .get(&(*object_id, lookup_index))
                .copied()
                .unwrap_or(0);
            key.source_attacked_this_turn = state.creatures_attacked_this_turn.contains(object_id);
            key.is_attacking = crate::game::restrictions::is_source_attacking(state, *object_id);
            key.is_blocking = crate::game::restrictions::is_source_blocking(state, *object_id);
            key.is_blocked = crate::game::restrictions::is_source_blocked(state, *object_id);
            Some(key)
        }
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        } => {
            if poison.has_choose_target {
                return None;
            }
            let fp = interner.intern(state, *id)?;
            Some(LegalityKey::new(LegalityClass::ChooseTarget, fp))
        }
        // Everything else (empty/multi/banded DeclareAttackers, CastSpell,
        // PassPriority, TapForConvoke [short-circuited in SimulationFilter],
        // non-object ChooseTarget): fresh per-candidate simulation.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_support::candidate_actions;
    use crate::types::game_state::GameState;

    #[test]
    fn default_pipeline_registered_filters_ordered_by_cost() {
        let pipeline = FilterPipeline::default_pipeline();
        let costs: Vec<FilterCost> = pipeline.filters.iter().map(|f| f.cost()).collect();
        // Pipeline must be monotonically non-decreasing in cost so cheap
        // rejections dominate and SimulationFilter is the last resort.
        for window in costs.windows(2) {
            assert!(
                window[0] <= window[1],
                "filters must be ordered cheapest-first: {:?}",
                costs
            );
        }
    }

    #[test]
    fn simulation_filter_accepts_pass_priority_at_opening_state() {
        // A fresh two-player state has Priority on player 0; PassPriority must
        // be accepted by the oracle. This is the baseline sanity check for the
        // `cheap ⊆ sim` invariant: if SimulationFilter rejected this,
        // BasicLegalityFilter would too.
        let state = GameState::new_two_player(42);
        let candidates = candidate_actions(&state);
        let pass = candidates
            .into_iter()
            .find(|c| matches!(c.action, crate::types::actions::GameAction::PassPriority))
            .expect("PassPriority should be a candidate in the opening state");
        assert!(SimulationFilter.accept(&state, &pass));
        assert!(BasicLegalityFilter.accept(&state, &pass));
    }

    /// The `cheap ⊆ sim` invariant: for every candidate generated by
    /// `candidate_actions` in a representative game state, if
    /// `BasicLegalityFilter` rejects the candidate, `SimulationFilter` must
    /// reject it too. Enforced over the full candidate set of a fresh
    /// two-player state to keep the test hermetic and fast — adding proptest
    /// state generation is a follow-up when deeper coverage is needed.
    #[test]
    fn basic_legality_is_subset_of_simulation() {
        let state = GameState::new_two_player(42);
        let candidates = candidate_actions(&state);
        for candidate in candidates {
            let cheap_accepts = BasicLegalityFilter.accept(&state, &candidate);
            let sim_accepts = SimulationFilter.accept(&state, &candidate);
            if !cheap_accepts {
                assert!(
                    !sim_accepts,
                    "cheap⊆sim violated: BasicLegalityFilter rejected `{}` \
                     but SimulationFilter accepted — candidate would be \
                     silently dropped. Action: {:?}",
                    candidate.action.variant_name(),
                    candidate.action
                );
            }
        }
    }

    // -- Equivalence-class legality memoization --

    use crate::ai_support::{ActionMetadata, TacticalClass};
    use crate::game::combat::{AttackTarget, CombatState};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Comparator, Effect, FilterProp, ParitySource,
        ParsedCondition, QuantityExpr, QuantityRef, TargetFilter, TargetRef, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterMatch;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_object(state: &mut GameState, card_id: u64, owner: u8, name: &str) -> ObjectId {
        create_object(
            state,
            CardId(card_id),
            PlayerId(owner),
            name.to_string(),
            Zone::Battlefield,
        )
    }

    fn cand(action: GameAction) -> CandidateAction {
        CandidateAction {
            action,
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Pass,
            },
        }
    }

    fn dynamic_ref() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::LifeAboveStarting,
        }
    }

    /// #1: two content-identical sources share ONE legality clone (memo hit).
    /// Revert (drop the memo) ⇒ 2 clones, failing the assertion.
    #[test]
    fn memo_dedups_identical_activate_ability_to_one_clone() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let a = make_object(&mut state, 100, 0, "Squirrel");
        let b = make_object(&mut state, 100, 0, "Squirrel");
        assert_eq!(
            object_fingerprint(&state, a),
            object_fingerprint(&state, b),
            "identical objects must share a fingerprint"
        );
        let pipeline = FilterPipeline::default_pipeline();
        let candidates = vec![
            cand(GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            }),
            cand(GameAction::ActivateAbility {
                source_id: b,
                ability_index: 0,
            }),
        ];
        crate::game::perf_counters::reset();
        let _ = pipeline.apply(&state, candidates);
        let clones = crate::game::perf_counters::snapshot().state_clone_for_legality;
        assert_eq!(
            clones, 1,
            "identical ActivateAbility candidates must reuse one legality clone; got {clones}"
        );
    }

    /// #17/#12-style: a dynamic activation cost poisons the activation gate, so
    /// each candidate runs a fresh clone. Revert the gate ⇒ memo ⇒ 1 clone.
    #[test]
    fn dynamic_activation_cost_poisons_activation_memo() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let a = make_object(&mut state, 100, 0, "Squirrel");
        let b = make_object(&mut state, 100, 0, "Squirrel");
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Player,
            },
        )
        .cost(AbilityCost::PayLife {
            amount: dynamic_ref(),
        });
        for id in [a, b] {
            let obj = state.objects.get_mut(&id).unwrap();
            Arc::make_mut(&mut obj.abilities).push(ability.clone());
        }
        let poison = LegalityPoisonGates::compute(&state);
        assert!(
            poison.has_activation,
            "dynamic PayLife cost must poison the activation gate"
        );
        let pipeline = FilterPipeline::default_pipeline();
        let candidates = vec![
            cand(GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            }),
            cand(GameAction::ActivateAbility {
                source_id: b,
                ability_index: 0,
            }),
        ];
        crate::game::perf_counters::reset();
        let _ = pipeline.apply(&state, candidates);
        let clones = crate::game::perf_counters::snapshot().state_clone_for_legality;
        assert_eq!(
            clones, 2,
            "poisoned activation must clone per candidate (no memo); got {clones}"
        );
    }

    /// #19: controller is part of the fingerprint.
    #[test]
    fn controller_is_part_of_fingerprint() {
        let mut state = GameState::new_two_player(42);
        let a = make_object(&mut state, 100, 0, "Squirrel");
        let b = make_object(&mut state, 100, 0, "Squirrel");
        state.objects.get_mut(&b).unwrap().controller = PlayerId(1);
        assert_ne!(
            object_fingerprint(&state, a),
            object_fingerprint(&state, b),
            "controller must be part of the fingerprint"
        );
    }

    /// #22: the fingerprint-membership contract — name, owner, and mana cost
    /// each independently distinguish fingerprints. Pins the contract so a
    /// future fp refactor cannot silently drop one of these fields.
    #[test]
    fn fingerprint_membership_guards_name_owner_manacost() {
        // name
        let mut s = GameState::new_two_player(1);
        let a = make_object(&mut s, 100, 0, "Squirrel");
        let b = make_object(&mut s, 100, 0, "Acorn");
        assert_ne!(
            object_fingerprint(&s, a),
            object_fingerprint(&s, b),
            "name must be in the fingerprint"
        );
        // owner (isolated from controller)
        let mut s = GameState::new_two_player(2);
        let a = make_object(&mut s, 100, 0, "Squirrel");
        let b = make_object(&mut s, 100, 0, "Squirrel");
        s.objects.get_mut(&b).unwrap().owner = PlayerId(1);
        assert_ne!(
            object_fingerprint(&s, a),
            object_fingerprint(&s, b),
            "owner must be in the fingerprint"
        );
        // mana cost
        let mut s = GameState::new_two_player(3);
        let a = make_object(&mut s, 100, 0, "Squirrel");
        let b = make_object(&mut s, 100, 0, "Squirrel");
        s.objects.get_mut(&b).unwrap().mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: 3,
        };
        assert_ne!(
            object_fingerprint(&s, a),
            object_fingerprint(&s, b),
            "mana cost must be in the fingerprint"
        );
    }

    /// #9: per-(source, ability) activation count is folded into the key.
    #[test]
    fn activation_count_is_folded_into_key() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let a = make_object(&mut state, 100, 0, "Squirrel");
        let b = make_object(&mut state, 100, 0, "Squirrel");
        state.activated_abilities_this_turn.insert((a, 0), 1);
        let poison = LegalityPoisonGates::compute(&state);
        // Shared interner: a and b have identical fingerprints, so they intern
        // to the SAME class id — the keys must still differ purely on the folded
        // activation count.
        let mut interner = FingerprintInterner::default();
        let ka = legality_equivalence_key(
            &state,
            &GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            },
            &poison,
            &mut interner,
        );
        let kb = legality_equivalence_key(
            &state,
            &GameAction::ActivateAbility {
                source_id: b,
                ability_index: 0,
            },
            &poison,
            &mut interner,
        );
        assert!(ka.is_some() && kb.is_some());
        assert_ne!(
            ka, kb,
            "activation count must be folded per (source, ability) into the key"
        );
    }

    /// #11: a source's live combat role is folded into the key.
    #[test]
    fn combat_role_is_folded_into_key() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let a = make_object(&mut state, 100, 0, "Squirrel");
        let b = make_object(&mut state, 100, 0, "Squirrel");
        let mut combat = CombatState::default();
        combat.blocker_to_attacker.insert(a, vec![b]);
        state.combat = Some(combat);
        let poison = LegalityPoisonGates::compute(&state);
        // Shared interner: a and b intern to the same class id; only the folded
        // live combat role (a is blocking, b is not) may distinguish the keys.
        let mut interner = FingerprintInterner::default();
        let ka = legality_equivalence_key(
            &state,
            &GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            },
            &poison,
            &mut interner,
        );
        let kb = legality_equivalence_key(
            &state,
            &GameAction::ActivateAbility {
                source_id: b,
                ability_index: 0,
            },
            &poison,
            &mut interner,
        );
        assert_ne!(
            ka, kb,
            "is_blocking combat role must be folded into the key"
        );
    }

    /// #2: single-attacker declarations memoize; multi/empty/banded do not.
    #[test]
    fn declare_attacker_single_keys_multi_and_banded_none() {
        let mut state = GameState::new_two_player(42);
        let a = make_object(&mut state, 100, 0, "Squirrel");
        let b = make_object(&mut state, 100, 0, "Squirrel");
        let poison = LegalityPoisonGates::compute(&state);
        let target = AttackTarget::Player(PlayerId(1));
        let single_a = GameAction::DeclareAttackers {
            attacks: vec![(a, target)],
            bands: vec![],
        };
        let single_b = GameAction::DeclareAttackers {
            attacks: vec![(b, target)],
            bands: vec![],
        };
        let mut interner = FingerprintInterner::default();
        assert!(legality_equivalence_key(&state, &single_a, &poison, &mut interner).is_some());
        assert_eq!(
            legality_equivalence_key(&state, &single_a, &poison, &mut interner),
            legality_equivalence_key(&state, &single_b, &poison, &mut interner),
            "identical single-attacker declarations must share a key"
        );
        let multi = GameAction::DeclareAttackers {
            attacks: vec![(a, target), (b, target)],
            bands: vec![],
        };
        assert!(legality_equivalence_key(&state, &multi, &poison, &mut interner).is_none());
        let empty = GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        };
        assert!(legality_equivalence_key(&state, &empty, &poison, &mut interner).is_none());
        let banded = GameAction::DeclareAttackers {
            attacks: vec![(a, target)],
            bands: vec![vec![a]],
        };
        assert!(legality_equivalence_key(&state, &banded, &poison, &mut interner).is_none());
    }

    /// #3 + #16: single-blocker declarations memoize; multi does not; the
    /// blocked attacker's ring-bearer designation distinguishes the key.
    #[test]
    fn declare_blocker_single_keys_and_ringbearer_distinguishes() {
        let mut state = GameState::new_two_player(42);
        let blocker = make_object(&mut state, 100, 0, "Squirrel");
        let atk1 = make_object(&mut state, 200, 1, "Bear");
        let atk2 = make_object(&mut state, 200, 1, "Bear");
        for id in [atk1, atk2] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        let poison = LegalityPoisonGates::compute(&state);
        let single1 = GameAction::DeclareBlockers {
            assignments: vec![(blocker, atk1)],
        };
        let single2 = GameAction::DeclareBlockers {
            assignments: vec![(blocker, atk2)],
        };
        // Shared interner: atk1 and atk2 intern to the same class id, so the
        // keys agree until the ring-bearer bool (folded into `counterpart`)
        // diverges — interning leaves the fingerprint identity untouched.
        let mut interner = FingerprintInterner::default();
        assert!(legality_equivalence_key(&state, &single1, &poison, &mut interner).is_some());
        assert_eq!(
            legality_equivalence_key(&state, &single1, &poison, &mut interner),
            legality_equivalence_key(&state, &single2, &poison, &mut interner),
            "blocking identical attackers must share a key"
        );
        let multi = GameAction::DeclareBlockers {
            assignments: vec![(blocker, atk1), (blocker, atk2)],
        };
        assert!(legality_equivalence_key(&state, &multi, &poison, &mut interner).is_none());
        // CR 701.54e: make atk2 the ring-bearer ⇒ counterpart differs ⇒ keys differ.
        state.ring_bearer.insert(PlayerId(1), Some(atk2));
        assert_ne!(
            legality_equivalence_key(&state, &single1, &poison, &mut interner),
            legality_equivalence_key(&state, &single2, &poison, &mut interner),
            "blocked ring-bearer designation must distinguish the key"
        );
    }

    /// #20: any attachment forces the fingerprint to `None` (never memoized).
    #[test]
    fn attachment_forces_none_fingerprint() {
        let mut state = GameState::new_two_player(42);
        let a = make_object(&mut state, 100, 0, "Squirrel");
        state
            .objects
            .get_mut(&a)
            .unwrap()
            .attachments
            .push(ObjectId(9999));
        assert!(
            object_fingerprint(&state, a).is_none(),
            "attachment-bearing object must FORCES-None"
        );
    }

    /// #21: QuantityExpr classifier — Fixed-only safe, any Ref poisons,
    /// arithmetic combinators propagate.
    #[test]
    fn quantity_classifier_fixed_vs_ref() {
        assert!(quantity_reads_only_memo_safe_state(&QuantityExpr::Fixed {
            value: 3
        }));
        assert!(!quantity_reads_only_memo_safe_state(&dynamic_ref()));
        assert!(quantity_reads_only_memo_safe_state(&QuantityExpr::Max {
            exprs: vec![
                QuantityExpr::Fixed { value: 2 },
                QuantityExpr::Fixed { value: 5 }
            ]
        }));
        assert!(!quantity_reads_only_memo_safe_state(&QuantityExpr::Max {
            exprs: vec![QuantityExpr::Fixed { value: 2 }, dynamic_ref()]
        }));
        assert!(!quantity_reads_only_memo_safe_state(
            &QuantityExpr::Difference {
                left: Box::new(QuantityExpr::Fixed { value: 1 }),
                right: Box::new(dynamic_ref()),
            }
        ));
        assert!(!quantity_reads_only_memo_safe_state(&QuantityExpr::Power {
            base: 2,
            exponent: Box::new(dynamic_ref()),
        }));
    }

    /// #5/#6/#14: FilterProp SAFE/POISON partition and combinator propagation.
    #[test]
    fn filterprop_classifier_partition() {
        assert!(filterprop_reads_only_candidate_fp(&FilterProp::Tapped));
        assert!(filterprop_reads_only_candidate_fp(&FilterProp::Named {
            name: "Squirrel".to_string()
        }));
        assert!(filterprop_reads_only_candidate_fp(
            &FilterProp::ManaValueParity {
                parity: ParitySource::Fixed(crate::types::ability::Parity::Even),
            }
        ));
        assert!(!filterprop_reads_only_candidate_fp(
            &FilterProp::AttackedThisTurn
        ));
        assert!(!filterprop_reads_only_candidate_fp(&FilterProp::WasKicked));
        assert!(!filterprop_reads_only_candidate_fp(
            &FilterProp::HasXInActivationCost
        ));
        assert!(filterprop_reads_only_candidate_fp(&FilterProp::Not {
            prop: Box::new(FilterProp::Tapped)
        }));
        assert!(!filterprop_reads_only_candidate_fp(&FilterProp::Not {
            prop: Box::new(FilterProp::AttackedThisTurn)
        }));
        assert!(!filterprop_reads_only_candidate_fp(&FilterProp::AnyOf {
            props: vec![FilterProp::Tapped, FilterProp::AttackedThisTurn]
        }));
        // Counters with a dynamic count ⇒ POISON; with a Fixed count ⇒ SAFE.
        assert!(!filterprop_reads_only_candidate_fp(&FilterProp::Counters {
            counters: CounterMatch::Any,
            comparator: Comparator::GE,
            count: dynamic_ref(),
        }));
        assert!(filterprop_reads_only_candidate_fp(&FilterProp::Counters {
            counters: CounterMatch::Any,
            comparator: Comparator::GE,
            count: QuantityExpr::Fixed { value: 1 },
        }));
    }

    /// #15: ParsedCondition classifier — source/global safe, dynamic unsafe.
    #[test]
    fn condition_classifier_dynamic_unsafe() {
        assert!(condition_reads_only_memo_safe_state(
            &ParsedCondition::SourceIsAttacking
        ));
        assert!(condition_reads_only_memo_safe_state(
            &ParsedCondition::HandSizeExact { count: 3 }
        ));
        let dynamic = ParsedCondition::QuantityComparison {
            lhs: dynamic_ref(),
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        };
        assert!(!condition_reads_only_memo_safe_state(&dynamic));
        assert!(!condition_reads_only_memo_safe_state(
            &ParsedCondition::And {
                conditions: vec![ParsedCondition::SourceIsAttacking, dynamic],
            }
        ));
    }

    /// #5/#6 at the gate level: a SAFE target filter passes the choose-target
    /// gate; a POISON filter fails it.
    #[test]
    fn choose_target_filter_safety_gate() {
        let safe =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Tapped]));
        assert!(target_filter_all_props_safe(&safe));
        let poison = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::AttackedThisTurn]),
        );
        assert!(!target_filter_all_props_safe(&poison));

        // An object ChooseTarget gets a memo key only when the gate is clear.
        let mut state = GameState::new_two_player(42);
        let target = make_object(&mut state, 100, 0, "Squirrel");
        let action = GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        };
        let clear = LegalityPoisonGates {
            has_declare_attacker: false,
            has_declare_blocker: false,
            has_activation: false,
            has_choose_target: false,
        };
        let mut interner = FingerprintInterner::default();
        assert!(legality_equivalence_key(&state, &action, &clear, &mut interner).is_some());
        let gated = LegalityPoisonGates {
            has_choose_target: true,
            ..clear
        };
        assert!(legality_equivalence_key(&state, &action, &gated, &mut interner).is_none());
    }
}
