//! Engine B — the **static ability-graph extractor** (offline candidate generator).
//!
//! This module is **purely additive** and changes no game behavior. It never
//! calls `apply()`, never drives a `GameRunner`, and never touches a
//! `GameState`. Given a list of card faces it builds a directed
//! ability/resource graph, finds strongly-connected components (Tarjan SCC),
//! and emits **candidate cycles** whose summed per-cycle [`ResourceVector`] is
//! *coverable* (net-progress: ≥1 axis strictly up — or unbounded-up — with no
//! controller-side consumed axis net-negative unless that axis is unbounded-up).
//!
//! It is the over-approximate, fast, card-list stage of the two-engine combo
//! detector: Engine B (here) *proposes* candidates; Engine A
//! ([`crate::analysis::detect_loop`], already shipped) is the sound, stateful
//! stage that *confirms* them by driving the reducer. A candidate is
//! **unconfirmed by construction** — it ignores targeting legality, timing
//! windows, "may" choices, and replacement interactions — so it is a
//! [`CandidateCycle`], deliberately **never** a [`crate::analysis::LoopCertificate`]
//! (whose soundness invariant requires a driven board-equality proof).
//!
//! Theory references (CS, not CR): Tarjan SCC for cycle finding; Karp–Miller /
//! Petri-net coverability for the net-progress test (see
//! `FEASIBILITY-AND-PLAN.md` §3).
//!
//! # PR-4a scope
//!
//! Five priority effect families are modeled — **mana** (CR 106.1), **counters**
//! (CR 122.1), **damage** (CR 120.1 / CR 704.5a), **tap/untap** (CR 701.26a/b),
//! and **cast/copy** (CR 601.2a). Every other [`Effect`] variant projects to
//! [`Projection::Unmodeled`] (contributes nothing) via an exhaustive no-wildcard
//! `match`, so a newly-added variant is a compile error until classified — the
//! same drift gate the four classifiers below share (precedent:
//! `FeatureSupport`, `game/coverage.rs`). Remaining effect families, the life
//! axis, and broader trigger-edge breadth land in PR-4b.

use std::collections::{BTreeMap, BTreeSet};

use petgraph::graph::{DiGraph, NodeIndex};

use crate::analysis::loop_check::{classify_win_kind, WinKind};
use crate::analysis::resource::{
    CounterClass, ObjectClass, ResourceAxis, ResourceVector, TriggerKind,
};
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, Effect, ManaProduction,
    QuantityExpr, TapStateChange,
};
use crate::types::card::CardFace;
use crate::types::counter::CounterMatch;
use crate::types::mana::{ManaColor, ManaCost, ManaType};
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;

/// CR 101: Static analysis has no concrete `PlayerId`, so the player-keyed
/// `ResourceVector` axes use a documented sentinel convention — the loop's
/// controller is `PlayerId(0)`, its opponent `PlayerId(1)`. Damage / mill aimed
/// at "any target" / "target player" / "each opponent" is keyed to [`OPPONENT`];
/// this keeps the candidate net vector compatible with the controller-scoped
/// coverability test and lets a candidate's axes feed Engine A's `covers()`.
const CONTROLLER: PlayerId = PlayerId(0);
const OPPONENT: PlayerId = PlayerId(1);

/// WUBRG + colorless index order, mirroring `resource::MANA_INDEX` (private
/// there). Index `i` of a [`ResourceVector::mana`] array is `MANA_COLORS[i]`.
const MANA_COLORS: [ManaType; 6] = [
    ManaType::White,
    ManaType::Blue,
    ManaType::Black,
    ManaType::Red,
    ManaType::Green,
    ManaType::Colorless,
];
/// Index of the colorless slot in [`MANA_COLORS`] / [`ResourceVector::mana`].
const COLORLESS_INDEX: usize = 5;

// ---------------------------------------------------------------------------
// AxisKey — the magnitude/player-agnostic edge-matching key
// ---------------------------------------------------------------------------

/// The magnitude- and player-id-agnostic projection of [`ResourceAxis`] used
/// **only** for static edge matching. It is a leaf parameterization of the same
/// axis vocabulary (one abstraction level up in genericity, same CR sections),
/// derived from `ResourceAxis` by dropping the runtime payload and adding the
/// axes that have no `ResourceVector` field (`Tap`).
///
/// Two round-3 collapses are baked into [`AxisKey::from`]:
/// **R3-MANA-COLLAPSE** — every `Mana(color)` folds to the single color-agnostic
/// [`AxisKey::Mana`] so any mana production intersects any mana cost; and
/// **R3-LANDFALL-COLLAPSE** — both `LandfallTriggers` and
/// `Trigger(Landfall)` fold to [`AxisKey::Landfall`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AxisKey {
    /// CR 106.1: any mana, any color (R3-MANA-COLLAPSE — a single fungible key).
    Mana,
    /// CR 120.1: damage dealt.
    Damage,
    /// CR 119.1: life.
    Life,
    /// CR 401: library size.
    Library,
    /// CR 122.1: a counter of a specific class on a specific object class.
    Counter(CounterClass, ObjectClass),
    /// CR 122.1: a **requires-only** wildcard for "remove a counter of any type"
    /// costs (R3-COUNTER-FUNGIBILITY); matches any `Counter(_, _)` producer.
    /// Never produced by [`AxisKey::from`].
    AnyCounter,
    /// CR 603: a non-counter trigger/event family (proliferate, magecraft, …).
    Trigger(TriggerKind),
    /// CR 111: tokens created.
    Tokens,
    /// CR 601: spells cast.
    Casts,
    /// CR 121: cards drawn.
    Draw,
    /// CR 701.26a/b: untapped state — produced by an untap, consumed by a tap.
    /// Has no `ResourceVector` numeric axis; injected directly into a node's
    /// produces/requires sets.
    Tap,
    /// CR 500.7: extra turns.
    ExtraTurn,
    /// CR 603.6a: enters-the-battlefield triggers.
    Etb,
    /// CR 603.6c: leaves-the-battlefield triggers.
    Ltb,
    /// CR 700.4: dies (creature-to-graveyard) triggers.
    Death,
    /// CR 701.21a: sacrifice triggers.
    Sac,
    /// CR 603: landfall triggers (R3-LANDFALL-COLLAPSE — the single landfall key).
    Landfall,
    /// CR 500.8: extra combat phases.
    Combat,
}

/// HIGH-2 compile-time drift gate: an exhaustive **no-wildcard** projection of
/// every [`ResourceAxis`] variant onto an [`AxisKey`]. A newly-added
/// `ResourceAxis` is a compile error here until mapped (precedent:
/// `FeatureSupport`, `game/coverage.rs`). Bakes in R3-MANA-COLLAPSE (every
/// `Mana(_)` → the one `AxisKey::Mana`) and R3-LANDFALL-COLLAPSE (both landfall
/// representations → `AxisKey::Landfall`). [`AxisKey::AnyCounter`] is never
/// produced here — it is a requires-only sentinel.
impl From<&ResourceAxis> for AxisKey {
    fn from(axis: &ResourceAxis) -> AxisKey {
        match axis {
            ResourceAxis::Mana(_) => AxisKey::Mana,
            ResourceAxis::Life(_) => AxisKey::Life,
            ResourceAxis::DamageDealt(_) => AxisKey::Damage,
            ResourceAxis::LibraryDelta(_) => AxisKey::Library,
            ResourceAxis::Counter(class, obj) => AxisKey::Counter(*class, *obj),
            ResourceAxis::Trigger(TriggerKind::Landfall) => AxisKey::Landfall,
            ResourceAxis::Trigger(kind) => AxisKey::Trigger(*kind),
            ResourceAxis::TokensCreated => AxisKey::Tokens,
            ResourceAxis::CardsDrawn => AxisKey::Draw,
            ResourceAxis::Casts => AxisKey::Casts,
            ResourceAxis::LandfallTriggers => AxisKey::Landfall,
            ResourceAxis::CombatPhases => AxisKey::Combat,
            ResourceAxis::ExtraTurns => AxisKey::ExtraTurn,
            ResourceAxis::DeathTriggers => AxisKey::Death,
            ResourceAxis::EtbTriggers => AxisKey::Etb,
            ResourceAxis::LtbTriggers => AxisKey::Ltb,
            ResourceAxis::SacTriggers => AxisKey::Sac,
        }
    }
}

// ---------------------------------------------------------------------------
// Projection — the per-Effect static resource contribution
// ---------------------------------------------------------------------------

/// Per-axis production magnitude marker (HIGH-1). The `Fixed` payload is read by
/// tests and is the seed for PR-4b cost-precision; PR-4a production logic only
/// branches on `Unbounded`, so the integer is intentionally inert in non-test
/// builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum AxisMagnitude {
    /// A statically-knowable fixed amount.
    Fixed(i32),
    /// Unbounded-up: a dynamic `QuantityExpr` sitting in a production position.
    /// Coverability treats this axis as coverable regardless of any fixed
    /// counter-cost on the same axis (HIGH-1).
    Unbounded,
}

/// The static resource contribution of a single [`Effect`] node. Typed enum (not
/// a `modeled: bool`) so the "unmodeled contributes nothing" invariant is
/// unforgeable. Over-approximate by design (candidate stage).
///
/// Carries the signed net [`ResourceVector`] for field-bearing axes, per-axis
/// magnitude markers (so dynamic production records `Unbounded`-up), and the
/// **field-less** axis injections (`Tap` from a `SetTapState`, `AnyCounter` from
/// an untyped counter removal) that a `ResourceVector` cannot express.
enum Projection {
    Modeled {
        // Boxed so the modeled variant doesn't bloat the (immediately-consumed)
        // enum — `ResourceVector` is ~320 bytes of maps/array.
        vector: Box<ResourceVector>,
        magnitudes: BTreeMap<AxisKey, AxisMagnitude>,
        produces: BTreeSet<AxisKey>,
        requires: BTreeSet<AxisKey>,
    },
    Unmodeled,
}

/// Accumulating builder for a [`Projection::Modeled`].
#[derive(Default)]
struct Proj {
    vector: ResourceVector,
    magnitudes: BTreeMap<AxisKey, AxisMagnitude>,
    produces: BTreeSet<AxisKey>,
    requires: BTreeSet<AxisKey>,
}

impl Proj {
    /// Record a production magnitude, upgrading to `Unbounded` if either the
    /// existing or incoming marker is unbounded-up.
    fn mark(&mut self, key: AxisKey, mag: AxisMagnitude) {
        let entry = self.magnitudes.entry(key).or_insert(mag);
        if matches!(mag, AxisMagnitude::Unbounded) {
            *entry = AxisMagnitude::Unbounded;
        }
    }
    fn add_mana(&mut self, idx: usize, amount: i64, mag: AxisMagnitude) {
        self.vector.mana[idx] += amount;
        if amount > 0 {
            self.mark(AxisKey::Mana, mag);
        }
    }
    fn add_counter(
        &mut self,
        class: CounterClass,
        obj: ObjectClass,
        amount: i64,
        mag: AxisMagnitude,
    ) {
        *self.vector.counters.entry((class, obj)).or_insert(0) += amount;
        if amount > 0 {
            self.mark(AxisKey::Counter(class, obj), mag);
        }
    }
    fn add_damage(&mut self, amount: i64, mag: AxisMagnitude) {
        *self.vector.damage_dealt.entry(OPPONENT).or_insert(0) += amount;
        if amount > 0 {
            self.mark(AxisKey::Damage, mag);
        }
    }
    fn finish(self) -> Projection {
        Projection::Modeled {
            vector: Box::new(self.vector),
            magnitudes: self.magnitudes,
            produces: self.produces,
            requires: self.requires,
        }
    }
}

/// Magnitude + amount of a production-position [`QuantityExpr`]: a static
/// `Fixed { value }` keeps its amount; any dynamic expression
/// (`Ref`/`Multiply`/`UpTo`/…) is unbounded-up and seeds a unit so the axis is
/// present and positive (HIGH-1 — never under-count dynamic production).
fn count_seed(q: &QuantityExpr) -> (i64, AxisMagnitude) {
    match q {
        QuantityExpr::Fixed { value } => (*value as i64, AxisMagnitude::Fixed(*value)),
        _ => (1, AxisMagnitude::Unbounded),
    }
}

/// CR 122.1: the object class a given counter class most naturally accumulates
/// on, used to key the counter axis when only the counter kind is known
/// statically (+1/+1 ⇒ creature, loyalty ⇒ planeswalker, poison/energy ⇒ player).
fn default_object_class(class: CounterClass) -> ObjectClass {
    match class {
        CounterClass::Plus1Plus1 | CounterClass::Minus1Minus1 => ObjectClass::Creature,
        CounterClass::Loyalty => ObjectClass::Planeswalker,
        CounterClass::Defense => ObjectClass::Battle,
        CounterClass::Poison | CounterClass::Energy => ObjectClass::Player,
        CounterClass::Other => ObjectClass::Other,
    }
}

/// CR 106.1: which mana slot(s) a [`ManaProduction`] seeds, plus its magnitude.
/// VECTOR-SEEDING layer (R4-MED1-COLOR-SEED) — a determinate single color seeds
/// that color slot (so the candidate can name a concrete-colored axis); any
/// color-flexible or multi-color production seeds the colorless sentinel. The
/// EDGE-KEY collapse (every mana → `AxisKey::Mana`) is a separate layer handled
/// by [`AxisKey::from`].
fn project_mana_production(p: &ManaProduction) -> (Vec<(usize, i64)>, AxisMagnitude) {
    let idx = |c: &ManaColor| -> usize {
        match c {
            ManaColor::White => 0,
            ManaColor::Blue => 1,
            ManaColor::Black => 2,
            ManaColor::Red => 3,
            ManaColor::Green => 4,
        }
    };
    match p {
        ManaProduction::Colorless { count } => {
            let (a, mag) = count_seed(count);
            (vec![(COLORLESS_INDEX, a)], mag)
        }
        ManaProduction::Fixed { colors, .. } => {
            let seeds: Vec<(usize, i64)> = colors.iter().map(|c| (idx(c), 1)).collect();
            (seeds, AxisMagnitude::Fixed(colors.len() as i32))
        }
        ManaProduction::Mixed {
            colorless_count,
            colors,
            ..
        } => {
            let mut seeds = vec![(COLORLESS_INDEX, *colorless_count as i64)];
            seeds.extend(colors.iter().map(|c| (idx(c), 1)));
            (
                seeds,
                AxisMagnitude::Fixed(*colorless_count as i32 + colors.len() as i32),
            )
        }
        ManaProduction::AnyOneColor {
            count,
            color_options,
            ..
        }
        | ManaProduction::AnyCombination {
            count,
            color_options,
            ..
        } => {
            let (a, mag) = count_seed(count);
            // R4-MED1-COLOR-SEED: a singleton color set pins a determinate color.
            let slot = if color_options.len() == 1 {
                idx(&color_options[0])
            } else {
                COLORLESS_INDEX
            };
            (vec![(slot, a)], mag)
        }
        ManaProduction::ChosenColor { count, .. }
        | ManaProduction::OpponentLandColors { count, .. }
        | ManaProduction::AnyTypeProduceableBy { count, .. }
        | ManaProduction::AnyInCommandersColorIdentity { count, .. }
        | ManaProduction::AnyOneColorAmongPermanents { count, .. }
        | ManaProduction::AnyCombinationOfObjectColors { count, .. } => {
            let (a, mag) = count_seed(count);
            (vec![(COLORLESS_INDEX, a)], mag)
        }
        ManaProduction::ChoiceAmongExiledColors { .. }
        | ManaProduction::ChoiceAmongCombinations { .. }
        | ManaProduction::TriggerEventManaType => {
            (vec![(COLORLESS_INDEX, 1)], AxisMagnitude::Fixed(1))
        }
        ManaProduction::DistinctColorsAmongPermanents { .. } => {
            (vec![(COLORLESS_INDEX, 1)], AxisMagnitude::Unbounded)
        }
    }
}

/// The central deliverable: project a single [`Effect`] onto its static resource
/// contribution. Exhaustive **no-wildcard** match over all 207 `Effect` variants
/// — five priority families modeled (CR 106.1 / 122.1 / 120.1 / 701.26 / 601.2),
/// every other variant `Projection::Unmodeled` (contributes nothing). PR-4b
/// reclassifies unmodeled arms without touching this match's exhaustiveness.
fn effect_projection(effect: &Effect) -> Projection {
    let mut b = Proj::default();
    match effect {
        // ----- MANA family (CR 106.1) -----
        Effect::Mana { produced, .. } => {
            let (seeds, mag) = project_mana_production(produced);
            for (slot, amount) in seeds {
                b.add_mana(slot, amount, mag);
            }
        }
        Effect::GainEnergy { amount } => {
            let (a, mag) = count_seed(amount);
            b.add_counter(CounterClass::Energy, ObjectClass::Player, a, mag);
        }
        // ----- COUNTER family (CR 122.1) -----
        Effect::PutCounter {
            counter_type,
            count,
            ..
        }
        | Effect::PutCounterAll {
            counter_type,
            count,
            ..
        } => {
            let class = CounterClass::from_counter_type(counter_type);
            let (a, mag) = count_seed(count);
            b.add_counter(class, default_object_class(class), a, mag);
        }
        Effect::MultiplyCounter { counter_type, .. } => {
            // Doubling/tripling existing counters scales with the current count —
            // dynamic growth, so mark the axis unbounded-up (HIGH-1).
            let class = CounterClass::from_counter_type(counter_type);
            b.add_counter(
                class,
                default_object_class(class),
                1,
                AxisMagnitude::Unbounded,
            );
        }
        Effect::RemoveCounter {
            counter_type,
            count,
            ..
        } => match counter_type {
            // CR 122.1: removing counters consumes the counter resource (negative).
            Some(ct) => {
                let class = CounterClass::from_counter_type(ct);
                let (a, _) = count_seed(count);
                b.add_counter(
                    class,
                    default_object_class(class),
                    -a,
                    AxisMagnitude::Fixed(0),
                );
            }
            // Untyped "remove a counter" ⇒ requires-only wildcard (R3-COUNTER-FUNGIBILITY).
            None => {
                b.requires.insert(AxisKey::AnyCounter);
            }
        },
        // CR 701.34: proliferate pumps the proliferate trigger axis mana-neutrally.
        Effect::Proliferate | Effect::ProliferateTarget { .. } => {
            *b.vector
                .generic_triggers
                .entry(TriggerKind::Proliferate)
                .or_insert(0) += 1;
        }
        // ----- DAMAGE family (CR 120.1 / CR 704.5a) -----
        Effect::DealDamage { amount, .. }
        | Effect::DamageAll { amount, .. }
        | Effect::DamageEachPlayer { amount, .. } => {
            let (a, mag) = count_seed(amount);
            b.add_damage(a, mag);
        }
        Effect::EachDealsDamageEqualToPower { .. } => {
            // Damage equal to each source's power — dynamic, unbounded-up.
            b.add_damage(1, AxisMagnitude::Unbounded);
        }
        // ----- TAP/UNTAP family (CR 701.26a/b) -----
        Effect::SetTapState { state, .. } => match state {
            // CR 701.26b: untapping produces untapped state (the loop pivot).
            TapStateChange::Untap => {
                b.produces.insert(AxisKey::Tap);
            }
            // CR 701.26a: tapping consumes untapped state (Opposition-style).
            TapStateChange::Tap => {
                b.requires.insert(AxisKey::Tap);
            }
        },
        // ----- CAST/COPY family (CR 601.2a) -----
        Effect::CopySpell { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::Encore
        | Effect::Myriad => {
            b.vector.casts_this_step += 1;
        }
        // ----- UNMODELED (over-approximate candidate stage; PR-4b breadth) -----
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::CounterAll { .. }
        | Effect::Token { .. }
        | Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DestroyAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::BounceAll { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Populate
        | Effect::Clash
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::GenericEffect { .. }
        | Effect::Cleanup { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RollDie { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::Planeswalk
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::ForEachCategoryExile { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::Exploit { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::TurnFaceUp { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::Conjure { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::Intensify { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::ChooseOneOf { .. }
        | Effect::Unimplemented { .. } => return Projection::Unmodeled,
    }
    b.finish()
}

// ---------------------------------------------------------------------------
// trigger_axis — TriggerMode → consumed event axis (the trigger-edge requirement)
// ---------------------------------------------------------------------------

/// MEDIUM-1 compile-time drift gate: an exhaustive **no-wildcard** match over all
/// 169 [`TriggerMode`] variants. A trigger node *consumes* (requires) the event
/// axis its mode fires on, so a producer of that axis edges into it. In PR-4a the
/// `Some` set is exactly {cast, counter, tap, mana} — the trigger consumers whose
/// matching producers are modeled in 4a. Every other variant — including the
/// lifegain/dies/ETB/sac families — returns `None` as an explicit arm; PR-4b
/// flips those bodies to `Some(..)` without touching the match's exhaustiveness.
fn trigger_axis(mode: &TriggerMode) -> Option<AxisKey> {
    match mode {
        // CR 601.2a: cast/copy triggers (storm, magecraft) consume the cast axis.
        TriggerMode::SpellCast
        | TriggerMode::SpellCopy
        | TriggerMode::SpellCastOrCopy
        | TriggerMode::SpellAbilityCast
        | TriggerMode::SpellAbilityCopy => Some(AxisKey::Casts),
        // CR 122.1: counter-added triggers fire on any counter producer (R3-COUNTER-FUNGIBILITY).
        TriggerMode::CounterAdded
        | TriggerMode::CounterAddedOnce
        | TriggerMode::CounterAddedAll
        | TriggerMode::CounterPlayerAddedAll
        | TriggerMode::CounterTypeAddedAll => Some(AxisKey::AnyCounter),
        // CR 701.26a: "becomes tapped" requires untapped state to consume.
        TriggerMode::Taps | TriggerMode::TapAll => Some(AxisKey::Tap),
        // CR 106.1: mana-added / tap-for-mana triggers consume the mana axis.
        TriggerMode::TapsForMana | TriggerMode::ManaAdded => Some(AxisKey::Mana),
        // ----- deferred to PR-4b (matching producers not modeled in 4a) -----
        TriggerMode::ChangesZone
        | TriggerMode::ChangesZoneAll
        | TriggerMode::ChangesController
        | TriggerMode::LeavesBattlefield
        | TriggerMode::DamageDone
        | TriggerMode::DamageDoneOnce
        | TriggerMode::DamageAll
        | TriggerMode::DamageDealtOnce
        | TriggerMode::DamageDoneOnceByController
        | TriggerMode::DamageReceived
        | TriggerMode::DamagePreventedOnce
        | TriggerMode::ExcessDamage
        | TriggerMode::ExcessDamageAll
        | TriggerMode::AbilityCast
        | TriggerMode::AbilityResolves
        | TriggerMode::AbilityTriggered
        | TriggerMode::Countered
        | TriggerMode::Attacks
        | TriggerMode::AttackersDeclared
        | TriggerMode::YouAttack
        | TriggerMode::YouAttackUnblocked
        | TriggerMode::AttackersDeclaredOneTarget
        | TriggerMode::AttackerBlocked
        | TriggerMode::AttackerBlockedOnce
        | TriggerMode::AttackerBlockedByCreature
        | TriggerMode::AttackerUnblocked
        | TriggerMode::AttackerUnblockedOnce
        | TriggerMode::Blocks
        | TriggerMode::BlockersDeclared
        | TriggerMode::BecomesBlocked
        | TriggerMode::CounterRemoved
        | TriggerMode::CounterRemovedOnce
        | TriggerMode::Sacrificed
        | TriggerMode::SacrificedOnce
        | TriggerMode::Destroyed
        | TriggerMode::Untaps
        | TriggerMode::UntapAll
        | TriggerMode::BecomesTarget
        | TriggerMode::BecomesTargetOnce
        | TriggerMode::Drawn
        | TriggerMode::Discarded
        | TriggerMode::DiscardedAll
        | TriggerMode::Milled
        | TriggerMode::MilledOnce
        | TriggerMode::MilledAll
        | TriggerMode::Exiled
        | TriggerMode::Revealed
        | TriggerMode::Shuffled
        | TriggerMode::LifeGained
        | TriggerMode::LifeLost
        | TriggerMode::LifeLostAll
        | TriggerMode::LifeChanged
        | TriggerMode::PayLife
        | TriggerMode::PayCumulativeUpkeep
        | TriggerMode::PayEcho
        | TriggerMode::TokenCreated
        | TriggerMode::TokenCreatedOnce
        | TriggerMode::TurnFaceUp
        | TriggerMode::Transformed
        | TriggerMode::Phase
        | TriggerMode::PhaseIn
        | TriggerMode::PhaseOut
        | TriggerMode::PhaseOutAll
        | TriggerMode::TurnBegin
        | TriggerMode::NewGame
        | TriggerMode::BecomeMonarch
        | TriggerMode::TakesInitiative
        | TriggerMode::LosesGame
        | TriggerMode::Championed
        | TriggerMode::Exerted
        | TriggerMode::Crewed
        | TriggerMode::Crews
        | TriggerMode::Saddled
        | TriggerMode::Saddles
        | TriggerMode::SaddlesOrCrews
        | TriggerMode::Cycled
        | TriggerMode::CycledOrDiscarded
        | TriggerMode::NinjutsuActivated
        | TriggerMode::KeywordAbilityActivated(..)
        | TriggerMode::AbilityActivated
        | TriggerMode::Evolve
        | TriggerMode::Evolved
        | TriggerMode::Explored
        | TriggerMode::Exploited
        | TriggerMode::Enlisted
        | TriggerMode::ManaExpend
        | TriggerMode::LandPlayed
        | TriggerMode::PlayCard
        | TriggerMode::Attached
        | TriggerMode::Unattach
        | TriggerMode::Adapt
        | TriggerMode::Connives
        | TriggerMode::Foretell
        | TriggerMode::Investigated
        | TriggerMode::DungeonCompleted
        | TriggerMode::RoomEntered
        | TriggerMode::PlanarDice
        | TriggerMode::PlaneswalkedFrom
        | TriggerMode::PlaneswalkedTo
        | TriggerMode::ChaosEnsues
        | TriggerMode::RolledDie
        | TriggerMode::RolledDieOnce
        | TriggerMode::FlippedCoin
        | TriggerMode::Clashed
        | TriggerMode::DayTimeChanges
        | TriggerMode::ClassLevelGained
        | TriggerMode::Copied
        | TriggerMode::ConjureAll
        | TriggerMode::Vote
        | TriggerMode::BecomeRenowned
        | TriggerMode::BecomeMonstrous
        | TriggerMode::Proliferate
        | TriggerMode::RingTemptsYou
        | TriggerMode::Surveil
        | TriggerMode::Scry
        | TriggerMode::PlayerPerformedAction
        | TriggerMode::Fight
        | TriggerMode::FightOnce
        | TriggerMode::Abandoned
        | TriggerMode::CaseSolved
        | TriggerMode::ClaimPrize
        | TriggerMode::CollectEvidence
        | TriggerMode::CommitCrime
        | TriggerMode::CrankContraption
        | TriggerMode::Devoured
        | TriggerMode::Discover
        | TriggerMode::Forage
        | TriggerMode::FullyUnlock
        | TriggerMode::GiveGift
        | TriggerMode::ManifestDread
        | TriggerMode::Mentored
        | TriggerMode::Mutates
        | TriggerMode::SearchedLibrary
        | TriggerMode::SeekAll
        | TriggerMode::SetInMotion
        | TriggerMode::Specializes
        | TriggerMode::Stationed
        | TriggerMode::Trains
        | TriggerMode::UnlockDoor
        | TriggerMode::VisitAttraction
        | TriggerMode::BecomesCrewed
        | TriggerMode::BecomesPlotted
        | TriggerMode::BecomesSaddled
        | TriggerMode::Immediate
        | TriggerMode::Always
        | TriggerMode::EntersOrAttacks
        | TriggerMode::AttacksOrBlocks
        | TriggerMode::StateCondition
        | TriggerMode::Airbend
        | TriggerMode::Earthbend
        | TriggerMode::Firebend
        | TriggerMode::Waterbend
        | TriggerMode::ElementalBend
        | TriggerMode::EntersOrHauntedCreatureDies
        | TriggerMode::HauntedCreatureDies
        | TriggerMode::Unknown(..) => None,
    }
}

// ---------------------------------------------------------------------------
// collect_effects — the recursive Effect-collecting walker
// ---------------------------------------------------------------------------

/// Collect every [`Effect`] reachable from an [`AbilityDefinition`]: the head
/// effect, the chained `sub_ability`/`else_ability`/`mode_abilities`, and — via
/// [`collect_effects_in_effect`] — the nested-effect payloads that the display
/// walkers (`build_ability_item`) do *not* descend. Borrows the faces, so the
/// returned references live as long as the input.
fn collect_effects<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
    collect_effects_in_effect(&def.effect, out);
    if let Some(sub) = &def.sub_ability {
        collect_effects(sub, out);
    }
    if let Some(els) = &def.else_ability {
        collect_effects(els, out);
    }
    for m in &def.mode_abilities {
        collect_effects(m, out);
    }
}

/// Push `effect`, then descend the nested-`AbilityDefinition` payloads carried by
/// the variants the display walkers skip (or this projection silently
/// under-counts). Structural traversal — a wildcard covers the leaf variants.
fn collect_effects_in_effect<'a>(effect: &'a Effect, out: &mut Vec<&'a Effect>) {
    out.push(effect);
    match effect {
        Effect::Vote {
            per_choice_effect, ..
        } => {
            for d in per_choice_effect {
                collect_effects(d, out);
            }
        }
        Effect::SeparateIntoPiles {
            chosen_pile_effect, ..
        } => collect_effects(chosen_pile_effect, out),
        Effect::RevealFromHand {
            on_decline: Some(d),
            ..
        } => collect_effects(d, out),
        Effect::CreateDelayedTrigger { effect, .. } => collect_effects(effect, out),
        Effect::RollDie { results, .. } => {
            for branch in results {
                collect_effects(&branch.effect, out);
            }
        }
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            ..
        }
        | Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
        } => {
            if let Some(d) = win_effect {
                collect_effects(d, out);
            }
            if let Some(d) = lose_effect {
                collect_effects(d, out);
            }
        }
        Effect::FlipCoinUntilLose { win_effect } => collect_effects(win_effect, out),
        Effect::ChooseOneOf { branches, .. } => {
            for d in branches {
                collect_effects(d, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Node model
// ---------------------------------------------------------------------------

/// Whether every collected `Effect`/cost of a node (or every member node of a
/// candidate) projected to a modeled `ResourceVector`, or at least one folded to
/// [`Projection::Unmodeled`]. A typed candidate-confidence axis replacing a raw
/// `bool` (CLAUDE.md "no raw bool" / R2) — self-documenting and extensible to
/// finer-grained confidence levels without touching call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelCompleteness {
    /// Every effect/cost projected to a modeled vector.
    #[default]
    FullyModeled,
    /// ≥1 effect/cost folded to [`Projection::Unmodeled`].
    ContainsUnmodeled,
}

impl ModelCompleteness {
    /// Lattice join over completeness: the result is [`Self::ContainsUnmodeled`]
    /// if either operand is (it is the absorbing element). Used to aggregate a
    /// node's effects and to roll member nodes up into a candidate.
    fn merge(self, other: ModelCompleteness) -> ModelCompleteness {
        // Exhaustive over the completeness lattice (no wildcard, mirroring the
        // crate's drift-gate discipline): a future finer-grained variant forces a
        // compile error here so its join is decided explicitly, not silently
        // absorbed into `ContainsUnmodeled`.
        match (self, other) {
            (ModelCompleteness::FullyModeled, ModelCompleteness::FullyModeled) => {
                ModelCompleteness::FullyModeled
            }
            (ModelCompleteness::FullyModeled, ModelCompleteness::ContainsUnmodeled)
            | (ModelCompleteness::ContainsUnmodeled, ModelCompleteness::FullyModeled)
            | (ModelCompleteness::ContainsUnmodeled, ModelCompleteness::ContainsUnmodeled) => {
                ModelCompleteness::ContainsUnmodeled
            }
        }
    }
}

/// One graph node per *ability* across the input faces.
#[derive(Debug, Clone)]
pub struct AbilityNode {
    /// Provenance: the card face this ability came from.
    pub face_name: String,
    /// Spell / Activated / … (informational provenance).
    pub kind: AbilityKind,
    /// Folded signed `Projection`/cost vectors over `collect_effects` + the cost.
    pub net: ResourceVector,
    /// HIGH-1: axes this node produces with `AxisMagnitude::Unbounded`.
    pub unbounded_production: BTreeSet<AxisKey>,
    /// Axes this node drives strictly up (incl. produced tap state + events).
    pub produces: BTreeSet<AxisKey>,
    /// Axes this node costs/needs + the trigger-event axis that fires it.
    pub requires: BTreeSet<AxisKey>,
    /// Whether every collected effect/cost projected, or ≥1 was `Unmodeled`
    /// (candidate-confidence flag).
    pub completeness: ModelCompleteness,
}

/// Mutable accumulator while folding one node's effects and cost.
#[derive(Default)]
struct NodeAcc {
    net: ResourceVector,
    unbounded_production: BTreeSet<AxisKey>,
    /// Field-less produced axes (`Tap`) injected directly.
    produces: BTreeSet<AxisKey>,
    /// Field-less required axes (`Tap`, `AnyCounter`) injected directly.
    requires: BTreeSet<AxisKey>,
    completeness: ModelCompleteness,
}

/// Fold one effect's [`Projection`] into the node accumulator.
fn fold_projection(acc: &mut NodeAcc, proj: Projection) {
    match proj {
        Projection::Modeled {
            vector,
            magnitudes,
            produces,
            requires,
        } => {
            add_into(&mut acc.net, &vector);
            for (key, mag) in magnitudes {
                if matches!(mag, AxisMagnitude::Unbounded) {
                    acc.unbounded_production.insert(key);
                }
            }
            acc.produces.extend(produces);
            acc.requires.extend(requires);
        }
        Projection::Unmodeled => acc.completeness = ModelCompleteness::ContainsUnmodeled,
    }
}

/// CR 106.1: negative mana magnitude a cost consumes — 0 when the cost pays no
/// mana, otherwise at least a unit (dynamic costs stay at unit, HIGH-1; the
/// color is irrelevant under R3-MANA-COLLAPSE so the sink is the colorless slot).
fn mana_cost_amount(cost: &ManaCost) -> i64 {
    if cost.is_without_paying_mana() {
        0
    } else {
        (cost.mana_value() as i64).max(1)
    }
}

fn sink_mana_cost(acc: &mut NodeAcc, cost: &ManaCost) {
    let amount = mana_cost_amount(cost);
    if amount > 0 {
        acc.net.mana[COLORLESS_INDEX] -= amount;
    }
}

/// CR 118 cost fold: the fourth compile-time drift gate — an exhaustive
/// **no-wildcard** match over all 29 [`AbilityCost`] variants. Polarity/sign
/// aware: a cost consumes a resource (negative `net`, ⇒ `requires`) or, in cost
/// position, *produces* one (positive `net`, ⇒ `produces`). Field-less axes
/// (`Tap`, `AnyCounter`) are injected directly.
fn fold_cost(acc: &mut NodeAcc, cost: &AbilityCost) {
    match cost {
        // CR 106.1: mana costs ⇒ requires Mana (R3-MANA-COLLAPSE).
        AbilityCost::Mana { cost } => sink_mana_cost(acc, cost),
        AbilityCost::ManaDynamic { .. } => acc.net.mana[COLORLESS_INDEX] -= 1,
        AbilityCost::Waterbend { cost } => sink_mana_cost(acc, cost),
        AbilityCost::NinjutsuFamily { mana_cost, .. } => sink_mana_cost(acc, mana_cost),
        // CR 701.26a/b: tap costs consume / untap costs produce untapped state.
        AbilityCost::Tap | AbilityCost::TapCreatures { .. } => {
            acc.requires.insert(AxisKey::Tap);
        }
        // {Q} — the untap-cost producer that closes the {Q}-untap engine class.
        AbilityCost::Untap => {
            acc.produces.insert(AxisKey::Tap);
        }
        // CR 306.5b: a planeswalker loyalty cost is sign-aware (R3-COUNTER-COST-SYMMETRY).
        AbilityCost::Loyalty { amount } => {
            if *amount != 0 {
                *acc.net
                    .counters
                    .entry((CounterClass::Loyalty, ObjectClass::Planeswalker))
                    .or_insert(0) += *amount as i64;
            }
        }
        // CR 122.1: Blight puts -1/-1 counters as a cost ⇒ produces that counter.
        AbilityCost::Blight { count } => {
            *acc.net
                .counters
                .entry((CounterClass::Minus1Minus1, ObjectClass::Creature))
                .or_insert(0) += *count as i64;
        }
        // CR 701.21a: sacrificing PRODUCES sac/LTB (and dies) events (R3-SAC-POLARITY).
        AbilityCost::Sacrifice(_) => {
            acc.net.sac_triggers += 1;
            acc.net.ltb_triggers += 1;
            acc.net.death_triggers += 1;
        }
        // CR 122.1: energy cost ⇒ requires the energy counter axis.
        AbilityCost::PayEnergy { .. } => {
            *acc.net
                .counters
                .entry((CounterClass::Energy, ObjectClass::Player))
                .or_insert(0) -= 1;
        }
        // CR 122.1: typed removal ⇒ requires that counter; untyped ⇒ AnyCounter wildcard.
        AbilityCost::RemoveCounter {
            count,
            counter_type,
            ..
        } => match counter_type {
            CounterMatch::OfType(ct) => {
                let class = CounterClass::from_counter_type(ct);
                *acc.net
                    .counters
                    .entry((class, default_object_class(class)))
                    .or_insert(0) -= *count as i64;
            }
            CounterMatch::Any => {
                acc.requires.insert(AxisKey::AnyCounter);
            }
        },
        // CR 118.3: an effect performed as a cost is projected the same way.
        AbilityCost::EffectCost { effect } => fold_projection(acc, effect_projection(effect)),
        // CR 601.2h: a Composite cost is conjunctive — every sub-cost is part of
        // the total cost and all are paid (partial payments are not allowed), so
        // the branches AND-fold (sum) into the node.
        AbilityCost::Composite { costs } => {
            for c in costs {
                fold_cost(acc, c);
            }
        }
        // CR 118.12a: a OneOf cost is disjunctive — the paying player chooses ONE
        // branch ("[do something] unless [a player does something else]"). It must
        // NOT AND-fold; see [`fold_one_of`].
        AbilityCost::OneOf { costs } => fold_one_of(acc, costs),
        // PayLife is deferred to PR-4b with GainLife/LoseLife (R3-LIFE-SYMMETRY).
        // PerCounter is a no-op per plan (its `base` is recursable but not folded).
        // The remaining structural costs carry no modeled axis in PR-4a.
        AbilityCost::PayLife { .. }
        | AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::PaySpeed { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::PerCounter { .. }
        | AbilityCost::Unimplemented { .. } => {}
    }
}

/// Per-key MAX envelope of one [`ResourceVector`] map field across `OneOf`
/// branches, treating a missing key as `0`: union the keys, take the max value,
/// and insert into `out` iff that max is nonzero. The union-of-keys construction
/// (not a pairwise fold) is what makes "branch A has the key, branch B lacks it"
/// resolve to `max(value, 0)` rather than dropping the comparison.
fn max_map_envelope<K, F>(out: &mut BTreeMap<K, i64>, branches: &[NodeAcc], get: F)
where
    K: Copy + Ord,
    F: Fn(&NodeAcc) -> &BTreeMap<K, i64>,
{
    let keys: BTreeSet<K> = branches
        .iter()
        .flat_map(|b| get(b).keys().copied())
        .collect();
    for k in keys {
        let m = branches
            .iter()
            .map(|b| get(b).get(&k).copied().unwrap_or(0))
            .max()
            .unwrap_or(0);
        if m != 0 {
            out.insert(k, m);
        }
    }
}

/// Fold a disjunctive [`AbilityCost::OneOf`] into the accumulator as an
/// **optimistic envelope** over its branches.
///
/// CR 118.12a: a `OneOf` cost is disjunctive — the paying player chooses ONE
/// branch ("[do something] unless [a player does something else]"). The runtime
/// confirms this: casting routes it through `WaitingFor::ActivationCostOneOfChoice`
/// and `cost_payability.rs` deems it payable when ANY branch `is_payable`. A
/// static candidate proposer must therefore NOT AND-fold the branches — summing
/// every alternative invents requirements / net-negative axes that no single
/// branch pays, turning a real loop into a false negative (the candidate that a
/// payable branch closes would be suppressed).
///
/// The envelope maximizes recall (Engine A is the sound confirmer that filters
/// any false positive that survives here):
/// - `produces` / `unbounded_production` = UNION (any branch's production is choosable)
/// - `requires` = INTERSECTION (only the requirements unavoidable in EVERY branch)
/// - `net` = per-axis MAX with missing-component = 0 (the most loop-favorable branch per axis)
/// - `completeness` = merge (`ContainsUnmodeled` if any branch is)
///
/// Self-consistency: `build_node` derives `requires` from a negative `net` sign;
/// `net_env` is negative on an axis ONLY when EVERY branch is negative there
/// (= unavoidable), which matches the explicit `requires` intersection. A
/// single-branch `OneOf` is identical to folding that branch; a nested
/// `OneOf`/`Composite` inside a branch is handled by the recursive temp
/// `fold_cost`.
fn fold_one_of(acc: &mut NodeAcc, costs: &[AbilityCost]) {
    if costs.is_empty() {
        return;
    }
    let branches: Vec<NodeAcc> = costs
        .iter()
        .map(|c| {
            let mut b = NodeAcc::default();
            fold_cost(&mut b, c);
            b
        })
        .collect();

    // produces / unbounded = UNION; completeness = merge across all branches.
    let mut produces_env = BTreeSet::new();
    let mut unbounded_env = BTreeSet::new();
    let mut completeness_env = ModelCompleteness::FullyModeled;
    for b in &branches {
        produces_env.extend(b.produces.iter().copied());
        unbounded_env.extend(b.unbounded_production.iter().copied());
        completeness_env = completeness_env.merge(b.completeness);
    }

    // requires = INTERSECTION (kept only if present in EVERY branch — a cost any
    // single branch dodges is not an unavoidable requirement).
    let mut requires_env = branches[0].requires.clone();
    for b in &branches[1..] {
        requires_env = &requires_env & &b.requires;
    }

    // net = per-axis MAX across ALL branches, missing-component = 0.
    // KEEP IN SYNC with `net_axis_components` / `add_into`: every `ResourceVector`
    // field must be enveloped here, or that axis of a `OneOf` cost is silently
    // mis-modeled. A new field is a compile error there, not here — re-check this
    // walk whenever those two are extended.
    let mut net_env = ResourceVector::default();
    for i in 0..6 {
        net_env.mana[i] = branches.iter().map(|b| b.net.mana[i]).max().unwrap_or(0);
    }
    max_map_envelope(&mut net_env.life, &branches, |b| &b.net.life);
    max_map_envelope(&mut net_env.damage_dealt, &branches, |b| {
        &b.net.damage_dealt
    });
    max_map_envelope(&mut net_env.library_delta, &branches, |b| {
        &b.net.library_delta
    });
    max_map_envelope(&mut net_env.counters, &branches, |b| &b.net.counters);
    max_map_envelope(&mut net_env.generic_triggers, &branches, |b| {
        &b.net.generic_triggers
    });
    net_env.tokens_created = branches
        .iter()
        .map(|b| b.net.tokens_created)
        .max()
        .unwrap_or(0);
    net_env.cards_drawn = branches
        .iter()
        .map(|b| b.net.cards_drawn)
        .max()
        .unwrap_or(0);
    net_env.casts_this_step = branches
        .iter()
        .map(|b| b.net.casts_this_step)
        .max()
        .unwrap_or(0);
    net_env.landfall_triggers = branches
        .iter()
        .map(|b| b.net.landfall_triggers)
        .max()
        .unwrap_or(0);
    net_env.combat_phases = branches
        .iter()
        .map(|b| b.net.combat_phases)
        .max()
        .unwrap_or(0);
    net_env.extra_turns = branches
        .iter()
        .map(|b| b.net.extra_turns)
        .max()
        .unwrap_or(0);
    net_env.death_triggers = branches
        .iter()
        .map(|b| b.net.death_triggers)
        .max()
        .unwrap_or(0);
    net_env.etb_triggers = branches
        .iter()
        .map(|b| b.net.etb_triggers)
        .max()
        .unwrap_or(0);
    net_env.ltb_triggers = branches
        .iter()
        .map(|b| b.net.ltb_triggers)
        .max()
        .unwrap_or(0);
    net_env.sac_triggers = branches
        .iter()
        .map(|b| b.net.sac_triggers)
        .max()
        .unwrap_or(0);

    // Merge the envelope into the live accumulator.
    acc.produces.extend(produces_env);
    acc.unbounded_production.extend(unbounded_env);
    acc.completeness = acc.completeness.merge(completeness_env);
    acc.requires.extend(requires_env);
    add_into(&mut acc.net, &net_env);
}

/// CR 106.1+: enumerate every nonzero [`ResourceVector`] component of `net` as a
/// signed `(ResourceAxis, amount)` pair — the input to the produces/requires
/// derivation. Adding a `ResourceVector` field without extending this walk is a
/// compile error via [`AxisKey::from`]'s exhaustiveness.
fn net_axis_components(net: &ResourceVector) -> Vec<(ResourceAxis, i64)> {
    let mut out = Vec::new();
    for (i, &n) in net.mana.iter().enumerate() {
        if n != 0 {
            out.push((ResourceAxis::Mana(MANA_COLORS[i]), n));
        }
    }
    for (pid, &n) in &net.life {
        if n != 0 {
            out.push((ResourceAxis::Life(*pid), n));
        }
    }
    for (pid, &n) in &net.damage_dealt {
        if n != 0 {
            out.push((ResourceAxis::DamageDealt(*pid), n));
        }
    }
    for (pid, &n) in &net.library_delta {
        if n != 0 {
            out.push((ResourceAxis::LibraryDelta(*pid), n));
        }
    }
    for (&(class, obj), &n) in &net.counters {
        if n != 0 {
            out.push((ResourceAxis::Counter(class, obj), n));
        }
    }
    for (&kind, &n) in &net.generic_triggers {
        if n != 0 {
            out.push((ResourceAxis::Trigger(kind), n));
        }
    }
    for (axis, n) in [
        (ResourceAxis::TokensCreated, net.tokens_created),
        (ResourceAxis::CardsDrawn, net.cards_drawn),
        (ResourceAxis::Casts, net.casts_this_step),
        (ResourceAxis::LandfallTriggers, net.landfall_triggers),
        (ResourceAxis::CombatPhases, net.combat_phases),
        (ResourceAxis::ExtraTurns, net.extra_turns),
        (ResourceAxis::DeathTriggers, net.death_triggers),
        (ResourceAxis::EtbTriggers, net.etb_triggers),
        (ResourceAxis::LtbTriggers, net.ltb_triggers),
        (ResourceAxis::SacTriggers, net.sac_triggers),
    ] {
        if n != 0 {
            out.push((axis, n));
        }
    }
    out
}

/// Component-wise `acc += v` over every [`ResourceVector`] axis.
fn add_into(acc: &mut ResourceVector, v: &ResourceVector) {
    for i in 0..6 {
        acc.mana[i] += v.mana[i];
    }
    for (k, n) in &v.life {
        *acc.life.entry(*k).or_insert(0) += n;
    }
    for (k, n) in &v.damage_dealt {
        *acc.damage_dealt.entry(*k).or_insert(0) += n;
    }
    for (k, n) in &v.library_delta {
        *acc.library_delta.entry(*k).or_insert(0) += n;
    }
    for (k, n) in &v.counters {
        *acc.counters.entry(*k).or_insert(0) += n;
    }
    for (k, n) in &v.generic_triggers {
        *acc.generic_triggers.entry(*k).or_insert(0) += n;
    }
    acc.tokens_created += v.tokens_created;
    acc.cards_drawn += v.cards_drawn;
    acc.casts_this_step += v.casts_this_step;
    acc.landfall_triggers += v.landfall_triggers;
    acc.combat_phases += v.combat_phases;
    acc.extra_turns += v.extra_turns;
    acc.death_triggers += v.death_triggers;
    acc.etb_triggers += v.etb_triggers;
    acc.ltb_triggers += v.ltb_triggers;
    acc.sac_triggers += v.sac_triggers;
}

/// Build one [`AbilityNode`] from a definition (the cost is the node's own
/// `def.cost`; `trigger_req` is the trigger-event axis for trigger nodes).
fn build_node(
    face_name: &str,
    def: &AbilityDefinition,
    trigger_req: Option<AxisKey>,
) -> AbilityNode {
    let mut acc = NodeAcc::default();
    let mut effects = Vec::new();
    collect_effects(def, &mut effects);
    for e in effects {
        fold_projection(&mut acc, effect_projection(e));
    }
    if let Some(cost) = &def.cost {
        fold_cost(&mut acc, cost);
    }

    let mut produces = acc.produces;
    let mut requires = acc.requires;
    for (axis, n) in net_axis_components(&acc.net) {
        let key = AxisKey::from(&axis);
        if n > 0 {
            produces.insert(key);
        } else if n < 0 {
            requires.insert(key);
        }
    }
    // Unbounded-up production axes are produced even if a same-node cost masks
    // the net component (HIGH-1).
    produces.extend(acc.unbounded_production.iter().copied());
    if let Some(req) = trigger_req {
        requires.insert(req);
    }

    AbilityNode {
        face_name: face_name.to_string(),
        kind: def.kind,
        net: acc.net,
        unbounded_production: acc.unbounded_production,
        produces,
        requires,
        completeness: acc.completeness,
    }
}

/// Build every node across the input faces from the four ability sources:
/// spell/activated abilities, trigger executes, replacement executes, and the
/// `GrantAbility`/`GrantTrigger` children of static abilities. A trigger or
/// replacement whose `execute == None` produces no node (LOW-4).
fn build_nodes(faces: &[&CardFace]) -> Vec<AbilityNode> {
    let mut nodes = Vec::new();
    for face in faces {
        for def in &face.abilities {
            nodes.push(build_node(&face.name, def, None));
        }
        for trig in &face.triggers {
            if let Some(def) = &trig.execute {
                nodes.push(build_node(&face.name, def, trigger_axis(&trig.mode)));
            }
        }
        for repl in &face.replacements {
            if let Some(def) = &repl.execute {
                nodes.push(build_node(&face.name, def, None));
            }
        }
        for stat in &face.static_abilities {
            for modi in &stat.modifications {
                match modi {
                    ContinuousModification::GrantAbility { definition } => {
                        nodes.push(build_node(&face.name, definition, None));
                    }
                    ContinuousModification::GrantTrigger { trigger } => {
                        if let Some(def) = &trigger.execute {
                            nodes.push(build_node(&face.name, def, trigger_axis(&trigger.mode)));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    nodes
}

// ---------------------------------------------------------------------------
// Edge model + graph
// ---------------------------------------------------------------------------

/// A producer→consumer resource edge, storing the shared axis key(s) (provenance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceEdge {
    pub via: Vec<AxisKey>,
}

/// The static ability/resource graph — a thin alias over the concrete petgraph
/// type so the public re-export is not dangling (LOW-3).
pub type AbilityGraph = DiGraph<AbilityNode, ResourceEdge>;

/// Does a produced axis satisfy a required axis? Plain equality, except the
/// requires-only [`AxisKey::AnyCounter`] wildcard matches any `Counter(_, _)`
/// producer (R3-COUNTER-FUNGIBILITY). The mana/landfall collapses already
/// happened in [`AxisKey::from`], so they need no special casing here.
fn axis_matches(produced: &AxisKey, required: &AxisKey) -> bool {
    match required {
        AxisKey::AnyCounter => matches!(produced, AxisKey::Counter(_, _)),
        _ => produced == required,
    }
}

/// Build the directed resource graph: edge A→B iff A.produces intersects
/// B.requires under [`axis_matches`]. Self-edges are allowed (a node that both
/// produces and requires the same axis).
pub fn build_ability_graph(nodes: Vec<AbilityNode>) -> AbilityGraph {
    let mut graph = AbilityGraph::new();
    let idxs: Vec<NodeIndex> = nodes.into_iter().map(|n| graph.add_node(n)).collect();
    for &a in &idxs {
        for &b in &idxs {
            let via: Vec<AxisKey> = graph[a]
                .produces
                .iter()
                .copied()
                .filter(|p| graph[b].requires.iter().any(|r| axis_matches(p, r)))
                .collect();
            if !via.is_empty() {
                graph.add_edge(a, b, ResourceEdge { via });
            }
        }
    }
    graph
}

// ---------------------------------------------------------------------------
// Candidate output + SCC/coverability
// ---------------------------------------------------------------------------

/// A static, **unconfirmed** candidate cycle. Deliberately NOT a
/// [`crate::analysis::LoopCertificate`] — it has no driven board-equality proof,
/// so naming it a certificate would violate that type's soundness invariant. It
/// reuses the certificate *vocabulary* (`ResourceAxis`, `WinKind`) so PR-5 can
/// feed [`Self::expected_axes`] to `LoopCertificate::covers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateCycle {
    /// Provenance: the card names whose abilities form the SCC.
    pub faces: Vec<String>,
    /// Summed per-cycle resource vector.
    pub net: ResourceVector,
    /// The named unbounded axes (reused vocabulary — feeds `detect_loop`'s `covers`).
    pub unbounded: Vec<ResourceAxis>,
    /// Tentative win classification.
    pub win_kind: WinKind,
    /// Whether ≥1 member node had unmodeled effects (lower confidence).
    pub completeness: ModelCompleteness,
}

impl CandidateCycle {
    /// The unbounded axes this candidate would pump, for PR-5's `covers()` call.
    pub fn expected_axes(&self) -> &[ResourceAxis] {
        &self.unbounded
    }
}

/// PR-4a candidate coverability (§3.7 step 4). Reuses the shared controller-scoped
/// axis helper [`ResourceVector::unbounded_axes_for`] for the net-progress axis
/// set, and layers the HIGH-1 `unbounded_production` override over the
/// consumed-axis sustainability veto (CR 732.2a): a net-negative consumed axis
/// (mana pool, controller life) is tolerated iff that axis is unbounded-up. The
/// override lives here, not in Engine A's `is_progress`, because it reads
/// magnitude markers that exist only in Engine B.
fn candidate_coverable(
    net: &ResourceVector,
    unbounded_production: &BTreeSet<AxisKey>,
    controller: PlayerId,
) -> bool {
    if net.mana.iter().any(|&n| n < 0) && !unbounded_production.contains(&AxisKey::Mana) {
        return false;
    }
    if net.life.get(&controller).copied().unwrap_or(0) < 0
        && !unbounded_production.contains(&AxisKey::Life)
    {
        return false;
    }
    !net.unbounded_axes_for(controller).is_empty() || !unbounded_production.is_empty()
}

/// Map a coverable [`AxisKey`] back to a named [`ResourceAxis`] at the candidate's
/// sentinel players (§3.7 step 5). For mana, recover the seeded color from the
/// first strictly-positive net slot (color survives the edge-key collapse), else
/// the colorless family sentinel. `Tap`/`AnyCounter` have no `ResourceAxis`.
fn axis_key_to_resource(key: &AxisKey, net: &ResourceVector) -> Option<ResourceAxis> {
    match key {
        AxisKey::Mana => {
            let color = net
                .mana
                .iter()
                .position(|&n| n > 0)
                .map(|i| MANA_COLORS[i])
                .unwrap_or(ManaType::Colorless);
            Some(ResourceAxis::Mana(color))
        }
        // CR 120.1 / CR 119.1 / CR 401: a player-keyed axis is attributed by net
        // sign rather than hardcoded — a controller-directed engine (self-damage,
        // lifegain, self-mill/draw) keeps the CONTROLLER; otherwise the axis is
        // opponent-directed (burn, drain, mill) and keys to OPPONENT.
        AxisKey::Damage => {
            if net.damage_dealt.get(&CONTROLLER).copied().unwrap_or(0) > 0 {
                Some(ResourceAxis::DamageDealt(CONTROLLER))
            } else {
                Some(ResourceAxis::DamageDealt(OPPONENT))
            }
        }
        AxisKey::Life => {
            if net.life.get(&CONTROLLER).copied().unwrap_or(0) > 0 {
                Some(ResourceAxis::Life(CONTROLLER))
            } else {
                Some(ResourceAxis::Life(OPPONENT))
            }
        }
        AxisKey::Library => {
            if net.library_delta.get(&CONTROLLER).copied().unwrap_or(0) != 0 {
                Some(ResourceAxis::LibraryDelta(CONTROLLER))
            } else {
                Some(ResourceAxis::LibraryDelta(OPPONENT))
            }
        }
        AxisKey::Counter(class, obj) => Some(ResourceAxis::Counter(*class, *obj)),
        AxisKey::Trigger(kind) => Some(ResourceAxis::Trigger(*kind)),
        AxisKey::Tokens => Some(ResourceAxis::TokensCreated),
        AxisKey::Casts => Some(ResourceAxis::Casts),
        AxisKey::Draw => Some(ResourceAxis::CardsDrawn),
        AxisKey::Landfall => Some(ResourceAxis::LandfallTriggers),
        AxisKey::Combat => Some(ResourceAxis::CombatPhases),
        AxisKey::ExtraTurn => Some(ResourceAxis::ExtraTurns),
        AxisKey::Etb => Some(ResourceAxis::EtbTriggers),
        AxisKey::Ltb => Some(ResourceAxis::LtbTriggers),
        AxisKey::Death => Some(ResourceAxis::DeathTriggers),
        AxisKey::Sac => Some(ResourceAxis::SacTriggers),
        AxisKey::Tap | AxisKey::AnyCounter => None,
    }
}

/// Engine B's entry point: build the ability graph for a card list, find SCCs
/// (Tarjan), and emit the coverable candidate cycles. Each candidate names the
/// unbounded `ResourceAxis` family it would pump so PR-5's confirmer can be fed a
/// card list and a set of expected axes.
pub fn candidate_cycles(faces: &[&CardFace]) -> Vec<CandidateCycle> {
    candidate_cycles_from_nodes(build_nodes(faces))
}

/// The SCC + coverability core (steps 2–5), separated from node construction so
/// synthetic-node fixtures can drive the full graph/SCC/coverability path.
pub(crate) fn candidate_cycles_from_nodes(nodes: Vec<AbilityNode>) -> Vec<CandidateCycle> {
    let graph = build_ability_graph(nodes);
    let mut out = Vec::new();
    for scc in petgraph::algo::tarjan_scc(&graph) {
        // CR 732.2a: a genuine cycle is a multi-node SCC or a self-looping node;
        // petgraph returns every node as its own trivial SCC.
        let is_cycle = scc.len() > 1 || (scc.len() == 1 && graph.contains_edge(scc[0], scc[0]));
        if !is_cycle {
            continue;
        }

        let mut net = ResourceVector::default();
        let mut unbounded_production = BTreeSet::new();
        let mut completeness = ModelCompleteness::FullyModeled;
        let mut faces_in: Vec<String> = Vec::new();
        for &idx in &scc {
            let node = &graph[idx];
            add_into(&mut net, &node.net);
            unbounded_production.extend(node.unbounded_production.iter().copied());
            completeness = completeness.merge(node.completeness);
            if !faces_in.contains(&node.face_name) {
                faces_in.push(node.face_name.clone());
            }
        }

        if !candidate_coverable(&net, &unbounded_production, CONTROLLER) {
            continue;
        }

        // §3.7 step 5: the controller-scoped strictly-up axes unioned with the
        // unbounded-production axes mapped back to concrete-colored ResourceAxes.
        let mut unbounded = net.unbounded_axes_for(CONTROLLER);
        for key in &unbounded_production {
            if let Some(axis) = axis_key_to_resource(key, &net) {
                if !unbounded.contains(&axis) {
                    unbounded.push(axis);
                }
            }
        }

        out.push(CandidateCycle {
            faces: faces_in,
            win_kind: classify_win_kind(CONTROLLER, &net),
            net,
            unbounded,
            completeness,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        default_target_filter_any, EffectScope, PlayerFilter, SacrificeCost, TriggerDefinition,
    };
    use crate::types::counter::CounterType;

    // --- fixture helpers ---------------------------------------------------

    fn fixed(n: i32) -> QuantityExpr {
        QuantityExpr::Fixed { value: n }
    }
    /// A non-`Fixed` quantity ⇒ unbounded-up production magnitude.
    fn dynamic() -> QuantityExpr {
        QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(fixed(1)),
        }
    }
    fn mana_effect(produced: ManaProduction) -> Effect {
        Effect::Mana {
            produced,
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
            target: None,
        }
    }
    fn colorless(count: QuantityExpr) -> ManaProduction {
        ManaProduction::Colorless { count }
    }
    fn put_counter(ct: CounterType, count: QuantityExpr) -> Effect {
        Effect::PutCounter {
            counter_type: ct,
            count,
            target: default_target_filter_any(),
        }
    }
    fn deal_damage(amount: QuantityExpr) -> Effect {
        Effect::DealDamage {
            amount,
            target: default_target_filter_any(),
            damage_source: None,
        }
    }
    fn set_tap(state: TapStateChange) -> Effect {
        Effect::SetTapState {
            target: default_target_filter_any(),
            scope: EffectScope::Single,
            state,
        }
    }
    fn activated(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, effect)
    }
    fn raw_node(name: &str) -> AbilityNode {
        AbilityNode {
            face_name: name.into(),
            kind: AbilityKind::Activated,
            net: ResourceVector::default(),
            unbounded_production: BTreeSet::new(),
            produces: BTreeSet::new(),
            requires: BTreeSet::new(),
            completeness: ModelCompleteness::FullyModeled,
        }
    }
    const P1P1: (CounterClass, ObjectClass) = (CounterClass::Plus1Plus1, ObjectClass::Creature);

    // === A. effect_projection per-family (revert = delete the arm) ==========

    #[test]
    fn mana_effect_projects_positive_mana() {
        let Projection::Modeled {
            vector, magnitudes, ..
        } = effect_projection(&mana_effect(colorless(fixed(2))))
        else {
            panic!("Effect::Mana must be modeled");
        };
        assert_eq!(vector.mana[COLORLESS_INDEX], 2);
        assert_eq!(
            magnitudes.get(&AxisKey::Mana),
            Some(&AxisMagnitude::Fixed(2))
        );
    }

    #[test]
    fn dynamic_production_marks_axis_unbounded() {
        let Projection::Modeled {
            vector, magnitudes, ..
        } = effect_projection(&mana_effect(colorless(dynamic())))
        else {
            panic!("modeled");
        };
        assert_eq!(
            vector.mana[COLORLESS_INDEX], 1,
            "dynamic production seeds a unit"
        );
        assert_eq!(
            magnitudes.get(&AxisKey::Mana),
            Some(&AxisMagnitude::Unbounded)
        );

        // Negative sibling: the SAME dynamic amount as a COST stays unit and is
        // never marked unbounded (production/cost polarity split, HIGH-1).
        let mut acc = NodeAcc::default();
        fold_cost(
            &mut acc,
            &AbilityCost::ManaDynamic {
                quantity: dynamic(),
            },
        );
        assert_eq!(acc.net.mana[COLORLESS_INDEX], -1);
        assert!(
            acc.unbounded_production.is_empty(),
            "a dynamic cost is not unbounded production"
        );
    }

    #[test]
    fn put_and_remove_counter_sign_split() {
        let Projection::Modeled { vector, .. } =
            effect_projection(&put_counter(CounterType::Plus1Plus1, fixed(3)))
        else {
            panic!("modeled");
        };
        assert_eq!(vector.counters.get(&P1P1), Some(&3));

        let remove = Effect::RemoveCounter {
            counter_type: Some(CounterType::Plus1Plus1),
            count: fixed(3),
            target: default_target_filter_any(),
        };
        let Projection::Modeled { vector, .. } = effect_projection(&remove) else {
            panic!("modeled")
        };
        assert_eq!(vector.counters.get(&P1P1), Some(&-3));
    }

    #[test]
    fn deal_damage_projects_opponent_damage() {
        let Projection::Modeled { vector, .. } = effect_projection(&deal_damage(fixed(1))) else {
            panic!("modeled");
        };
        assert_eq!(vector.damage_dealt.get(&OPPONENT), Some(&1));
    }

    #[test]
    fn set_tap_state_both_polarities() {
        let Projection::Modeled { produces, .. } =
            effect_projection(&set_tap(TapStateChange::Untap))
        else {
            panic!("modeled");
        };
        assert!(
            produces.contains(&AxisKey::Tap),
            "untap produces untapped state"
        );

        let Projection::Modeled { requires, .. } = effect_projection(&set_tap(TapStateChange::Tap))
        else {
            panic!("modeled");
        };
        assert!(
            requires.contains(&AxisKey::Tap),
            "tap consumes untapped state"
        );
    }

    #[test]
    fn unmodeled_effect_projects_nothing() {
        assert!(matches!(
            effect_projection(&Effect::unimplemented("x", "y")),
            Projection::Unmodeled
        ));
        // A deferred-family variant (Draw) is also inert in PR-4a.
        let draw = Effect::Draw {
            count: fixed(1),
            target: default_target_filter_any(),
        };
        assert!(matches!(effect_projection(&draw), Projection::Unmodeled));
    }

    // === B. graph + SCC + coverability (the load-bearing path) ==============

    #[test]
    fn two_node_mana_counter_cycle_is_candidate() {
        // A: costs {1} (mana -1), produces a +1/+1 counter. B: consumes the
        // counter (cost), produces {2} mana. Edges A→B (counter), B→A (mana).
        let mut a = raw_node("A");
        a.net.mana[COLORLESS_INDEX] = -1;
        a.net.counters.insert(P1P1, 1);
        a.produces.insert(AxisKey::Counter(P1P1.0, P1P1.1));
        a.requires.insert(AxisKey::Mana);
        let mut b = raw_node("B");
        b.net.mana[COLORLESS_INDEX] = 2;
        b.net.counters.insert(P1P1, -1);
        b.produces.insert(AxisKey::Mana);
        b.requires.insert(AxisKey::Counter(P1P1.0, P1P1.1));

        let cands = candidate_cycles_from_nodes(vec![a, b]);
        assert_eq!(cands.len(), 1, "the mana/counter cycle is one candidate");
        assert_eq!(
            cands[0].unbounded,
            vec![ResourceAxis::Mana(ManaType::Colorless)]
        );
        assert_eq!(cands[0].win_kind, WinKind::Advantage);
    }

    #[test]
    fn disjoint_producers_yield_no_candidate() {
        let mut a = raw_node("A");
        a.produces.insert(AxisKey::Mana);
        a.net.mana[COLORLESS_INDEX] = 1;
        let mut b = raw_node("B");
        b.produces.insert(AxisKey::Mana);
        b.net.mana[COLORLESS_INDEX] = 1;
        assert!(
            candidate_cycles_from_nodes(vec![a, b]).is_empty(),
            "two producers with no requires form no edges, no SCC"
        );
    }

    #[test]
    fn net_negative_cycle_is_not_candidate() {
        // The SCC makes net progress on a GAINED axis (a token) but net-SPENDS a
        // consumed axis (mana -2, fixed cost {3} vs {1} production) ⇒ unsustainable
        // ⇒ rejected by the consumed-axis veto. The token isolates the veto: it
        // passes the net-progress gate, so the ONLY rejecter is sustainability.
        // REVERT PROBE: remove the mana<0 veto in `candidate_coverable` ⇒ the token
        // makes it wrongly emit.
        let mut a = raw_node("A");
        a.net.mana[COLORLESS_INDEX] = -3;
        a.net.tokens_created = 1; // a gained axis that survives the cycle
        a.net.counters.insert(P1P1, 1);
        a.produces.insert(AxisKey::Counter(P1P1.0, P1P1.1));
        a.requires.insert(AxisKey::Mana);
        let mut b = raw_node("B");
        b.net.mana[COLORLESS_INDEX] = 1;
        b.net.counters.insert(P1P1, -1);
        b.produces.insert(AxisKey::Mana);
        b.requires.insert(AxisKey::Counter(P1P1.0, P1P1.1));
        assert!(
            candidate_cycles_from_nodes(vec![a, b]).is_empty(),
            "a net-negative mana cycle with no unbounded production is not coverable"
        );
    }

    #[test]
    fn unbounded_production_covers_fixed_cost() {
        // HIGH-1, the Priest+Mantle shape: mana net -2 (fixed cost {3} vs
        // unit-seeded production) BUT the production side is unbounded-up ⇒ the
        // override forces the mana axis coverable. REVERT PROBE: drop the
        // `unbounded_production` clause of `candidate_coverable` ⇒ 0 candidates.
        let mut a = raw_node("A");
        a.net.mana[COLORLESS_INDEX] = 1;
        a.unbounded_production.insert(AxisKey::Mana);
        a.produces.insert(AxisKey::Mana);
        a.requires.insert(AxisKey::Tap);
        let mut b = raw_node("B");
        b.net.mana[COLORLESS_INDEX] = -3;
        b.produces.insert(AxisKey::Tap);
        b.requires.insert(AxisKey::Mana);

        let cands = candidate_cycles_from_nodes(vec![a, b]);
        assert_eq!(cands.len(), 1);
        assert!(cands[0]
            .unbounded
            .iter()
            .any(|x| matches!(x, ResourceAxis::Mana(_))));
    }

    #[test]
    fn untap_cost_node_produces_tap_and_closes_scc() {
        // HIGH-1 RESIDUAL: build the nodes through `build_node` so the real
        // AbilityCost::Untap → produces:Tap arm fires. REVERT PROBE: reclassify
        // that cost arm to no-op/requires ⇒ no B→…→A back-edge ⇒ 0 candidates.
        let mut def_b = activated(Effect::unimplemented("test", "umbral pump"));
        def_b.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(3),
                },
                AbilityCost::Untap,
            ],
        });
        let node_b = build_node("Umbral", &def_b, None);
        assert!(
            node_b.produces.contains(&AxisKey::Tap),
            "{{Q}} untap cost produces Tap"
        );
        assert!(
            node_b.requires.contains(&AxisKey::Mana),
            "{{3}} cost requires Mana"
        );
        // Paired sibling: a tap cost requires (not produces) Tap.
        let mut def_tap = activated(Effect::unimplemented("test", "tapper"));
        def_tap.cost = Some(AbilityCost::Tap);
        assert!(build_node("Tapper", &def_tap, None)
            .requires
            .contains(&AxisKey::Tap));

        let mut def_a = activated(mana_effect(colorless(dynamic())));
        def_a.cost = Some(AbilityCost::Tap);
        let node_a = build_node("Priest", &def_a, None);
        assert!(node_a.unbounded_production.contains(&AxisKey::Mana));

        let cands = candidate_cycles_from_nodes(vec![node_a, node_b]);
        assert_eq!(cands.len(), 1, "the {{Q}}-untap engine closes its SCC");
        assert!(cands[0]
            .unbounded
            .iter()
            .any(|x| matches!(x, ResourceAxis::Mana(_))));
    }

    #[test]
    fn colored_mana_feeds_generic_cost_is_candidate() {
        // R3-MANA-COLLAPSE: a Mana(Green) unbounded producer feeds a GENERIC {3}
        // (colorless) cost. Built through `build_node` so produces/requires are
        // DERIVED from the colored/colorless net via `From<&ResourceAxis>` — the
        // collapse is genuinely exercised, not hardcoded. REVERT PROBE: make `From`
        // key the colorless generic cost distinctly from colored production ⇒
        // A.produces ∩ B.requires == ∅, the A→B edge never forms, 0 candidates.
        let mut def_a = activated(mana_effect(ManaProduction::AnyCombination {
            count: dynamic(),
            color_options: vec![ManaColor::Green],
        }));
        def_a.cost = Some(AbilityCost::Tap);
        let node_a = build_node("GreenSource", &def_a, None);
        assert_eq!(
            node_a.net.mana[4], 1,
            "green is seeded by the singleton color set"
        );
        assert!(node_a.produces.contains(&AxisKey::Mana));
        assert!(node_a.requires.contains(&AxisKey::Tap));

        let mut def_b = activated(Effect::unimplemented("t", "generic pump"));
        def_b.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(3),
                },
                AbilityCost::Untap,
            ],
        });
        let node_b = build_node("GenericPump", &def_b, None);
        assert_eq!(
            node_b.net.mana[COLORLESS_INDEX], -3,
            "generic {{3}} sinks to colorless"
        );
        assert!(
            node_b.requires.contains(&AxisKey::Mana),
            "generic cost requires the fungible Mana key"
        );
        assert!(node_b.produces.contains(&AxisKey::Tap));

        let cands = candidate_cycles_from_nodes(vec![node_a, node_b]);
        assert_eq!(cands.len(), 1);
        assert!(
            cands[0]
                .unbounded
                .contains(&ResourceAxis::Mana(ManaType::Green)),
            "the seeded green color is recovered for the output axis"
        );
    }

    #[test]
    fn any_counter_cost_matches_typed_counter_producer() {
        // R3-COUNTER-FUNGIBILITY: an AnyCounter requirement intersects any typed
        // counter producer; a typed-mismatched requirement does not.
        let mut producer = raw_node("Producer");
        producer.produces.insert(AxisKey::Counter(P1P1.0, P1P1.1));
        let mut any_consumer = raw_node("AnyConsumer");
        any_consumer.requires.insert(AxisKey::AnyCounter);
        let mut typed_consumer = raw_node("TypedConsumer");
        typed_consumer.requires.insert(AxisKey::Counter(
            CounterClass::Loyalty,
            ObjectClass::Planeswalker,
        ));

        let g = build_ability_graph(vec![producer, any_consumer, typed_consumer]);
        assert!(
            g.contains_edge(NodeIndex::new(0), NodeIndex::new(1)),
            "AnyCounter matches the +1/+1 producer"
        );
        assert!(
            !g.contains_edge(NodeIndex::new(0), NodeIndex::new(2)),
            "a typed Loyalty requirement does not match a +1/+1 producer"
        );
    }

    #[test]
    fn cost_position_counter_production_is_producer() {
        // R3-COUNTER-COST-SYMMETRY: loyalty + / Blight are cost-position producers;
        // loyalty - is a requirer. REVERT PROBE (Loyalty→no-op) drops `np.produces`.
        let mut plus = activated(Effect::unimplemented("t", "loy+"));
        plus.cost = Some(AbilityCost::Loyalty { amount: 2 });
        let np = build_node("Plus", &plus, None);
        assert!(np.produces.contains(&AxisKey::Counter(
            CounterClass::Loyalty,
            ObjectClass::Planeswalker
        )));

        let mut minus = activated(Effect::unimplemented("t", "loy-"));
        minus.cost = Some(AbilityCost::Loyalty { amount: -7 });
        let nm = build_node("Minus", &minus, None);
        assert!(nm.requires.contains(&AxisKey::Counter(
            CounterClass::Loyalty,
            ObjectClass::Planeswalker
        )));

        let mut blight = activated(Effect::unimplemented("t", "blight"));
        blight.cost = Some(AbilityCost::Blight { count: 1 });
        let nb = build_node("Blight", &blight, None);
        assert!(nb.produces.contains(&AxisKey::Counter(
            CounterClass::Minus1Minus1,
            ObjectClass::Creature
        )));
    }

    #[test]
    fn sacrifice_cost_produces_sac_and_ltb_events() {
        // R3-SAC-POLARITY: sacrificing is an event PRODUCER (sac/ltb/dies), not a
        // requirer. REVERT PROBE: grouping Sacrifice as `requires` flips all three.
        let mut sac = activated(Effect::unimplemented("t", "sac"));
        sac.cost = Some(AbilityCost::Sacrifice(SacrificeCost::count(
            default_target_filter_any(),
            1,
        )));
        let ns = build_node("Sac", &sac, None);
        assert!(ns.produces.contains(&AxisKey::Sac));
        assert!(ns.produces.contains(&AxisKey::Ltb));
        assert!(ns.produces.contains(&AxisKey::Death));
        assert!(
            !ns.requires.contains(&AxisKey::Sac),
            "sacrifice does not REQUIRE the sac axis"
        );
    }

    #[test]
    fn opponent_damage_cycle_classifies_lethal() {
        // A mana engine (A) feeds a pinger (B) that deals 1 to the opponent and
        // untaps the engine (SetTapState{Untap}) each cycle ⇒ LethalDamage. The
        // damage is routed through the real DealDamage→add_damage path (not
        // hardcoded), so REVERT PROBE: key add_damage to CONTROLLER ⇒ win_kind
        // flips to Advantage and the candidate names DamageDealt(CONTROLLER).
        let mut def_a = activated(mana_effect(colorless(dynamic())));
        def_a.cost = Some(AbilityCost::Tap);
        let node_a = build_node("Engine", &def_a, None);

        let mut def_b = activated(deal_damage(fixed(1)));
        def_b.sub_ability = Some(Box::new(activated(set_tap(TapStateChange::Untap))));
        def_b.cost = Some(AbilityCost::Mana {
            cost: ManaCost::generic(1),
        });
        let node_b = build_node("Pinger", &def_b, None);
        assert!(
            node_b.produces.contains(&AxisKey::Tap),
            "the untap effect produces Tap"
        );

        let cands = candidate_cycles_from_nodes(vec![node_a, node_b]);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].win_kind, WinKind::LethalDamage);
        assert!(cands[0]
            .unbounded
            .contains(&ResourceAxis::DamageDealt(OPPONENT)));
        // DISCRIMINATION: the same net, with the victim AS controller, is
        // self-damage ⇒ Advantage (the controller-scoped classification).
        assert_eq!(
            classify_win_kind(OPPONENT, &cands[0].net),
            WinKind::Advantage,
            "the same damage, with the victim as controller, is self-damage (Advantage)"
        );
    }

    // === C. walker completeness (revert = drop a recursion arm) =============

    #[test]
    fn collect_effects_descends_nested_effect_variants() {
        let branch = AbilityDefinition::new(AbilityKind::Spell, mana_effect(colorless(fixed(1))));
        let mut top = activated(Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: vec![branch],
        });
        top.sub_ability = Some(Box::new(activated(put_counter(
            CounterType::Plus1Plus1,
            fixed(1),
        ))));

        let mut effects = Vec::new();
        collect_effects(&top, &mut effects);
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Mana { .. })),
            "the ChooseOneOf branch's Mana is collected"
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::PutCounter { .. })),
            "the sub_ability PutCounter is collected"
        );
    }

    #[test]
    fn collect_effects_skips_execute_none() {
        let mut none_face = CardFace {
            name: "TrigNone".into(),
            ..CardFace::default()
        };
        none_face
            .triggers
            .push(TriggerDefinition::new(TriggerMode::SpellCast));
        assert!(
            build_nodes(&[&none_face]).is_empty(),
            "a trigger with execute == None yields no node"
        );

        let mut some_face = CardFace {
            name: "TrigSome".into(),
            ..CardFace::default()
        };
        let mut trig = TriggerDefinition::new(TriggerMode::SpellCast);
        trig.execute = Some(Box::new(activated(mana_effect(colorless(fixed(1))))));
        some_face.triggers.push(trig);
        assert_eq!(
            build_nodes(&[&some_face]).len(),
            1,
            "execute == Some yields one node"
        );
    }

    // === D. real-card-data corpus smoke (export-gated graceful skip) ========

    #[test]
    fn corpus_priority_family_combo_yields_candidate() {
        let db = crate::test_support::shared_card_db();
        let (Some(priest), Some(mantle)) = (
            db.get_face_by_name("Priest of Titania"),
            db.get_face_by_name("Umbral Mantle"),
        ) else {
            return; // export/fixture absent: skip gracefully, never fail spuriously
        };

        // The Umbral-Mantle granted ability's {Q} untap cost must surface as a Tap producer.
        let nodes = build_nodes(&[mantle]);
        assert!(
            nodes.iter().any(|n| n.produces.contains(&AxisKey::Tap)),
            "Umbral Mantle's granted {{Q}} untap cost produces the Tap axis"
        );

        let cands = candidate_cycles(&[priest, mantle]);
        assert!(
            cands.iter().any(|c| {
                c.faces.iter().any(|f| f == "Priest of Titania")
                    && c.faces.iter().any(|f| f == "Umbral Mantle")
                    && c.unbounded
                        .iter()
                        .any(|a| matches!(a, ResourceAxis::Mana(_)))
            }),
            "Priest of Titania + Umbral Mantle yields a mana-family candidate cycle; got {cands:?}"
        );
    }

    // === E. PR-4a review resolution (PR #4493) ==============================

    #[test]
    fn one_of_cost_disjunctive_envelope_emits_candidate() {
        // MAINTAINER REGRESSION (PR #4493): `AbilityCost::OneOf` is disjunctive —
        // the payer chooses ONE branch (CR 118.12a). The payoff node B's cost is
        // `OneOf { {1}  |  {100} }`: the cheap {1} branch closes the loop, the
        // {100} branch is unsustainable mana the real loop never pays. The
        // candidate MUST still be emitted (the proposer envelopes optimistically).
        //
        // DISCRIMINATION: revert `fold_one_of` to the AND-fold (`for c in costs {
        // fold_cost(acc, c) }`) and B costs {101}; the cycle's fixed +1 mana then
        // nets to -100 with no UNBOUNDED mana production, so `candidate_coverable`
        // vetoes and this candidate DISAPPEARS (0 emitted). The envelope keeps
        // mana at max(-1,-100) = -1, balanced to 0, so it survives.

        // Engine node A: tap for {1} (FIXED, so mana stays a veto axis) plus an
        // UNBOUNDED +1/+1 counter — the payoff and the non-mana progress axis that
        // makes the cycle coverable without making mana unbounded. Requires Tap.
        let mut def_a = activated(mana_effect(colorless(fixed(1))));
        def_a.sub_ability = Some(Box::new(activated(put_counter(
            CounterType::Plus1Plus1,
            dynamic(),
        ))));
        def_a.cost = Some(AbilityCost::Tap);
        let node_a = build_node("Engine", &def_a, None);
        assert_eq!(
            node_a.net.mana[COLORLESS_INDEX], 1,
            "fixed +1 mana producer"
        );
        assert!(
            node_a
                .unbounded_production
                .contains(&AxisKey::Counter(P1P1.0, P1P1.1)),
            "the dynamic counter is the unbounded progress axis"
        );
        assert!(
            !node_a.unbounded_production.contains(&AxisKey::Mana),
            "mana production is FIXED, so mana stays a coverability veto axis"
        );

        // Payoff node B: untaps the engine (produces Tap), cost = the disjunction.
        let mut def_b = activated(set_tap(TapStateChange::Untap));
        def_b.cost = Some(AbilityCost::OneOf {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Mana {
                    cost: ManaCost::generic(100),
                },
            ],
        });
        let node_b = build_node("Payoff", &def_b, None);
        assert_eq!(
            node_b.net.mana[COLORLESS_INDEX], -1,
            "envelope keeps the cheap branch's mana (max(-1,-100)), not the AND-fold's -101"
        );
        assert!(node_b.requires.contains(&AxisKey::Mana));
        assert!(node_b.produces.contains(&AxisKey::Tap));

        let cands = candidate_cycles_from_nodes(vec![node_a, node_b]);
        assert_eq!(
            cands.len(),
            1,
            "the disjunctive OneOf branch closes the loop; got {cands:?}"
        );
        assert!(cands[0]
            .unbounded
            .iter()
            .any(|a| matches!(a, ResourceAxis::Counter(..))));
    }

    #[test]
    fn one_of_envelope_per_axis_max_and_requires_intersection() {
        // fold_one_of envelope mechanics (PR #4493): net = per-axis MAX (missing
        // component = 0), produces = UNION, requires = INTERSECTION over branches.
        // Cost = `OneOf { {2}  |  ({5} + Tap + Sacrifice) }`; the payer picks ONE.
        let mut def = activated(Effect::unimplemented("t", "disjoint cost"));
        def.cost = Some(AbilityCost::OneOf {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(2),
                },
                AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana {
                            cost: ManaCost::generic(5),
                        },
                        AbilityCost::Tap,
                        AbilityCost::Sacrifice(SacrificeCost::count(
                            default_target_filter_any(),
                            1,
                        )),
                    ],
                },
            ],
        });
        let node = build_node("Disjoint", &def, None);

        // per-axis MAX net: the cheaper mana survives (-2, not the AND-fold's -7),
        // while the sac branch's event production is unioned in (absent in branch
        // A ⇒ treated as 0 ⇒ max(0,1) = 1).
        assert_eq!(
            node.net.mana[COLORLESS_INDEX], -2,
            "envelope keeps the cheaper branch's mana"
        );
        assert_eq!(
            node.net.sac_triggers, 1,
            "the sac branch's production is unioned into the envelope"
        );
        assert!(node.produces.contains(&AxisKey::Sac));
        assert!(node.produces.contains(&AxisKey::Ltb));
        assert!(node.produces.contains(&AxisKey::Death));

        // INTERSECTION requires: Mana (derived from the surviving -2 net) is
        // unavoidable in BOTH branches; Tap is only in branch B ⇒ dropped.
        assert!(node.requires.contains(&AxisKey::Mana));
        assert!(
            !node.requires.contains(&AxisKey::Tap),
            "a requirement present in only one branch is not unavoidable ⇒ dropped by ∩"
        );
    }

    #[test]
    fn axis_key_to_resource_resolves_player_by_net_sign() {
        // gemini CORRECTNESS (PR #4493): player-keyed axes attribute CONTROLLER vs
        // OPPONENT by inspecting `net`, not a hardcoded OPPONENT. Each pair flips
        // the player by moving the nonzero entry — the OLD hardcoded code returned
        // OPPONENT for every controller-directed case, so each CONTROLLER assertion
        // discriminates against it.

        // Life: controller lifegain ⇒ CONTROLLER; opponent drain ⇒ OPPONENT.
        let mut gain = ResourceVector::default();
        gain.life.insert(CONTROLLER, 5);
        assert_eq!(
            axis_key_to_resource(&AxisKey::Life, &gain),
            Some(ResourceAxis::Life(CONTROLLER))
        );
        let mut drain = ResourceVector::default();
        drain.life.insert(OPPONENT, -5);
        assert_eq!(
            axis_key_to_resource(&AxisKey::Life, &drain),
            Some(ResourceAxis::Life(OPPONENT))
        );

        // Damage: self-damage engine ⇒ CONTROLLER; opponent burn ⇒ OPPONENT.
        let mut self_dmg = ResourceVector::default();
        self_dmg.damage_dealt.insert(CONTROLLER, 3);
        assert_eq!(
            axis_key_to_resource(&AxisKey::Damage, &self_dmg),
            Some(ResourceAxis::DamageDealt(CONTROLLER))
        );
        let mut opp_dmg = ResourceVector::default();
        opp_dmg.damage_dealt.insert(OPPONENT, 3);
        assert_eq!(
            axis_key_to_resource(&AxisKey::Damage, &opp_dmg),
            Some(ResourceAxis::DamageDealt(OPPONENT))
        );

        // Library: self-mill/draw (any nonzero controller delta) ⇒ CONTROLLER;
        // opponent mill ⇒ OPPONENT.
        let mut self_lib = ResourceVector::default();
        self_lib.library_delta.insert(CONTROLLER, -7);
        assert_eq!(
            axis_key_to_resource(&AxisKey::Library, &self_lib),
            Some(ResourceAxis::LibraryDelta(CONTROLLER))
        );
        let mut opp_lib = ResourceVector::default();
        opp_lib.library_delta.insert(OPPONENT, -7);
        assert_eq!(
            axis_key_to_resource(&AxisKey::Library, &opp_lib),
            Some(ResourceAxis::LibraryDelta(OPPONENT))
        );
    }

    #[test]
    fn model_completeness_tracks_and_rolls_up_unmodeled() {
        // gemini R2 (PR #4493): the typed completeness flag replaces a raw bool.
        // A modeled effect ⇒ FullyModeled; an unmodeled one ⇒ ContainsUnmodeled.
        let modeled = build_node(
            "Modeled",
            &activated(mana_effect(colorless(fixed(1)))),
            None,
        );
        assert_eq!(modeled.completeness, ModelCompleteness::FullyModeled);

        let unmodeled = build_node(
            "Unmodeled",
            &activated(Effect::unimplemented("x", "y")),
            None,
        );
        assert_eq!(unmodeled.completeness, ModelCompleteness::ContainsUnmodeled);
        assert_ne!(unmodeled.completeness, ModelCompleteness::FullyModeled);

        // merge is the absorbing lattice join.
        assert_eq!(
            ModelCompleteness::FullyModeled.merge(ModelCompleteness::FullyModeled),
            ModelCompleteness::FullyModeled
        );
        assert_eq!(
            ModelCompleteness::FullyModeled.merge(ModelCompleteness::ContainsUnmodeled),
            ModelCompleteness::ContainsUnmodeled
        );
        assert_eq!(
            ModelCompleteness::ContainsUnmodeled.merge(ModelCompleteness::FullyModeled),
            ModelCompleteness::ContainsUnmodeled
        );

        // Candidate-level rollup over a 2-node mana/counter cycle: all-modeled
        // members ⇒ FullyModeled; one unmodeled member ⇒ ContainsUnmodeled.
        let mut a = raw_node("A");
        a.net.mana[COLORLESS_INDEX] = -1;
        a.net.counters.insert(P1P1, 1);
        a.produces.insert(AxisKey::Counter(P1P1.0, P1P1.1));
        a.requires.insert(AxisKey::Mana);
        let mut b = raw_node("B");
        b.net.mana[COLORLESS_INDEX] = 2;
        b.net.counters.insert(P1P1, -1);
        b.produces.insert(AxisKey::Mana);
        b.requires.insert(AxisKey::Counter(P1P1.0, P1P1.1));

        let modeled_cands = candidate_cycles_from_nodes(vec![a.clone(), b.clone()]);
        assert_eq!(modeled_cands.len(), 1);
        assert_eq!(
            modeled_cands[0].completeness,
            ModelCompleteness::FullyModeled,
            "an all-modeled cycle must NOT report ContainsUnmodeled"
        );

        a.completeness = ModelCompleteness::ContainsUnmodeled;
        let mixed_cands = candidate_cycles_from_nodes(vec![a, b]);
        assert_eq!(mixed_cands.len(), 1);
        assert_eq!(
            mixed_cands[0].completeness,
            ModelCompleteness::ContainsUnmodeled,
            "one unmodeled member rolls the candidate up to ContainsUnmodeled"
        );
    }
}
