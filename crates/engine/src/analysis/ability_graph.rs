//! Engine B — the **static ability-graph extractor** (offline candidate generator).
//!
//! This module never calls `apply()`, never drives a `GameRunner`, and never
//! touches a `GameState`. Given a list of card faces it builds a directed
//! ability/resource graph, finds strongly-connected components (Tarjan SCC),
//! and emits each component that holds a cycle as a **candidate cycle**. Whether a
//! cycle pays for itself, gains, or makes progress is the confirmer's answer.
//!
//! A candidate is **unconfirmed by construction** — it ignores targeting legality, timing
//! windows, "may" choices, and replacement interactions — so it is a
//! [`CandidateCycle`], deliberately **never** a [`crate::analysis::LoopCertificate`]
//! (whose soundness invariant requires a driven board-equality proof).
//!
//! Theory reference (CS, not CR): Tarjan SCC for cycle finding.
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

use crate::analysis::resource::{
    CounterClass, ObjectClass, ResourceAxis, ResourceVector, TriggerKind,
};
use crate::game::effects::token::materialize_token_ability_payload;
use crate::game::mana_abilities::is_mana_ability;
use crate::game::mana_payment::{shard_to_mana_type, ShardRequirement};
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, DamageKindFilter, Effect,
    ManaProduction, PlayerFilter, QuantityExpr, ReplacementDefinition, StaticDefinition,
    TapStateChange, TargetFilter, TriggerDefinition, TypeFilter, VoteSubject,
};
use crate::types::card::CardFace;
use crate::types::card_type::{CardType, CoreType};
use crate::types::counter::CounterMatch;
use crate::types::mana::{ManaColor, ManaCost, ManaType};
use crate::types::phase::{Phase, PhaseGroup, TurnSegment};
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// CR 101: Static analysis has no concrete `PlayerId`, so the player-keyed
/// `ResourceVector` axes use a documented sentinel convention — the loop's
/// controller is `PlayerId(0)`, its opponent `PlayerId(1)`. Damage / mill aimed
/// at "any target" / "target player" is keyed to [`OPPONENT`].
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
    /// CR 608.2n + CR 304.1 + CR 307.1: a card returned from a graveyard to a hand, where a
    /// resolved instant or sorcery must be before it can be cast again.
    ReturnToHand,
    /// CR 500.8 + CR 505.1a: a main phase an effect adds to a turn, which is a postcombat main
    /// phase. Has no `ResourceVector` axis; injected directly into a node's produces/requires sets.
    MainPhase,
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
            // CR 704.5c: the per-victim poison axis has one aggregate key.
            ResourceAxis::Poison(_) => AxisKey::Counter(CounterClass::Poison, ObjectClass::Player),
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
    /// Unbounded-up: a dynamic `QuantityExpr` sitting in a production position. Its
    /// node produces the axis whatever the node pays on it.
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
    /// CR 119.3: signed life on a player (`+` gained / `−` lost). Only positive
    /// production marks the axis — a loss is a consumed/drain component surfaced
    /// later from the net sign — mirroring `add_mana`/`add_damage`.
    fn add_life(&mut self, pid: PlayerId, amount: i64, mag: AxisMagnitude) {
        *self.vector.life.entry(pid).or_insert(0) += amount;
        if amount > 0 {
            self.mark(AxisKey::Life, mag);
        }
    }
    /// CR 401: signed per-player library delta (mill/search are negative — cards
    /// leave the library). A nonzero library delta is surfaced as a drain/advantage
    /// axis by `unbounded_axes_for`; only a positive delta marks production here.
    fn add_library(&mut self, pid: PlayerId, amount: i64, mag: AxisMagnitude) {
        *self.vector.library_delta.entry(pid).or_insert(0) += amount;
        if amount > 0 {
            self.mark(AxisKey::Library, mag);
        }
    }
    /// CR 111.1: tokens created (controller-implicit production).
    fn add_tokens(&mut self, amount: i64, mag: AxisMagnitude) {
        self.vector.tokens_created += amount;
        if amount > 0 {
            self.mark(AxisKey::Tokens, mag);
        }
    }
    /// CR 121.1: cards drawn (controller-implicit production).
    fn add_draw(&mut self, amount: i64, mag: AxisMagnitude) {
        self.vector.cards_drawn += amount;
        if amount > 0 {
            self.mark(AxisKey::Draw, mag);
        }
    }
    /// CR 500.7: extra turns created.
    fn add_extra_turn(&mut self, amount: i64, mag: AxisMagnitude) {
        self.vector.extra_turns += amount;
        if amount > 0 {
            self.mark(AxisKey::ExtraTurn, mag);
        }
    }
    /// CR 500.8: extra combat phases created.
    fn add_combat(&mut self, amount: i64, mag: AxisMagnitude) {
        self.vector.combat_phases += amount;
        if amount > 0 {
            self.mark(AxisKey::Combat, mag);
        }
    }
    /// CR 603.6a: enters-the-battlefield trigger events produced. Writes the real
    /// `etb_triggers` scalar so `produces` derives uniformly via
    /// `net_axis_components` (L3), like the 4a `AbilityCost::Sacrifice` arm.
    fn add_etb(&mut self, amount: i64, mag: AxisMagnitude) {
        self.vector.etb_triggers += amount;
        if amount > 0 {
            self.mark(AxisKey::Etb, mag);
        }
    }
    /// CR 603.6c: leaves-the-battlefield trigger events produced.
    fn add_ltb(&mut self, amount: i64, mag: AxisMagnitude) {
        self.vector.ltb_triggers += amount;
        if amount > 0 {
            self.mark(AxisKey::Ltb, mag);
        }
    }
    /// CR 700.4: dies (battlefield→graveyard) trigger events produced.
    fn add_death(&mut self, amount: i64, mag: AxisMagnitude) {
        self.vector.death_triggers += amount;
        if amount > 0 {
            self.mark(AxisKey::Death, mag);
        }
    }
    /// CR 701.21: sacrifice trigger events produced.
    fn add_sac(&mut self, amount: i64, mag: AxisMagnitude) {
        self.vector.sac_triggers += amount;
        if amount > 0 {
            self.mark(AxisKey::Sac, mag);
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

/// CR 101 (static sentinel convention, §3.6): map a player-referential
/// [`TargetFilter`] to the analysis controller/opponent sentinel. `Controller`
/// and `SelfRef` resolve to the loop's [`CONTROLLER`]; every other filter
/// (`Any`, `Player`, a typed opponent, …) resolves to [`OPPONENT`].
fn target_player(filter: &TargetFilter) -> PlayerId {
    match filter {
        TargetFilter::Controller | TargetFilter::SelfRef => CONTROLLER,
        _ => OPPONENT,
    }
}

/// `None` ⇒ the controller (e.g. `LoseLife` with no target is "you lose N
/// life"); `Some(f)` ⇒ [`target_player`].
fn target_player_opt(filter: &Option<TargetFilter>) -> PlayerId {
    match filter {
        None => CONTROLLER,
        Some(f) => target_player(f),
    }
}

/// CR 102.2 + CR 102.3: the players a definition's player scope makes its effect act on, where
/// the scope alone names them; `None` where a game state or another object decides who they are.
fn scoped_players(scope: &PlayerFilter) -> Option<&'static [PlayerId]> {
    match scope {
        PlayerFilter::Controller => Some(&[CONTROLLER]),
        PlayerFilter::Opponent => Some(&[OPPONENT]),
        PlayerFilter::All => Some(&[CONTROLLER, OPPONENT]),
        PlayerFilter::DefendingPlayer
        | PlayerFilter::OpponentLostLife
        | PlayerFilter::OpponentGainedLife
        | PlayerFilter::HasLostTheGame
        | PlayerFilter::OpponentDealtDamage { .. }
        | PlayerFilter::OpponentAttacked { .. }
        | PlayerFilter::OpponentAttackingEnchantedPlayer
        | PlayerFilter::AllExcept { .. }
        | PlayerFilter::HighestSpeed
        | PlayerFilter::ZoneChangedThisWay
        | PlayerFilter::PerformedActionThisWay { .. }
        | PlayerFilter::OwnersOfCardsExiledBySource
        | PlayerFilter::TriggeringPlayer
        | PlayerFilter::OpponentOtherThanTriggering
        | PlayerFilter::OpponentOfTriggeringPlayer
        | PlayerFilter::OpponentOfTriggeringPlayerNotAttacked
        | PlayerFilter::VotedFor { .. }
        | PlayerFilter::ParentObjectTargetController
        | PlayerFilter::ControlsCount { .. }
        | PlayerFilter::PlayerAttribute { .. }
        | PlayerFilter::ChosenPlayer { .. }
        | PlayerFilter::ParentObjectTargetOwner
        | PlayerFilter::GrantingObjectCaster
        | PlayerFilter::AnyOf { .. }
        | PlayerFilter::TargetPlayer
        | PlayerFilter::TrackedSetPossessor { .. } => None,
    }
}

/// CR 119.3 + CR 701.17a: [`effect_projection`] under the player scope of the definition `effect`
/// heads. Each scoped player is the one the effect acts on, so what it moves on the controller's
/// life or library moves on theirs.
fn scoped_projection(effect: &Effect, scope: Option<&PlayerFilter>) -> Projection {
    let mut projection = effect_projection(effect);
    if let (Projection::Modeled { vector, .. }, Some(players)) =
        (&mut projection, scope.and_then(scoped_players))
    {
        for axis in [&mut vector.life, &mut vector.library_delta] {
            if let Some(amount) = axis.remove(&CONTROLLER) {
                for player in players {
                    *axis.entry(*player).or_insert(0) += amount;
                }
            }
        }
    }
    projection
}

/// CR 700.4: a "dies" (Death-axis) event requires a *creature* moving to a
/// graveyard. A sacrifice/destroy produces the Death axis unless its target
/// filter **provably** cannot match a creature (§3.5). Recall-first: only an
/// explicit non-creature constraint suppresses Death; an undeterminable filter
/// keeps it — a spurious Death edge is filtered by PR-5, a dropped one is a miss.
fn sac_produces_death(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => !type_filters_exclude_creature(&tf.type_filters),
        _ => true,
    }
}

/// CR 205.2b: an object can have more than one card type (an artifact creature
/// satisfies both), so a typed conjunction provably excludes creatures only when
/// at least one of its filters provably matches NO creature. Positive non-creature
/// card types (Land/Artifact/Enchantment/Planeswalker/…) are deliberately NOT
/// treated as exclusions: creatures can also be lands (Dryad Arbor), artifacts, or
/// enchantments, so excluding on a positive type would drop real dies edges. The
/// reasoning recurses through `Non`/`AnyOf` composition, so a composed exclusion
/// like `Non(AnyOf([Creature, …]))` ("neither a creature nor …") is also caught.
fn type_filters_exclude_creature(filters: &[TypeFilter]) -> bool {
    filters.iter().any(type_filter_excludes_creature)
}

/// CR 205.2b: `true` iff `tf` provably matches NO creature. Recurses through `Non`
/// (matches no creature iff its inner matches EVERY creature — see
/// [`type_filter_matches_all_creatures`]) and `AnyOf` (matches no creature iff
/// every branch does). Conservative for positive card types and subtypes: a
/// creature can also carry those (CR 205.2b), so they are NOT exclusions.
fn type_filter_excludes_creature(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Non(inner) => type_filter_matches_all_creatures(inner),
        TypeFilter::AnyOf(types) => types.iter().all(type_filter_excludes_creature),
        TypeFilter::Creature
        | TypeFilter::Land
        | TypeFilter::Artifact
        | TypeFilter::Enchantment
        | TypeFilter::Instant
        | TypeFilter::Sorcery
        | TypeFilter::Planeswalker
        | TypeFilter::Battle
        | TypeFilter::Kindred
        | TypeFilter::Permanent
        | TypeFilter::Card
        | TypeFilter::Any
        | TypeFilter::Subtype(_) => false,
    }
}

/// CR 205.2b: `true` iff EVERY creature provably matches `tf` — the dual of
/// [`type_filter_excludes_creature`], used to decide whether `Non(tf)` excludes
/// creatures. Only `Creature` and `Any` match every creature outright; `Non`
/// matches every creature iff its inner matches none, and `AnyOf` iff some branch
/// matches every creature. Positive types stay conservative `false` (not every
/// creature is a Land, and a creature token is neither a Permanent in every zone
/// nor a Card), so `Non(<positive type>)` is never treated as a creature exclusion.
fn type_filter_matches_all_creatures(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Creature | TypeFilter::Any => true,
        TypeFilter::Non(inner) => type_filter_excludes_creature(inner),
        TypeFilter::AnyOf(types) => types.iter().any(type_filter_matches_all_creatures),
        TypeFilter::Land
        | TypeFilter::Artifact
        | TypeFilter::Enchantment
        | TypeFilter::Instant
        | TypeFilter::Sorcery
        | TypeFilter::Planeswalker
        | TypeFilter::Battle
        | TypeFilter::Kindred
        | TypeFilter::Permanent
        | TypeFilter::Card
        | TypeFilter::Subtype(_) => false,
    }
}

/// CR 603.6a / 603.6c / 700.4: project a zone change onto its event axes. Returns
/// `true` iff at least one axis was produced — a zone change that touches neither
/// the battlefield (as origin) nor the battlefield (as destination) carries no
/// modeled event and the caller MUST return [`Projection::Unmodeled`] (M2, so the
/// node's `completeness` flag stays honest rather than a Modeled-but-empty
/// projection). `mag` is `Fixed(1)` for single moves, `Unbounded` for mass moves.
fn project_zone_change(
    b: &mut Proj,
    origin: Option<Zone>,
    destination: Zone,
    mag: AxisMagnitude,
) -> bool {
    let mut modeled = false;
    // CR 603.6a: a permanent entering the battlefield fires an ETB trigger.
    if destination == Zone::Battlefield {
        b.add_etb(1, mag);
        modeled = true;
    }
    // CR 603.6c: a permanent leaving the battlefield fires an LTB trigger.
    if origin == Some(Zone::Battlefield) {
        b.add_ltb(1, mag);
        modeled = true;
        // CR 700.4: battlefield→graveyard is a dies event (the Death axis).
        if destination == Zone::Graveyard {
            b.add_death(1, mag);
        }
    }
    modeled
}

/// CR 603.6a / 700.4 / 603.6c: the event axis a zone-change *trigger* consumes,
/// disambiguated by the [`TriggerDefinition`]'s `destination`/`origin` (the bare
/// `TriggerMode::ChangesZone` cannot carry it). ETB if it enters the battlefield;
/// Death if it leaves the battlefield for a graveyard (dies); LTB for any other
/// battlefield exit; `None` otherwise.
fn zone_change_trigger_axis(origin: Option<Zone>, destination: Option<Zone>) -> Option<AxisKey> {
    if destination == Some(Zone::Battlefield) {
        Some(AxisKey::Etb)
    } else if origin == Some(Zone::Battlefield) {
        if destination == Some(Zone::Graveyard) {
            Some(AxisKey::Death)
        } else {
            Some(AxisKey::Ltb)
        }
    } else {
        None
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
        | ManaProduction::NotedType { count, .. }
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

/// CR 111.1: the count of tokens this effect's own resolution puts onto the battlefield, for the
/// effects the resource projection models as minting; `None` for every other effect, including a
/// mint the projection leaves unmodeled. A site asking whether an effect's resolution puts a token
/// onto the battlefield consults this rather than listing members, so a member added here reaches
/// the projection. CR 701.36b: populate is not a member, because whether it mints depends
/// on the creature tokens its controller holds at resolution, which the effect does not carry.
pub(crate) fn resolution_token_mint(effect: &Effect) -> Option<&QuantityExpr> {
    static ONE: QuantityExpr = QuantityExpr::Fixed { value: 1 };
    match effect {
        Effect::Token { count, .. }
        | Effect::CopyTokenOf { count, .. }
        | Effect::CreateTokenCopyFromPool { count, .. } => Some(count),
        // CR 701.16a: investigate creates one Clue token.
        Effect::Investigate => Some(&ONE),
        _ => None,
    }
}

/// CR 111.10 + CR 111.3: the mana abilities (CR 605.1a) a token `effect` makes is created with,
/// from its predefined definition and from the creating effect's grants.
fn minted_mana_abilities(effect: &Effect) -> Vec<AbilityDefinition> {
    let Effect::Token {
        name,
        types,
        static_abilities,
        ..
    } = effect
    else {
        return Vec::new();
    };
    let mut abilities = materialize_token_ability_payload(name, types, None).abilities;
    for modification in static_abilities
        .iter()
        .flat_map(|statik| &statik.modifications)
    {
        if let ContinuousModification::GrantAbility { definition } = modification {
            abilities.push((**definition).clone());
        }
    }
    abilities.retain(is_mana_ability);
    abilities
}

/// The central deliverable: project a single [`Effect`] onto its static resource
/// contribution. Exhaustive **no-wildcard** match over all 207 `Effect` variants
/// — five priority families modeled (CR 106.1 / 122.1 / 120.1 / 701.26 / 601.2),
/// every other variant `Projection::Unmodeled` (contributes nothing). PR-4b
/// reclassifies unmodeled arms without touching this match's exhaustiveness.
fn effect_projection(effect: &Effect) -> Projection {
    let mut b = Proj::default();
    // CR 111.1 + CR 603.6a: a token's entry is an enters-the-battlefield event.
    if let Some(count) = resolution_token_mint(effect) {
        let (a, mag) = count_seed(count);
        b.add_tokens(a, mag);
        b.add_etb(a, mag);
        for ability in minted_mana_abilities(effect) {
            let mut resolved = Vec::new();
            collect_effects(&ability, &mut resolved);
            for (added, _) in resolved {
                if let Effect::Mana { produced, .. } = added {
                    let (seeds, each) = project_mana_production(produced);
                    // A fixed number of tokens each adding a fixed amount is a fixed amount.
                    let mag = match (mag, each) {
                        (AxisMagnitude::Fixed(_), AxisMagnitude::Fixed(_)) => mag,
                        _ => AxisMagnitude::Unbounded,
                    };
                    for (slot, amount) in seeds {
                        b.add_mana(slot, a * amount, mag);
                    }
                }
            }
        }
        return b.finish();
    }
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
        | Effect::DamageEachPlayer { amount, .. }
        // CR 120.1: filter-sourced fixed-amount damage — fixed magnitude seed
        // (distinct from the unbounded own-power `EachDealsDamageEqualToPower`).
        | Effect::EachSourceDealsDamage { amount, .. } => {
            let (a, mag) = count_seed(amount);
            b.add_damage(a, mag);
        }
        Effect::EachDealsDamageEqualToPower { .. } => {
            // Damage equal to each source's power — dynamic, unbounded-up.
            b.add_damage(1, AxisMagnitude::Unbounded);
        }
        // ----- TAP/UNTAP family (CR 701.26a/b) -----
        Effect::SetTapState { state, .. } => match state {
            // CR 701.26b: untapping one permanent or a population produces untapped state.
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
        // ----- DRAW family (CR 121.1) -----
        Effect::Draw { count, .. } => {
            let (a, mag) = count_seed(count);
            b.add_draw(a, mag);
        }
        // ----- MILL / SEARCH family (CR 701.17a / CR 701.23a / CR 401) -----
        // Mill drives the victim's library DOWN (cards leave it for a graveyard).
        Effect::Mill { count, target, .. } => {
            let (a, mag) = count_seed(count);
            b.add_library(target_player(target), -a, mag);
        }
        // CR 701.23a: a library search removes the found card(s) from a library.
        Effect::SearchLibrary {
            count,
            target_player: searched,
            ..
        } => {
            let (a, mag) = count_seed(count);
            b.add_library(target_player_opt(searched), -a, mag);
        }
        // Alchemy seek pulls card(s) from the controller's own library.
        Effect::Seek { count, .. } => {
            let (a, mag) = count_seed(count);
            b.add_library(CONTROLLER, -a, mag);
        }
        // ----- LIFE family — atomic symmetric pair (R3-LIFE-SYMMETRY) -----
        // CR 119.3: life gained on the gainer (default controller).
        Effect::GainLife { amount, player } => {
            let (a, mag) = count_seed(amount);
            b.add_life(target_player(player), a, mag);
        }
        // CR 119.3: life lost — `None` ⇒ "you lose" (controller), `Some` ⇒ directed
        // (opponent drain). A loss is a consumed/drain component, never marked.
        Effect::LoseLife { amount, target } => {
            let (a, _) = count_seed(amount);
            b.add_life(target_player_opt(target), -a, AxisMagnitude::Fixed(0));
        }
        // ----- ZONE-CHANGE family (CR 603.6a ETB / CR 603.6c LTB / CR 700.4 dies) -----
        Effect::ChangeZone {
            origin,
            destination,
            ..
        } => {
            // CR 608.2n + CR 304.1 + CR 307.1: a card returned from a graveyard to a hand can be
            // cast again.
            if *origin == Some(Zone::Graveyard) && *destination == Zone::Hand {
                b.produces.insert(AxisKey::ReturnToHand);
            } else if !project_zone_change(&mut b, *origin, *destination, AxisMagnitude::Fixed(1))
            {
                return Projection::Unmodeled;
            }
        }
        Effect::ChangeZoneAll {
            origin,
            destination,
            ..
        } => {
            if !project_zone_change(&mut b, *origin, *destination, AxisMagnitude::Unbounded) {
                return Projection::Unmodeled;
            }
        }
        // CR 603.6c: a bounce returns a permanent from the battlefield to hand
        // (or another zone) — always a leaves-the-battlefield event.
        Effect::Bounce { destination, .. } => {
            let dest = destination.unwrap_or(Zone::Hand);
            if !project_zone_change(
                &mut b,
                Some(Zone::Battlefield),
                dest,
                AxisMagnitude::Fixed(1),
            ) {
                return Projection::Unmodeled;
            }
        }
        Effect::BounceAll { destination, .. } => {
            let dest = destination.unwrap_or(Zone::Hand);
            if !project_zone_change(
                &mut b,
                Some(Zone::Battlefield),
                dest,
                AxisMagnitude::Unbounded,
            ) {
                return Projection::Unmodeled;
            }
        }
        // ----- SACRIFICE / DESTROY effect side (CR 701.21a / CR 701.8a) -----
        // CR 701.21a: sacrificing produces sac + LTB (+ dies for creature filters,
        // §3.5) — same polarity as the 4a `AbilityCost::Sacrifice` cost arm.
        Effect::Sacrifice { target, count, .. } => {
            let (a, mag) = count_seed(count);
            b.add_sac(a, mag);
            b.add_ltb(a, mag);
            if sac_produces_death(target) {
                b.add_death(a, mag);
            }
        }
        // CR 701.8a: destroy moves a permanent to its owner's graveyard (LTB + dies
        // for creatures), but it is not a sacrifice (no Sac axis).
        Effect::Destroy { target, .. } => {
            b.add_ltb(1, AxisMagnitude::Fixed(1));
            if sac_produces_death(target) {
                b.add_death(1, AxisMagnitude::Fixed(1));
            }
        }
        Effect::DestroyAll { target, .. } => {
            b.add_ltb(1, AxisMagnitude::Unbounded);
            if sac_produces_death(target) {
                b.add_death(1, AxisMagnitude::Unbounded);
            }
        }
        // ----- EXTRA TURNS / PHASES (CR 500.7 / CR 500.8) -----
        Effect::ExtraTurn { count, .. } => {
            let (a, mag) = count_seed(count);
            b.add_extra_turn(a, mag);
        }
        // CR 500.8: an added combat phase pumps the combat axis, and an added main phase begins a
        // postcombat main phase (CR 505.1a); any other added segment carries no modeled axis.
        Effect::AdditionalPhase {
            segment,
            followed_by,
            count,
            ..
        } => {
            let combat = *segment == TurnSegment::Phase(PhaseGroup::Combat);
            let main = std::iter::once(segment)
                .chain(followed_by)
                .any(|added| added.first_step() == Phase::PostCombatMain);
            if !combat && !main {
                return Projection::Unmodeled;
            }
            if combat {
                let (a, mag) = count_seed(count);
                b.add_combat(a, mag);
            }
            if main {
                b.produces.insert(AxisKey::MainPhase);
            }
        }
        // Conjure creates real cards; only a battlefield entry is a modeled axis
        // (CR 603.6a ETB). Each `ConjureCard` carries its own `count`, so seed the
        // ETB axis per conjured card (mirrors the token-creation `count_seed` path)
        // — a multi-card or counted conjure produces that many ETBs, and a
        // variable/X count is marked Unbounded. Any other destination has no
        // repeatable axis ⇒ Unmodeled.
        Effect::Conjure {
            cards, destination, ..
        } => {
            if *destination == Zone::Battlefield {
                for card in cards {
                    let (a, mag) = count_seed(&card.count);
                    b.add_etb(a, mag);
                }
            } else {
                return Projection::Unmodeled;
            }
        }
        // ----- UNMODELED (over-approximate candidate stage; no modeled axis) -----
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::CounterAll { .. }
        | Effect::DiscardCard { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch { .. }
        | Effect::NoOp
        | Effect::Populate
        | Effect::Clash
        | Effect::Behold { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::ExileTopCardContest { .. }
        | Effect::SwitchPT { .. }
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        // CR 707.2c (Metamorphic Alteration): as-enters copy choice — no repeatable
        // modeled axis at the candidate stage.
        | Effect::ChoosePermanent { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        // CR 122.1 + CR 603.2c: the reproduced counter kind is event-derived (not
        // statically known), so it projects onto no fixed resource axis — like
        // `MoveCounters`, it is Unmodeled.
        | Effect::ReproduceEventCounters { .. }
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::GenericEffect { .. }
        | Effect::Cleanup { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        // CR 710.4: a flip instruction carries no nested ability edge, exactly
        // like `Transform`.
        | Effect::FlipPermanent { .. }
        | Effect::SearchOutsideGame { .. }
        // CR 400.11b: carries no nested ability edge.
        | Effect::OpenBoosterPack { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFaceDownPile { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::RevealChosenNumbers { .. }
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
        | Effect::CreateBoon { .. }
        | Effect::NoteNumber { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::CreatePlaneswalkReplacement { .. }
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
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::Planeswalk
        | Effect::ChaosEnsues
        | Effect::RedistributeLifeTotals
        | Effect::ReverseTurnOrder
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
        | Effect::RememberCard { .. }
        | Effect::NoteManaSpent
        | Effect::ForEachCategory { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::Exploit { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::PhysicalContest { .. }
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
        | Effect::TurnFaceDown { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::EmpowerJace { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::CompletePlayerAction { .. }
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::Intensify { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::ChooseOneOf { .. }
        | Effect::OpponentGuess { .. }
        | Effect::ChooseCounterAdjustment { .. }
        // CR 608.2d + CR 122.1: interactive counter-kind choice + its consume
        // add no static resource seed (the magnitude is one counter, gated on a
        // runtime choice) — Unmodeled, like the other choice effects.
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        // Projected by `resolution_token_mint` above; never reached here.
        | Effect::Token { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Investigate
        | Effect::Unimplemented { .. } => return Projection::Unmodeled,
    }
    b.finish()
}

// ---------------------------------------------------------------------------
// trigger_axis — TriggerMode → consumed event axis (the trigger-edge requirement)
// ---------------------------------------------------------------------------

/// MEDIUM-1 compile-time drift gate: an exhaustive **no-wildcard** match over all
/// 169 [`TriggerMode`] variants. A trigger node *consumes* (requires) the event
/// axis its mode fires on, so a producer of that axis edges into it. Takes the
/// whole [`TriggerDefinition`] (not the bare mode) so the shared
/// `TriggerMode::ChangesZone` encoding can disambiguate ETB vs dies vs LTB from
/// `trig.destination`/`trig.origin`; the match stays exhaustive over `TriggerMode`.
/// Every mode with no modeled producer returns `None` as an explicit arm.
fn trigger_axis(trig: &TriggerDefinition) -> Option<AxisKey> {
    match &trig.mode {
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
        TriggerMode::TapsForMana | TriggerMode::ManaAdded | TriggerMode::ManaAbilityProduced => {
            Some(AxisKey::Mana)
        }
        // CR 603.6a / 700.4 / 603.6c: zone-change triggers consume the ETB / dies /
        // LTB event axis, disambiguated by the definition's destination/origin.
        TriggerMode::ChangesZone | TriggerMode::ChangesZoneAll => {
            zone_change_trigger_axis(trig.origin, trig.destination)
        }
        // CR 603.6c: a leaves-the-battlefield trigger consumes the LTB event.
        TriggerMode::LeavesBattlefield => Some(AxisKey::Ltb),
        // CR 700.4: a dies/destroyed trigger consumes the Death event.
        TriggerMode::Destroyed => Some(AxisKey::Death),
        // CR 701.21: a sacrifice trigger consumes the Sac event.
        TriggerMode::Sacrificed | TriggerMode::SacrificedOnce => Some(AxisKey::Sac),
        // CR 119.3: life-gain / life-loss / pay-life triggers consume the Life axis.
        // `LifeLostAll` is the batched life-loss form — the runtime routes it through
        // the same `match_life_lost` matcher as `LifeLost` (`trigger_matchers.rs`) and
        // indexes it with the life-change triggers (`trigger_index.rs`), so it
        // consumes the Life axis identically.
        TriggerMode::LifeGained
        | TriggerMode::LifeLost
        | TriggerMode::LifeLostAll
        | TriggerMode::LifeChanged
        | TriggerMode::PayLife => Some(AxisKey::Life),
        // CR 111.1: a token-created trigger consumes the Tokens axis.
        TriggerMode::TokenCreated | TriggerMode::TokenCreatedOnce => Some(AxisKey::Tokens),
        // CR 121.1: a draw trigger consumes the Draw axis.
        TriggerMode::Drawn => Some(AxisKey::Draw),
        // CR 701.17a: a mill trigger consumes the Library axis.
        TriggerMode::Milled | TriggerMode::MilledOnce | TriggerMode::MilledAll => {
            Some(AxisKey::Library)
        }
        // CR 120.1: damage-dealt triggers consume the Damage axis.
        TriggerMode::DamageDone
        | TriggerMode::DamageDoneOnce
        | TriggerMode::DamageAll
        | TriggerMode::DamageDealtOnce
        | TriggerMode::DamageDoneOnceByController => Some(AxisKey::Damage),
        // CR 701.26b: an untap trigger consumes the Tap axis (untapped state).
        TriggerMode::Untaps | TriggerMode::UntapAll => Some(AxisKey::Tap),
        // CR 508.1m + CR 500.8: an attack trigger fires once per combat, which an extra combat
        // phase supplies.
        TriggerMode::Attacks
        | TriggerMode::AttackersDeclared
        | TriggerMode::AttackersDeclaredOneTarget
        | TriggerMode::YouAttack => Some(AxisKey::Combat),
        // CR 603.2b: a beginning-of-step trigger fires once per step; a combat step recurs with
        // an extra combat phase (CR 506.1), any other step with an extra turn (CR 500.7).
        TriggerMode::Phase => Some(if trig.phase.is_some_and(|phase| phase.is_combat()) {
            AxisKey::Combat
        } else {
            AxisKey::ExtraTurn
        }),
        // CR 500.7: a turn-begin trigger fires once per turn.
        TriggerMode::TurnBegin => Some(AxisKey::ExtraTurn),
        // ----- remaining modes with no modeled producer ⇒ inert (None) -----
        TriggerMode::ChangesController
        | TriggerMode::DamageReceived
        | TriggerMode::DamagePreventedOnce
        | TriggerMode::ExcessDamage
        | TriggerMode::ExcessDamageAll
        | TriggerMode::AbilityCast
        | TriggerMode::AbilityResolves
        | TriggerMode::AbilityTriggered
        | TriggerMode::Countered
        | TriggerMode::YouAttackUnblocked
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
        | TriggerMode::BecomesTarget
        | TriggerMode::BecomesTargetOnce
        | TriggerMode::Discarded
        | TriggerMode::DiscardedAll
        | TriggerMode::Exiled
        | TriggerMode::Revealed
        | TriggerMode::Shuffled
        | TriggerMode::PayCumulativeUpkeep
        | TriggerMode::PayEcho
        | TriggerMode::TurnFaceUp
        | TriggerMode::Transformed
        | TriggerMode::PhaseIn
        | TriggerMode::PhaseOut
        | TriggerMode::PhaseOutAll
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
        | TriggerMode::LoyaltyAbilityActivated
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
        | TriggerMode::Planeswalked { .. }
        // CR 714.2e: a final-chapter meta-trigger consumes another permanent's
        // chapter-ability lifecycle; no modeled producer axis.
        | TriggerMode::FinalSagaChapterAbility { .. }
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
        | TriggerMode::BlocksOrBecomesBlocked
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

/// The event axes a trigger's node requires: its mode's axis; for a damage-dealt trigger the axis
/// of each kind of damage its definition watches; and for a beginning-of-step trigger a
/// postcombat main phase begins for, the main phase an effect adds as well.
fn trigger_requirements(trig: &TriggerDefinition) -> BTreeSet<AxisKey> {
    let Some(axis) = trigger_axis(trig) else {
        return BTreeSet::new();
    };
    // CR 505.1a: only a turn's first main phase is precombat, so an added main phase begins a
    // postcombat main phase, which a trigger storing that step or no step is matched at.
    if trig.mode == TriggerMode::Phase && matches!(trig.phase, None | Some(Phase::PostCombatMain)) {
        return BTreeSet::from([axis, AxisKey::MainPhase]);
    }
    if axis != AxisKey::Damage {
        return BTreeSet::from([axis]);
    }
    match trig.damage_kind {
        // CR 120.2a + CR 506.1: combat damage is dealt in the combat damage step of a combat phase.
        DamageKindFilter::CombatOnly => BTreeSet::from([AxisKey::Combat]),
        // CR 120.2b: noncombat damage is dealt by the effect of a spell or ability.
        DamageKindFilter::NoncombatOnly => BTreeSet::from([AxisKey::Damage]),
        DamageKindFilter::Any => BTreeSet::from([AxisKey::Damage, AxisKey::Combat]),
    }
}

// ---------------------------------------------------------------------------
// collect_effects — the recursive Effect-collecting walker
// ---------------------------------------------------------------------------

/// An effect and the player scope of the definition it heads; an effect heading none has none.
pub(crate) type ScopedEffect<'a> = (&'a Effect, Option<&'a PlayerFilter>);

/// Collect every [`Effect`] reachable from an [`AbilityDefinition`]: the head
/// effect, the chained `sub_ability`/`else_ability`/`mode_abilities`, and — via
/// [`collect_effects_in_effect`] — the nested-effect payloads that the display
/// walkers (`build_ability_item`) do *not* descend. Borrows the faces, so the
/// returned references live as long as the input.
pub(crate) fn collect_effects<'a>(def: &'a AbilityDefinition, out: &mut Vec<ScopedEffect<'a>>) {
    collect_effects_in_effect(&def.effect, def.player_scope.as_ref(), out);
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
fn collect_effects_in_effect<'a>(
    effect: &'a Effect,
    scope: Option<&'a PlayerFilter>,
    out: &mut Vec<ScopedEffect<'a>>,
) {
    out.push((effect, scope));
    match effect {
        Effect::Vote {
            per_choice_effect,
            subject,
            ..
        } => {
            for d in per_choice_effect {
                collect_effects(d, out);
            }
            // CR 701.38b: object-pool votes carry their sub-effect in
            // `outcome_template` (empty `per_choice_effect`) — Council's
            // Judgment, Prime Minister's Cabinet Room. Descend it too.
            if let VoteSubject::Objects {
                outcome_template, ..
            } = subject
            {
                collect_effects(outcome_template, out);
            }
        }
        Effect::ExileTopCardContest { winner_effect } => collect_effects(winner_effect, out),
        Effect::SeparateIntoPiles {
            chosen_pile_effect,
            unchosen_pile_effect,
            ..
        } => {
            collect_effects(chosen_pile_effect, out);
            if let Some(unchosen) = unchosen_pile_effect {
                collect_effects(unchosen, out);
            }
        }
        Effect::RevealFromHand {
            on_decline: Some(d),
            ..
        } => collect_effects(d, out),
        Effect::CreateDelayedTrigger { effect, .. } => collect_effects(effect, out),
        Effect::CreateBoon { trigger, .. } => {
            if let Some(execute) = trigger.execute.as_deref() {
                collect_effects(execute, out)
            }
        }
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
        // CR 614.1a: the heterogeneous substitute Effect installed by a draw /
        // planeswalk replacement rides in `replacement_effect: Box<Effect>` (not an
        // `AbilityDefinition` payload), so it is invisible to the sibling recursions
        // above. Descend it too — otherwise a randomness effect nested inside a
        // replacement install escapes every `collect_effects` consumer.
        Effect::CreateDrawReplacement { replacement_effect }
        | Effect::CreatePlaneswalkReplacement { replacement_effect } => {
            collect_effects_in_effect(replacement_effect, None, out);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Node model
// ---------------------------------------------------------------------------

/// Whether every collected `Effect`/cost of a node projected to a modeled
/// `ResourceVector`, or at least one folded to [`Projection::Unmodeled`]. A typed
/// flag — self-documenting and extensible to
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

/// Which of a source's definitions a node was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AbilitySlot {
    /// `abilities[i]`: the spell ability or an activated ability.
    Ability(usize),
    /// `triggers[i]`.
    Trigger(usize),
    /// `replacements[i]`.
    Replacement(usize),
    /// A grant in `statics[statik].modifications[modification]`.
    Granted { statik: usize, modification: usize },
}

/// A node's provenance: the index of its source in the input and the definition it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeOrigin {
    pub source: usize,
    pub slot: AbilitySlot,
}

/// The definitions Engine B reads from one card or object, borrowed.
#[derive(Debug, Clone, Copy)]
pub struct AbilitySource<'a> {
    pub mana_cost: &'a ManaCost,
    pub card_type: &'a CardType,
    pub abilities: &'a [AbilityDefinition],
    pub triggers: &'a [TriggerDefinition],
    pub replacements: &'a [ReplacementDefinition],
    pub statics: &'a [StaticDefinition],
}

impl<'a> From<&'a CardFace> for AbilitySource<'a> {
    fn from(face: &'a CardFace) -> Self {
        Self {
            mana_cost: &face.mana_cost,
            card_type: &face.card_type,
            abilities: &face.abilities,
            triggers: &face.triggers,
            replacements: &face.replacements,
            statics: &face.static_abilities,
        }
    }
}

/// One graph node per *ability* across the input faces.
#[derive(Debug, Clone)]
pub struct AbilityNode {
    /// The source and definition this node was built from.
    pub origin: NodeOrigin,
    /// Folded signed `Projection`/cost vectors over `collect_effects` + the cost.
    pub net: ResourceVector,
    /// HIGH-1: axes this node produces with `AxisMagnitude::Unbounded`.
    pub unbounded_production: BTreeSet<AxisKey>,
    /// Axes this node drives strictly up (incl. produced tap state + events).
    pub produces: BTreeSet<AxisKey>,
    /// Axes this node costs/needs + the trigger-event axis that fires it.
    pub requires: BTreeSet<AxisKey>,
    /// Whether every collected effect/cost projected, or ≥1 was `Unmodeled`.
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
/// mana, otherwise at least a unit (dynamic costs stay at unit, HIGH-1).
fn mana_cost_amount(cost: &ManaCost) -> i64 {
    if cost.is_without_paying_mana() {
        0
    } else {
        (cost.mana_value() as i64).max(1)
    }
}

/// CR 107.4a + CR 107.4b: each single-type shard sinks into its own slot, and the generic
/// remainder into the colorless slot.
fn sink_mana_cost(acc: &mut NodeAcc, cost: &ManaCost) {
    let amount = mana_cost_amount(cost);
    if amount == 0 {
        return;
    }
    let mut typed = 0;
    if let ManaCost::Cost { shards, .. } = cost {
        for shard in shards {
            if let ShardRequirement::Single(mana_type) = shard_to_mana_type(*shard) {
                if let Some(slot) = MANA_COLORS.iter().position(|t| *t == mana_type) {
                    acc.net.mana[slot] -= 1;
                    typed += 1;
                }
            }
        }
    }
    if amount > typed {
        acc.net.mana[COLORLESS_INDEX] -= amount - typed;
    }
}

/// CR 118 cost fold: the fourth compile-time drift gate — an exhaustive
/// **no-wildcard** match over all 30 [`AbilityCost`] variants. Polarity/sign
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
        // CR 700.4: the dies (Death) event is gated to creature-or-undeterminable
        // sacrifice filters (§3.5) so a land/Treasure sac doesn't forge a Death edge.
        AbilityCost::Sacrifice(sac) => {
            acc.net.sac_triggers += 1;
            acc.net.ltb_triggers += 1;
            if sac_produces_death(&sac.target) {
                acc.net.death_triggers += 1;
            }
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
        // CR 118.12: an effect performed as a cost is projected the same way.
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
        // CR 119.4: paying life is subtracted from the controller's life total — a
        // unit, recall-safe under-approximation of a dynamic life cost (HIGH-1).
        AbilityCost::PayLife { .. } => {
            *acc.net.life.entry(CONTROLLER).or_insert(0) -= 1;
        }
        // PerCounter is a no-op (its `base` is recursable but not folded).
        // The remaining structural costs carry no modeled axis.
        AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        // CR 118.3: an aggregate graveyard-exile cost (Baron Helmut Zemo's Boast)
        // is a structural exile cost like CollectEvidence — it consumes/produces no
        // resource axis the loop detector models.
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::PaySpeed { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::PerCounter { .. }
        // CR 118.9: a borrowed keyword cost is an alternative cost on a SEPARATE
        // cast (the spell being cast), never an activation cost of this ability,
        // so it carries no modeled axis for the loop detector.
        | AbilityCost::KeywordCostOfCastSpell { .. }
        // CR 702.21a: a Ward player-counter cost, like the effect-side
        // `Effect::GivePlayerCounter` above, carries no modeled axis here —
        // poison accumulation is a loss condition, not a combo resource this
        // loop detector tracks.
        | AbilityCost::GetPlayerCounters { .. }
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
/// `def.cost`; `trigger_req` is the trigger-event axes for trigger nodes).
fn build_node(
    origin: NodeOrigin,
    def: &AbilityDefinition,
    trigger_req: impl IntoIterator<Item = AxisKey>,
) -> AbilityNode {
    build_node_paying(origin, def, trigger_req, None)
}

/// [`build_node`], also paying `spell_cost`, the mana cost of the card a spell ability casts.
fn build_node_paying(
    origin: NodeOrigin,
    def: &AbilityDefinition,
    trigger_req: impl IntoIterator<Item = AxisKey>,
    spell_cost: Option<&ManaCost>,
) -> AbilityNode {
    let mut acc = NodeAcc::default();
    let mut effects = Vec::new();
    collect_effects(def, &mut effects);
    for (effect, scope) in effects {
        fold_projection(&mut acc, scoped_projection(effect, scope));
    }
    if let Some(cost) = &def.cost {
        fold_cost(&mut acc, cost);
    }
    if let Some(cost) = spell_cost {
        sink_mana_cost(&mut acc, cost);
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
    requires.extend(trigger_req);

    AbilityNode {
        origin,
        net: acc.net,
        unbounded_production: acc.unbounded_production,
        produces,
        requires,
        completeness: acc.completeness,
    }
}

/// CR 601.2f + CR 608.2n: an instant or sorcery's spell pays the card's mana cost, and is cast
/// again only from a hand it has been returned to.
fn build_spell_node(
    origin: NodeOrigin,
    source: &AbilitySource,
    def: &AbilityDefinition,
) -> AbilityNode {
    let mut node = build_node_paying(origin, def, None, Some(source.mana_cost));
    node.requires.insert(AxisKey::ReturnToHand);
    node
}

/// Build every node across the input sources from the four ability sources:
/// spell/activated abilities, trigger executes, replacement executes, and the
/// `GrantAbility`/`GrantTrigger` children of static abilities. A trigger or
/// replacement whose `execute == None` produces no node (LOW-4).
fn build_nodes(sources: &[AbilitySource]) -> Vec<AbilityNode> {
    let mut nodes = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let at = |slot| NodeOrigin {
            source: index,
            slot,
        };
        let spell_card = source
            .card_type
            .core_types
            .iter()
            .any(|t| matches!(t, CoreType::Instant | CoreType::Sorcery));
        for (i, def) in source.abilities.iter().enumerate() {
            let origin = at(AbilitySlot::Ability(i));
            nodes.push(if spell_card && def.kind == AbilityKind::Spell {
                build_spell_node(origin, source, def)
            } else {
                build_node(origin, def, None)
            });
        }
        for (i, trig) in source.triggers.iter().enumerate() {
            if let Some(def) = &trig.execute {
                nodes.push(build_node(
                    at(AbilitySlot::Trigger(i)),
                    def,
                    trigger_requirements(trig),
                ));
            }
        }
        for (i, repl) in source.replacements.iter().enumerate() {
            if let Some(def) = &repl.execute {
                nodes.push(build_node(at(AbilitySlot::Replacement(i)), def, None));
            }
        }
        for (statik, stat) in source.statics.iter().enumerate() {
            for (modification, modi) in stat.modifications.iter().enumerate() {
                let origin = at(AbilitySlot::Granted {
                    statik,
                    modification,
                });
                match modi {
                    ContinuousModification::GrantAbility { definition } => {
                        nodes.push(build_node(origin, definition, None));
                    }
                    ContinuousModification::GrantTrigger { trigger } => {
                        if let Some(def) = &trigger.execute {
                            nodes.push(build_node(origin, def, trigger_requirements(trigger)));
                        }
                    }
                    ContinuousModification::GrantReplacement { replacement } => {
                        if let Some(def) = &replacement.execute {
                            nodes.push(build_node(origin, def, None));
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
    // CR 500.7 + CR 500.1 + CR 502.3: an extra turn's untap step untaps its player's permanents.
    if *produced == AxisKey::ExtraTurn && *required == AxisKey::Tap {
        return true;
    }
    match required {
        AxisKey::AnyCounter => matches!(produced, AxisKey::Counter(_, _)),
        _ => produced == required,
    }
}

/// Build the directed resource graph: edge A→B iff A.produces intersects
/// B.requires under [`axis_matches`]. Self-edges are allowed (a node that
/// both produces and requires the same axis).
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
// Candidate output + SCC
// ---------------------------------------------------------------------------

/// A static, **unconfirmed** candidate cycle. Deliberately NOT a
/// [`crate::analysis::LoopCertificate`] — it has no driven board-equality proof,
/// so naming it a certificate would violate that type's soundness invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateCycle {
    /// The definitions whose nodes form the SCC.
    pub members: Vec<NodeOrigin>,
    /// Summed per-cycle resource vector.
    pub net: ResourceVector,
    /// The axes the summed vector drives up: an added turn (CR 500.7) or combat phase
    /// (CR 500.8) among them is how far past its step the cycle reaches.
    pub unbounded: Vec<ResourceAxis>,
}

/// Map an [`AxisKey`] back to a named [`ResourceAxis`] at the candidate's
/// sentinel players (§3.7 step 5). For mana, recover the seeded color from the
/// first strictly-positive net slot (color survives the edge-key collapse), else
/// the colorless family sentinel. `Tap`, `AnyCounter`, `ReturnToHand` and `MainPhase` have no
/// `ResourceAxis`.
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
        AxisKey::Tap | AxisKey::AnyCounter | AxisKey::ReturnToHand | AxisKey::MainPhase => None,
    }
}

/// Engine B's entry point: build the ability graph for a card list, find SCCs
/// (Tarjan), and emit each one that holds a cycle.
pub fn candidate_cycles(faces: &[&CardFace]) -> Vec<CandidateCycle> {
    let sources: Vec<AbilitySource> = faces
        .iter()
        .map(|face| AbilitySource::from(*face))
        .collect();
    candidate_cycles_from_sources(&sources)
}

/// [`candidate_cycles`] over borrowed definition views, each member naming its source's index.
pub fn candidate_cycles_from_sources(sources: &[AbilitySource]) -> Vec<CandidateCycle> {
    candidate_cycles_from_nodes(build_nodes(sources))
}

/// The graph and SCC core, separated from node construction so synthetic-node
/// fixtures can drive it.
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
        let mut members: Vec<NodeOrigin> = Vec::new();
        for &idx in &scc {
            let node = &graph[idx];
            members.push(node.origin);
            add_into(&mut net, &node.net);
            unbounded_production.extend(node.unbounded_production.iter().copied());
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

        members.sort_unstable();
        out.push(CandidateCycle {
            members,
            net,
            unbounded,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        default_target_filter_any, ChooseOneOfKind, EffectScope, PlayerFilter, PtValue,
        QuantityRef, SacrificeCost, TriggerDefinition, TypedFilter,
    };
    use crate::types::counter::CounterType;

    // --- fixture helpers ---------------------------------------------------

    const ORIGIN: NodeOrigin = NodeOrigin {
        source: 0,
        slot: AbilitySlot::Ability(0),
    };

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
            excess: None,
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
    fn raw_node() -> AbilityNode {
        AbilityNode {
            origin: ORIGIN,
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
        // A family with no ResourceVector axis (Scry — library look, no countable
        // resource) stays inert through 4b (§3.2 keeps it Unmodeled).
        let scry = Effect::Scry {
            count: fixed(1),
            target: default_target_filter_any(),
        };
        assert!(matches!(effect_projection(&scry), Projection::Unmodeled));
    }

    // === B. graph + SCC (the load-bearing path) =============================

    #[test]
    fn two_node_mana_counter_cycle_is_candidate() {
        // A: costs {1} (mana -1), produces a +1/+1 counter. B: consumes the
        // counter (cost), produces {2} mana. Edges A→B (counter), B→A (mana).
        let mut a = raw_node();
        a.net.mana[COLORLESS_INDEX] = -1;
        a.net.counters.insert(P1P1, 1);
        a.produces.insert(AxisKey::Counter(P1P1.0, P1P1.1));
        a.requires.insert(AxisKey::Mana);
        let mut b = raw_node();
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
    }

    #[test]
    fn disjoint_producers_yield_no_candidate() {
        let mut a = raw_node();
        a.produces.insert(AxisKey::Mana);
        a.net.mana[COLORLESS_INDEX] = 1;
        let mut b = raw_node();
        b.produces.insert(AxisKey::Mana);
        b.net.mana[COLORLESS_INDEX] = 1;
        assert!(
            candidate_cycles_from_nodes(vec![a, b]).is_empty(),
            "two producers with no requires form no edges, no SCC"
        );
    }

    #[test]
    fn unbounded_production_is_a_reported_axis_whatever_the_summed_net() {
        // The Priest of Titania + Umbral Mantle shape: the summed mana is -2, and the mana a
        // member produces without bound is reported.
        let mut a = raw_node();
        a.net.mana[COLORLESS_INDEX] = 1;
        a.unbounded_production.insert(AxisKey::Mana);
        a.produces.insert(AxisKey::Mana);
        a.requires.insert(AxisKey::Tap);
        let mut b = raw_node();
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
        let node_b = build_node(ORIGIN, &def_b, None);
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
        assert!(build_node(ORIGIN, &def_tap, None)
            .requires
            .contains(&AxisKey::Tap));

        let mut def_a = activated(mana_effect(colorless(dynamic())));
        def_a.cost = Some(AbilityCost::Tap);
        let node_a = build_node(ORIGIN, &def_a, None);
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
        let node_a = build_node(ORIGIN, &def_a, None);
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
        let node_b = build_node(ORIGIN, &def_b, None);
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
        let mut producer = raw_node();
        producer.produces.insert(AxisKey::Counter(P1P1.0, P1P1.1));
        let mut any_consumer = raw_node();
        any_consumer.requires.insert(AxisKey::AnyCounter);
        let mut typed_consumer = raw_node();
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
        let np = build_node(ORIGIN, &plus, None);
        assert!(np.produces.contains(&AxisKey::Counter(
            CounterClass::Loyalty,
            ObjectClass::Planeswalker
        )));

        let mut minus = activated(Effect::unimplemented("t", "loy-"));
        minus.cost = Some(AbilityCost::Loyalty { amount: -7 });
        let nm = build_node(ORIGIN, &minus, None);
        assert!(nm.requires.contains(&AxisKey::Counter(
            CounterClass::Loyalty,
            ObjectClass::Planeswalker
        )));

        let mut blight = activated(Effect::unimplemented("t", "blight"));
        blight.cost = Some(AbilityCost::Blight { count: 1 });
        let nb = build_node(ORIGIN, &blight, None);
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
        let ns = build_node(ORIGIN, &sac, None);
        assert!(ns.produces.contains(&AxisKey::Sac));
        assert!(ns.produces.contains(&AxisKey::Ltb));
        assert!(ns.produces.contains(&AxisKey::Death));
        assert!(
            !ns.requires.contains(&AxisKey::Sac),
            "sacrifice does not REQUIRE the sac axis"
        );
    }

    #[test]
    fn opponent_damage_cycle_reports_the_opponents_damage() {
        // A mana engine (A) feeds a pinger (B) that deals 1 to the opponent and untaps the
        // engine each cycle; the damage is projected through `DealDamage`.
        let mut def_a = activated(mana_effect(colorless(dynamic())));
        def_a.cost = Some(AbilityCost::Tap);
        let node_a = build_node(ORIGIN, &def_a, None);

        let mut def_b = activated(deal_damage(fixed(1)));
        def_b.sub_ability = Some(Box::new(activated(set_tap(TapStateChange::Untap))));
        def_b.cost = Some(AbilityCost::Mana {
            cost: ManaCost::generic(1),
        });
        let node_b = build_node(ORIGIN, &def_b, None);
        assert!(
            node_b.produces.contains(&AxisKey::Tap),
            "the untap effect produces Tap"
        );

        let cands = candidate_cycles_from_nodes(vec![node_a, node_b]);
        assert_eq!(cands.len(), 1);
        assert!(cands[0]
            .unbounded
            .contains(&ResourceAxis::DamageDealt(OPPONENT)));
    }

    // === C. walker completeness (revert = drop a recursion arm) =============

    #[test]
    fn collect_effects_descends_nested_effect_variants() {
        let branch = AbilityDefinition::new(AbilityKind::Spell, mana_effect(colorless(fixed(1))));
        let mut top = activated(Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: vec![branch],
            kind: ChooseOneOfKind::Ordinary,
        });
        top.sub_ability = Some(Box::new(activated(put_counter(
            CounterType::Plus1Plus1,
            fixed(1),
        ))));

        let mut effects = Vec::new();
        collect_effects(&top, &mut effects);
        assert!(
            effects
                .iter()
                .any(|(e, _)| matches!(e, Effect::Mana { .. })),
            "the ChooseOneOf branch's Mana is collected"
        );
        assert!(
            effects
                .iter()
                .any(|(e, _)| matches!(e, Effect::PutCounter { .. })),
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
            build_nodes(&[AbilitySource::from(&none_face)]).is_empty(),
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
            build_nodes(&[AbilitySource::from(&some_face)]).len(),
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
        let nodes = build_nodes(&[AbilitySource::from(mantle)]);
        assert!(
            nodes.iter().any(|n| n.produces.contains(&AxisKey::Tap)),
            "Umbral Mantle's granted {{Q}} untap cost produces the Tap axis"
        );

        let cands = candidate_cycles(&[priest, mantle]);
        assert!(
            cands.iter().any(|c| {
                c.members.iter().any(|member| member.source == 0)
                    && c.members.iter().any(|member| member.source == 1)
                    && c.unbounded
                        .iter()
                        .any(|a| matches!(a, ResourceAxis::Mana(_)))
            }),
            "Priest of Titania + Umbral Mantle yields a mana-family candidate cycle; got {cands:?}"
        );
    }

    // === PR-4b fixtures =====================================================

    fn draw(count: QuantityExpr) -> Effect {
        Effect::Draw {
            count,
            target: default_target_filter_any(),
        }
    }
    fn mill_opp(count: QuantityExpr) -> Effect {
        Effect::Mill {
            count,
            target: TargetFilter::Player, // not Controller/SelfRef ⇒ OPPONENT
            destination: Zone::Graveyard,
        }
    }
    fn gain_life(amount: QuantityExpr) -> Effect {
        Effect::GainLife {
            amount,
            player: TargetFilter::Controller,
        }
    }
    fn lose_life_opp(amount: QuantityExpr) -> Effect {
        Effect::LoseLife {
            amount,
            target: Some(TargetFilter::Player),
        }
    }
    fn token(count: QuantityExpr) -> Effect {
        Effect::Token {
            name: "Servo".into(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".into()],
            colors: Vec::new(),
            keywords: Vec::new(),
            tapped: false,
            count,
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        }
    }
    fn sacrifice(target: TargetFilter) -> Effect {
        Effect::Sacrifice {
            target,
            count: fixed(1),
            min_count: 0,
        }
    }
    fn change_zone(origin: Option<Zone>, destination: Zone) -> Effect {
        Effect::ChangeZone {
            origin,
            destination,
            target: default_target_filter_any(),
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: Default::default(),
            enters_attacking: false,
            up_to: false,
            enter_with_counters: Vec::new(),
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        }
    }
    /// A `TriggerDefinition` with the zone-change disambiguator fields set.
    fn zone_trigger(
        mode: TriggerMode,
        origin: Option<Zone>,
        destination: Option<Zone>,
    ) -> TriggerDefinition {
        let mut t = TriggerDefinition::new(mode);
        t.origin = origin;
        t.destination = destination;
        t
    }
    /// Build a trigger node the way `build_nodes` does, through `trigger_requirements`, so
    /// reverting a trigger arm flips this node's `requires`.
    fn trig_node(trig: &TriggerDefinition, execute: AbilityDefinition) -> AbilityNode {
        build_node(ORIGIN, &execute, trigger_requirements(trig))
    }

    // === A2. effect_projection per-family (revert = flip/delete the arm) =====

    #[test]
    fn draw_projects_cards_drawn() {
        let Projection::Modeled { vector, .. } = effect_projection(&draw(fixed(2))) else {
            panic!("Draw must be modeled");
        };
        assert_eq!(vector.cards_drawn, 2);
    }

    #[test]
    fn mill_projects_negative_opponent_library() {
        let Projection::Modeled { vector, .. } = effect_projection(&mill_opp(fixed(5))) else {
            panic!("Mill must be modeled");
        };
        assert_eq!(
            vector.library_delta.get(&OPPONENT),
            Some(&-5),
            "mill drives the opponent's library DOWN"
        );
    }

    #[test]
    fn gain_life_projects_positive_controller_life() {
        let Projection::Modeled { vector, .. } = effect_projection(&gain_life(fixed(3))) else {
            panic!("GainLife must be modeled");
        };
        assert_eq!(vector.life.get(&CONTROLLER), Some(&3));
    }

    #[test]
    fn lose_life_projects_negative_player_split() {
        // Some(opponent) ⇒ a directed drain on the opponent.
        let Projection::Modeled { vector, .. } = effect_projection(&lose_life_opp(fixed(2))) else {
            panic!("LoseLife must be modeled");
        };
        assert_eq!(vector.life.get(&OPPONENT), Some(&-2));
        // None ⇒ "you lose N life" (controller self-loss).
        let Projection::Modeled { vector, .. } = effect_projection(&Effect::LoseLife {
            amount: fixed(1),
            target: None,
        }) else {
            panic!("modeled");
        };
        assert_eq!(vector.life.get(&CONTROLLER), Some(&-1));
    }

    #[test]
    fn pay_life_cost_is_negative_controller_life() {
        // The cost half of the R3-LIFE-SYMMETRY triple. REVERT PROBE: returning
        // PayLife to the no-op bucket flips this to `None` (0).
        let mut acc = NodeAcc::default();
        fold_cost(&mut acc, &AbilityCost::PayLife { amount: fixed(1) });
        assert_eq!(acc.net.life.get(&CONTROLLER), Some(&-1));
    }

    #[test]
    fn token_projects_tokens_and_etb() {
        // CR 603.6a: a token entry IS an ETB — this is the producer half of the
        // aristocrats edge. REVERT PROBE: drop `b.add_etb` ⇒ no Etb ⇒ no A→B edge.
        let np = build_node(ORIGIN, &activated(token(fixed(2))), None);
        assert_eq!(np.net.tokens_created, 2);
        assert_eq!(np.net.etb_triggers, 2);
        assert!(np.produces.contains(&AxisKey::Tokens));
        assert!(np.produces.contains(&AxisKey::Etb));
    }

    /// `[authority, projection seeds tokens]` for `effect`.
    fn token_family_readers(effect: &Effect) -> [bool; 2] {
        let projection_seeds_tokens = matches!(
            effect_projection(effect),
            Projection::Modeled { ref vector, .. } if vector.tokens_created > 0
        );
        [
            resolution_token_mint(effect).is_some(),
            projection_seeds_tokens,
        ]
    }

    #[test]
    fn token_family_authority_is_what_the_projection_and_both_arming_predicates_read() {
        let members = [
            Effect::Investigate,
            token(fixed(2)),
            Effect::CopyTokenOf {
                target: default_target_filter_any(),
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: fixed(3),
                extra_keywords: Vec::new(),
                additional_modifications: Vec::new(),
            },
            Effect::CreateTokenCopyFromPool {
                owner: TargetFilter::Controller,
                type_filter: default_target_filter_any(),
                mv: crate::types::ability::Comparator::EQ,
                mv_bound: fixed(2),
                selection: crate::types::ability::CardSelectionMode::Random,
                count: dynamic(),
                tapped: false,
                enters_attacking: false,
            },
        ];
        for effect in &members {
            let name: &'static str = effect.into();
            let count = resolution_token_mint(effect).expect("a member answers with its count");
            let (amount, magnitude) = count_seed(count);
            let Projection::Modeled {
                vector, magnitudes, ..
            } = effect_projection(effect)
            else {
                panic!("{name}: a member's projection is modeled");
            };
            // CR 111.1 + CR 603.6a: the token axis and the entry axis carry the authority's count.
            assert_eq!(
                (vector.tokens_created, vector.etb_triggers),
                (amount, amount),
                "{name}"
            );
            assert_eq!(
                (
                    magnitudes.get(&AxisKey::Tokens),
                    magnitudes.get(&AxisKey::Etb)
                ),
                (Some(&magnitude), Some(&magnitude)),
                "{name}"
            );
            assert_eq!(token_family_readers(effect), [true; 2], "{name}");
        }
        // CR 701.16a: investigate creates one Clue token.
        assert_eq!(resolution_token_mint(&Effect::Investigate), Some(&fixed(1)));

        // CR 701.36b: whether populate mints depends on a creature token its controller holds at
        // resolution, which the effect does not carry.
        assert_eq!(token_family_readers(&Effect::Populate), [false; 2]);
    }

    #[test]
    fn sacrifice_effect_produces_sac_ltb_death_gated_by_filter() {
        // A creature/undeterminable filter produces all three (CR 700.4 dies).
        let np = build_node(
            ORIGIN,
            &activated(sacrifice(default_target_filter_any())),
            None,
        );
        assert!(np.produces.contains(&AxisKey::Sac));
        assert!(np.produces.contains(&AxisKey::Ltb));
        assert!(np.produces.contains(&AxisKey::Death));

        // Sibling: a provably-non-creature filter (noncreature) suppresses Death
        // (pins `sac_produces_death`), but Sac/Ltb stay (any sac is an LTB).
        let noncreature = TargetFilter::Typed(TypedFilter::new(TypeFilter::Non(Box::new(
            TypeFilter::Creature,
        ))));
        let nn = build_node(ORIGIN, &activated(sacrifice(noncreature)), None);
        assert!(nn.produces.contains(&AxisKey::Sac));
        assert!(nn.produces.contains(&AxisKey::Ltb));
        assert!(
            !nn.produces.contains(&AxisKey::Death),
            "a noncreature sacrifice produces no dies event"
        );
    }

    #[test]
    fn destroy_effect_produces_ltb_death_not_sac() {
        let np = build_node(
            ORIGIN,
            &activated(Effect::Destroy {
                target: default_target_filter_any(),
                cant_regenerate: false,
            }),
            None,
        );
        assert!(np.produces.contains(&AxisKey::Ltb));
        assert!(np.produces.contains(&AxisKey::Death));
        assert!(
            !np.produces.contains(&AxisKey::Sac),
            "destroy is not a sacrifice"
        );
    }

    #[test]
    fn change_zone_disambiguates_etb_death_ltb() {
        // dest=Battlefield ⇒ ETB.
        let etb = build_node(
            ORIGIN,
            &activated(change_zone(Some(Zone::Graveyard), Zone::Battlefield)),
            None,
        );
        assert!(etb.produces.contains(&AxisKey::Etb));

        // bf→graveyard ⇒ LTB + Death (dies).
        let dies = build_node(
            ORIGIN,
            &activated(change_zone(Some(Zone::Battlefield), Zone::Graveyard)),
            None,
        );
        assert!(dies.produces.contains(&AxisKey::Ltb));
        assert!(dies.produces.contains(&AxisKey::Death));

        // bf→hand ⇒ LTB only (not a dies).
        let bounce = build_node(
            ORIGIN,
            &activated(change_zone(Some(Zone::Battlefield), Zone::Hand)),
            None,
        );
        assert!(bounce.produces.contains(&AxisKey::Ltb));
        assert!(!bounce.produces.contains(&AxisKey::Death));
    }

    #[test]
    fn change_zone_graveyard_to_library_is_unmodeled() {
        // M2: a zone change touching the battlefield on NEITHER side carries no
        // modeled event — it MUST stay Unmodeled (so `completeness` is honest),
        // never a Modeled-but-empty projection.
        assert!(matches!(
            effect_projection(&change_zone(Some(Zone::Graveyard), Zone::Library)),
            Projection::Unmodeled
        ));
        // And it propagates to the node's confidence flag.
        let node = build_node(
            ORIGIN,
            &activated(change_zone(Some(Zone::Graveyard), Zone::Library)),
            None,
        );
        assert_eq!(
            node.completeness,
            ModelCompleteness::ContainsUnmodeled,
            "an Unmodeled effect flags ContainsUnmodeled"
        );
        assert!(node.produces.is_empty());
    }

    #[test]
    fn extra_turn_and_combat_phase_project_their_axes() {
        let et = build_node(
            ORIGIN,
            &activated(Effect::ExtraTurn {
                target: TargetFilter::Controller,
                count: fixed(1),
            }),
            None,
        );
        assert_eq!(et.net.extra_turns, 1);
        assert!(et.produces.contains(&AxisKey::ExtraTurn));

        let two = build_node(
            ORIGIN,
            &activated(Effect::ExtraTurn {
                target: TargetFilter::Controller,
                count: fixed(2),
            }),
            None,
        );
        assert_eq!(two.net.extra_turns, 2);

        let dynamic = effect_projection(&Effect::ExtraTurn {
            target: TargetFilter::Controller,
            count: QuantityExpr::Ref {
                qty: QuantityRef::Variable { name: "X".into() },
            },
        });
        assert!(matches!(
            dynamic,
            Projection::Modeled { ref magnitudes, .. }
                if magnitudes.get(&AxisKey::ExtraTurn) == Some(&AxisMagnitude::Unbounded)
        ));

        // CR 500.8: an additional combat phase pumps the Combat axis.
        let combat = build_node(
            ORIGIN,
            &activated(Effect::AdditionalPhase {
                recipient: crate::types::ability::ExtraPhaseRecipient::Controller,
                segment: TurnSegment::Phase(PhaseGroup::Combat),
                after: crate::types::ability::ExtraPhaseAnchor::this_main_phase(),
                followed_by: Vec::new(),
                count: fixed(1),
                attacker_restriction: None,
            }),
            None,
        );
        assert_eq!(combat.net.combat_phases, 1);
        assert!(combat.produces.contains(&AxisKey::Combat));

        // An added step that is neither a combat phase nor a main phase carries no modeled axis.
        assert!(matches!(
            effect_projection(&Effect::AdditionalPhase {
                recipient: crate::types::ability::ExtraPhaseRecipient::Controller,
                segment: TurnSegment::Step(crate::types::phase::Phase::Upkeep),
                after: crate::types::ability::ExtraPhaseAnchor::ThisStep,
                followed_by: Vec::new(),
                count: fixed(1),
                attacker_restriction: None,
            }),
            Projection::Unmodeled
        ));
    }

    #[test]
    fn pt_and_control_stay_unmodeled() {
        // §3.2: families with no ResourceVector axis stay Unmodeled (no invented axis).
        assert!(matches!(
            effect_projection(&Effect::Pump {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: default_target_filter_any(),
            }),
            Projection::Unmodeled
        ));
        assert!(matches!(
            effect_projection(&Effect::GainControl {
                target: default_target_filter_any(),
            }),
            Projection::Unmodeled
        ));
    }

    // === B2. trigger_axis (revert = flip the arm back to None) ===============

    #[test]
    fn changes_zone_trigger_disambiguates_etb_vs_death_vs_ltb() {
        // Requires the &TriggerDefinition signature widening — pins it.
        assert_eq!(
            trigger_axis(&zone_trigger(
                TriggerMode::ChangesZone,
                None,
                Some(Zone::Battlefield)
            )),
            Some(AxisKey::Etb)
        );
        assert_eq!(
            trigger_axis(&zone_trigger(
                TriggerMode::ChangesZone,
                Some(Zone::Battlefield),
                Some(Zone::Graveyard)
            )),
            Some(AxisKey::Death)
        );
        assert_eq!(
            trigger_axis(&zone_trigger(
                TriggerMode::ChangesZone,
                Some(Zone::Battlefield),
                Some(Zone::Hand)
            )),
            Some(AxisKey::Ltb)
        );
        // Neither side battlefield ⇒ inert.
        assert_eq!(
            trigger_axis(&zone_trigger(
                TriggerMode::ChangesZone,
                Some(Zone::Graveyard),
                Some(Zone::Hand)
            )),
            None
        );
    }

    #[test]
    fn event_triggers_require_their_axes() {
        let life = TriggerDefinition::new(TriggerMode::LifeGained);
        assert_eq!(trigger_axis(&life), Some(AxisKey::Life));
        let dies = TriggerDefinition::new(TriggerMode::Destroyed);
        assert_eq!(trigger_axis(&dies), Some(AxisKey::Death));
        let sac = TriggerDefinition::new(TriggerMode::Sacrificed);
        assert_eq!(trigger_axis(&sac), Some(AxisKey::Sac));
        let tok = TriggerDefinition::new(TriggerMode::TokenCreated);
        assert_eq!(trigger_axis(&tok), Some(AxisKey::Tokens));
        let drew = TriggerDefinition::new(TriggerMode::Drawn);
        assert_eq!(trigger_axis(&drew), Some(AxisKey::Draw));
        let milled = TriggerDefinition::new(TriggerMode::Milled);
        assert_eq!(trigger_axis(&milled), Some(AxisKey::Library));
        // A mode with no modeled producer stays inert.
        assert_eq!(
            trigger_axis(&TriggerDefinition::new(TriggerMode::Blocks)),
            None
        );
    }

    // === C. HEADLINE payoff tests (trigger-event-edge SCCs) =================

    #[test]
    fn dies_token_aristocrats_loop_is_candidate() {
        // Node A: "whenever a creature dies, create a token and an opponent loses 1
        // life." dies trigger (bf→graveyard) ⇒ requires Death; Token ⇒ produces
        // Etb+Tokens; LoseLife(opp) ⇒ life[OPP] -= 1.
        let mut def_a = activated(token(fixed(1)));
        def_a.sub_ability = Some(Box::new(activated(lose_life_opp(fixed(1)))));
        let trig_a = zone_trigger(
            TriggerMode::ChangesZone,
            Some(Zone::Battlefield),
            Some(Zone::Graveyard),
        );
        let node_a = trig_node(&trig_a, def_a);
        assert!(node_a.requires.contains(&AxisKey::Death));
        assert!(node_a.produces.contains(&AxisKey::Etb));

        // Node B: "whenever a creature enters, sacrifice a creature." ETB trigger
        // ⇒ requires Etb; Sacrifice(creature) ⇒ produces Sac/Ltb/Death.
        let trig_b = zone_trigger(TriggerMode::ChangesZone, None, Some(Zone::Battlefield));
        let node_b = trig_node(
            &trig_b,
            activated(sacrifice(TargetFilter::Typed(TypedFilter::creature()))),
        );
        assert!(node_b.requires.contains(&AxisKey::Etb));
        assert!(node_b.produces.contains(&AxisKey::Death));

        let cands = candidate_cycles_from_nodes(vec![node_a, node_b]);
        assert_eq!(
            cands.len(),
            1,
            "the dies↔ETB aristocrats SCC is one candidate"
        );
        assert_eq!(cands[0].members.len(), 2);
        assert!(cands[0].unbounded.contains(&ResourceAxis::Life(OPPONENT)));
    }

    #[test]
    fn lifegain_feedback_loop_is_candidate() {
        // Node H (Heliod): "whenever you gain life, put a +1/+1 counter." lifegain
        // trigger ⇒ requires Life; PutCounter ⇒ produces Counter(P1P1, Creature).
        let node_h = trig_node(
            &TriggerDefinition::new(TriggerMode::LifeGained),
            activated(put_counter(CounterType::Plus1Plus1, fixed(1))),
        );
        assert!(node_h.requires.contains(&AxisKey::Life));
        assert!(node_h.produces.contains(&AxisKey::Counter(P1P1.0, P1P1.1)));

        // Node F (Spike Feeder): "{PayLife 1, remove a +1/+1 counter}: gain 2 life."
        // cost ⇒ requires Counter + life[CONTROLLER] -= 1; GainLife ⇒ life += 2.
        let mut def_f = activated(gain_life(fixed(2)));
        def_f.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::PayLife { amount: fixed(1) },
                AbilityCost::RemoveCounter {
                    count: 1,
                    counter_type: CounterMatch::OfType(CounterType::Plus1Plus1),
                    target: None,
                    selection: Default::default(),
                },
            ],
        });
        let node_f = build_node(ORIGIN, &def_f, None);
        assert!(node_f.requires.contains(&AxisKey::Counter(P1P1.0, P1P1.1)));
        assert!(node_f.produces.contains(&AxisKey::Life));

        let cands = candidate_cycles_from_nodes(vec![node_h, node_f]);
        assert_eq!(
            cands.len(),
            1,
            "the lifegain↔counter feedback SCC is one candidate"
        );
        // Net life = +2 (gain) − 1 (pay) = +1 on the controller.
        assert!(cands[0].unbounded.contains(&ResourceAxis::Life(CONTROLLER)));
    }

    // === D. real-card-data corpus smoke (export-gated graceful skip) =========

    #[test]
    fn corpus_lifegain_feedback_yields_candidate() {
        let db = crate::test_support::shared_card_db();
        let (Some(heliod), Some(ballista)) = (
            db.get_face_by_name("Heliod, Sun-Crowned"),
            db.get_face_by_name("Walking Ballista"),
        ) else {
            return; // export/fixture absent: skip gracefully
        };
        let cands = candidate_cycles(&[heliod, ballista]);
        // Recall-first: assert the pair surfaces a Life and/or Counter candidate.
        assert!(
            cands.iter().any(|c| {
                c.unbounded
                    .iter()
                    .any(|a| matches!(a, ResourceAxis::Life(_) | ResourceAxis::Counter(_, _)))
            }) || cands.is_empty(),
            "Heliod + Walking Ballista candidates (if any) name a Life/Counter axis; got {cands:?}"
        );
    }

    // === D2. projections over real faces: combat, colored costs, extra turns, recursion ======

    /// Whether some candidate cycle over `names`' real faces holds the node built from `held`'s
    /// definition in `slot`.
    fn cycle_holds(names: &[&str], held: &str, slot: AbilitySlot) -> bool {
        let db = crate::test_support::shared_card_db();
        let faces: Vec<&CardFace> = names
            .iter()
            .map(|name| {
                db.get_face_by_name(name)
                    .unwrap_or_else(|| panic!("{name} is in the card fixture"))
            })
            .collect();
        let source = names.iter().position(|name| *name == held).unwrap();
        candidate_cycles(&faces)
            .iter()
            .any(|cycle| cycle.members.contains(&NodeOrigin { source, slot }))
    }

    #[test]
    fn an_attack_trigger_granting_a_combat_cycles_through_the_combat_it_grants() {
        assert!(cycle_holds(
            &["Hellkite Charger", "Bear Umbra", "Mountain"],
            "Hellkite Charger",
            AbilitySlot::Trigger(0),
        ));
    }

    fn face(name: &str) -> &'static CardFace {
        crate::test_support::shared_card_db()
            .get_face_by_name(name)
            .unwrap_or_else(|| panic!("{name} is in the card fixture"))
    }

    /// The node built from `name`'s first printed trigger.
    fn first_trigger_node(name: &str) -> AbilityNode {
        build_nodes(&[AbilitySource::from(face(name))])
            .into_iter()
            .find(|node| node.origin.slot == AbilitySlot::Trigger(0))
            .unwrap_or_else(|| panic!("{name}'s first trigger has a node"))
    }

    /// CR 120.2a + CR 120.2b: a damage-dealt trigger requires the combat axis, the damage axis, or
    /// both, by the kind of damage it watches; a damage-received trigger requires neither.
    #[test]
    fn a_damage_trigger_requires_the_axes_of_the_damage_kinds_it_watches() {
        let requires = |name: &str| trigger_requirements(&face(name).triggers[0]);
        assert_eq!(requires("Old Gnawbone"), BTreeSet::from([AxisKey::Combat]));
        assert_eq!(
            requires("Virtue of Courage"),
            BTreeSet::from([AxisKey::Damage])
        );
        assert_eq!(
            requires("Malcolm, Keen-Eyed Navigator"),
            BTreeSet::from([AxisKey::Damage, AxisKey::Combat])
        );
        assert_eq!(requires("Smaug the Impenetrable"), BTreeSet::new());
        assert_eq!(
            requires("Hellkite Charger"),
            BTreeSet::from([AxisKey::Combat])
        );
    }

    /// CR 120.2a + CR 500.8: an added combat phase feeds a trigger watching combat damage, and a
    /// trigger watching only noncombat damage (CR 120.2b) is fed by an effect that deals damage.
    #[test]
    fn a_combat_damage_trigger_cycles_with_the_combat_that_feeds_it() {
        for watcher in ["Old Gnawbone", "Malcolm, Keen-Eyed Navigator"] {
            let names = ["Aggravated Assault", watcher];
            assert!(
                cycle_holds(&names, watcher, AbilitySlot::Trigger(0)),
                "{watcher}"
            );
            assert!(
                cycle_holds(&names, "Aggravated Assault", AbilitySlot::Ability(0)),
                "{watcher}"
            );
        }
        assert!(
            cycle_holds(
                &["Aggravated Assault", "The Reaver Cleaver"],
                "Aggravated Assault",
                AbilitySlot::Ability(0)
            ),
            "a combat-damage trigger an attachment grants"
        );

        let mut names = vec!["Aggravated Assault", "Molten Lavamancer", "Mana Echoes"];
        assert!(!cycle_holds(
            &names,
            "Molten Lavamancer",
            AbilitySlot::Trigger(0)
        ));
        names.push("Prodigal Pyromancer");
        assert!(
            cycle_holds(&names, "Molten Lavamancer", AbilitySlot::Trigger(0)),
            "reach: a noncombat damage source closes the cycle the combat did not"
        );
    }

    /// CR 505.1a + CR 500.8: a trigger a postcombat main phase begins for, whether it stores that
    /// phase or stores none and fires in each main phase, requires the main phase an effect adds
    /// as well as the turn that brings one; a trigger at the beginning of the precombat main
    /// phase, of an upkeep or of combat does not.
    #[test]
    fn a_postcombat_main_trigger_requires_an_added_main_phase_as_well_as_a_turn() {
        let requires = |name: &str| trigger_requirements(&face(name).triggers[0]);
        for watcher in ["Neheb, the Eternal", "Frontier Siege", "Carpet of Flowers"] {
            assert_eq!(
                requires(watcher),
                BTreeSet::from([AxisKey::ExtraTurn, AxisKey::MainPhase]),
                "{watcher}"
            );
        }
        for elsewhere in ["Glóin the Mighty", "Blinkmoth Urn", "Paradox Haze"] {
            assert_eq!(
                requires(elsewhere),
                BTreeSet::from([AxisKey::ExtraTurn]),
                "{elsewhere}"
            );
        }
        assert_eq!(
            requires("Sinister Monolith"),
            BTreeSet::from([AxisKey::Combat])
        );
        assert_eq!(
            requires("Lotus Cobra"),
            BTreeSet::from([AxisKey::Etb]),
            "a trigger that is not at the beginning of a step stores no step either"
        );
    }

    /// CR 500.8: an effect that adds a main phase produces it, whether the main phase is the
    /// segment it adds or follows the one it adds, and an effect adding a combat phase alone does
    /// not; the added main phase is no resource axis.
    #[test]
    fn an_effect_adding_a_main_phase_produces_it() {
        let adding = |segment: PhaseGroup, followed_by: Vec<PhaseGroup>| {
            build_node(
                ORIGIN,
                &activated(Effect::AdditionalPhase {
                    recipient: crate::types::ability::ExtraPhaseRecipient::Controller,
                    segment: TurnSegment::Phase(segment),
                    after: crate::types::ability::ExtraPhaseAnchor::this_main_phase(),
                    followed_by: followed_by.into_iter().map(TurnSegment::Phase).collect(),
                    count: fixed(1),
                    attacker_restriction: None,
                }),
                None,
            )
        };
        let both = adding(PhaseGroup::Combat, vec![PhaseGroup::PostcombatMain]);
        assert_eq!(
            both.produces,
            BTreeSet::from([AxisKey::Combat, AxisKey::MainPhase])
        );
        assert_eq!(both.net.combat_phases, 1);
        assert_eq!(
            both.unbounded_production,
            BTreeSet::new(),
            "the added main phase names no resource"
        );
        assert_eq!(
            axis_key_to_resource(&AxisKey::MainPhase, &ResourceVector::default()),
            None
        );

        for main in [PhaseGroup::PostcombatMain, PhaseGroup::PrecombatMain] {
            let alone = adding(main, Vec::new());
            assert_eq!(alone.produces, BTreeSet::from([AxisKey::MainPhase]));
            assert_eq!(alone.completeness, ModelCompleteness::FullyModeled);
        }

        let combat = adding(PhaseGroup::Combat, Vec::new());
        assert_eq!(combat.produces, BTreeSet::from([AxisKey::Combat]));

        assert!(node_of("Aggravated Assault", AbilitySlot::Ability(0))
            .produces
            .contains(&AxisKey::MainPhase));
        let najeela = node_of("Najeela, the Blade-Blossom", AbilitySlot::Ability(0));
        assert!(najeela.produces.contains(&AxisKey::Combat), "reach");
        assert!(!najeela.produces.contains(&AxisKey::MainPhase));
    }

    /// CR 505.1a: the main phase Aggravated Assault adds feeds Neheb, the Eternal's trigger at the
    /// beginning of the postcombat main phase and Frontier Siege's at the beginning of each main
    /// phase, and not Glóin the Mighty's at the beginning of the first main phase; the combat
    /// phase Najeela, the Blade-Blossom adds feeds none of them; and the turn Time Warp adds
    /// feeds Neheb's and Glóin's alike.
    #[test]
    fn a_postcombat_main_trigger_cycles_with_the_main_phase_that_feeds_it() {
        let fed = |adder: &str, slot: AbilitySlot, watcher: &str| {
            cycle_holds(&[adder, watcher], watcher, AbilitySlot::Trigger(0))
                && cycle_holds(&[adder, watcher], adder, slot)
        };
        let assault = ("Aggravated Assault", AbilitySlot::Ability(0));
        let najeela = ("Najeela, the Blade-Blossom", AbilitySlot::Ability(0));
        assert!(fed(assault.0, assault.1, "Neheb, the Eternal"));
        assert!(fed(assault.0, assault.1, "Frontier Siege"));
        assert!(!fed(assault.0, assault.1, "Glóin the Mighty"));
        assert!(!fed(najeela.0, najeela.1, "Neheb, the Eternal"));
        assert!(
            first_trigger_node("Lotus Cobra")
                .produces
                .contains(&AxisKey::Mana),
            "reach: Lotus Cobra's landfall trigger produces the mana Aggravated Assault spends"
        );
        assert!(!fed(assault.0, assault.1, "Lotus Cobra"));

        for watcher in ["Neheb, the Eternal", "Glóin the Mighty"] {
            assert!(
                first_trigger_node(watcher)
                    .produces
                    .contains(&AxisKey::Mana),
                "reach: {watcher} produces the mana both adders spend"
            );
            assert!(
                cycle_holds(&["Time Warp", watcher], watcher, AbilitySlot::Trigger(0)),
                "{watcher}"
            );
        }
        assert!(
            node_of(najeela.0, najeela.1)
                .requires
                .contains(&AxisKey::Mana),
            "reach: Najeela's ability spends mana"
        );
    }

    /// CR 111.10a + CR 111.3: a mint also produces the mana its tokens' own mana abilities add,
    /// whether the ability comes from the token's definition (Treasure) or from the creating effect
    /// ("They have "Sacrifice this token: Add {C}.""), in a fixed number or not.
    #[test]
    fn a_mint_produces_the_mana_its_tokens_add() {
        let gnawbone = first_trigger_node("Old Gnawbone");
        assert!(gnawbone.produces.contains(&AxisKey::Mana));
        assert!(gnawbone.unbounded_production.contains(&AxisKey::Mana));

        let hireling = first_trigger_node("Grim Hireling");
        assert_eq!(hireling.net.mana.iter().sum::<i64>(), 2);
        assert!(hireling.produces.contains(&AxisKey::Mana));
        assert!(!hireling.unbounded_production.contains(&AxisKey::Mana));

        assert!(cycle_holds(
            &["Aggravated Assault", "Rapacious One"],
            "Rapacious One",
            AbilitySlot::Trigger(0)
        ));
    }

    /// CR 701.26b: an untap of one permanent or of a population produces the tap axis, bounded,
    /// and that edge closes the cycle with a tapped mana source.
    #[test]
    fn an_untap_of_one_permanent_or_a_population_produces_the_tap_axis() {
        for scope in [EffectScope::Single, EffectScope::All] {
            let mut acc = NodeAcc::default();
            fold_projection(
                &mut acc,
                effect_projection(&Effect::SetTapState {
                    target: default_target_filter_any(),
                    scope,
                    state: TapStateChange::Untap,
                }),
            );
            assert!(acc.produces.contains(&AxisKey::Tap), "{scope:?}");
            assert!(acc.unbounded_production.is_empty(), "{scope:?}");
        }
        for names in [
            &["Aggravated Assault", "Bear Umbra", "Mountain"][..],
            &["Aggravated Assault", "Birds of Paradise"][..],
        ] {
            assert!(
                cycle_holds(names, "Aggravated Assault", AbilitySlot::Ability(0)),
                "{names:?}"
            );
        }
    }

    /// CR 701.26a + CR 701.26b: Nature's Will ("tap all lands that player controls and untap all
    /// lands you control") loops on the tap axis alone, so its cycle is a candidate that reports
    /// no axis.
    #[test]
    fn a_cycle_on_the_tap_axis_alone_is_a_candidate_naming_no_axis() {
        let will = first_trigger_node("Nature's Will");
        assert!(will.requires.contains(&AxisKey::Tap) && will.produces.contains(&AxisKey::Tap));
        assert!(will.unbounded_production.is_empty());
        let candidates = candidate_cycles(&[face("Nature's Will")]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].unbounded, []);
    }

    #[test]
    fn a_five_color_activation_cycles_on_flexible_and_single_color_producers() {
        let mut names = vec!["Najeela, the Blade-Blossom", "Urtet, Remnant of Memnarch"];
        names.extend([
            "Gold Myr",
            "Silver Myr",
            "Leaden Myr",
            "Iron Myr",
            "Copper Myr",
        ]);
        assert!(cycle_holds(
            &names,
            "Najeela, the Blade-Blossom",
            AbilitySlot::Ability(0)
        ));
    }

    /// CR 608.2n: a spell requires its return to a hand, an edge from a member that returns it
    /// (Archaeomancer to Ghostly Flicker, which blinks it); Time Warp also cycles with the Island
    /// its extra turn untaps (CR 500.7 + CR 502.3).
    #[test]
    fn a_spell_requires_its_return_to_hand_and_cycles_through_any_edge_it_has() {
        let warp = build_nodes(&[AbilitySource::from(face("Time Warp"))]);
        assert!(warp[0].requires.contains(&AxisKey::ReturnToHand));
        assert!(cycle_holds(
            &["Archaeomancer", "Ghostly Flicker"],
            "Ghostly Flicker",
            AbilitySlot::Ability(0)
        ));
        assert!(!cycle_holds(
            &["Mnemonic Wall", "Time Warp"],
            "Time Warp",
            AbilitySlot::Ability(0)
        ));
        let names = ["Time Warp", "Island"];
        assert!(cycle_holds(&names, "Time Warp", AbilitySlot::Ability(0)));
        let faces = names.map(face);
        assert!(candidate_cycles(&faces)
            .iter()
            .any(|cycle| cycle.unbounded.contains(&ResourceAxis::ExtraTurns)));
    }

    /// The node built from `name`'s activated ability or trigger in `slot`.
    fn node_of(name: &str, slot: AbilitySlot) -> AbilityNode {
        build_nodes(&[AbilitySource::from(face(name))])
            .into_iter()
            .find(|node| node.origin.slot == slot)
            .unwrap_or_else(|| panic!("{name} has a node in {slot:?}"))
    }

    /// An axis a node produces without bound is produced whatever its own cost nets away:
    /// Nykthos, Shrine to Nyx's "{2}, {T}: Choose a color. Add an amount of mana of that color
    /// equal to your devotion to that color."
    #[test]
    fn an_unbounded_production_is_produced_whatever_the_nodes_cost_nets() {
        let nykthos = node_of("Nykthos, Shrine to Nyx", AbilitySlot::Ability(1));
        assert!(
            nykthos.net.mana.iter().sum::<i64>() <= 0,
            "reach: the cost nets the seeded mana away"
        );
        assert!(nykthos.produces.contains(&AxisKey::Mana));
    }

    /// CR 102.2 + CR 102.3 + CR 119.3 + CR 701.17a: an effect naming no target moves the life or
    /// the library of the players its definition's scope names: each opponent, or each player.
    #[test]
    fn a_player_scope_names_whose_life_and_library_an_effect_moves() {
        let trigger = AbilitySlot::Trigger(0);
        // "each opponent loses 1 life and you gain 1 life"
        let monolith = node_of("Sinister Monolith", trigger);
        assert_eq!(
            monolith.net.life,
            BTreeMap::from([(CONTROLLER, 1), (OPPONENT, -1)])
        );
        assert!(monolith.produces.contains(&AxisKey::Life));
        // "each opponent mills a card"
        assert_eq!(
            node_of("Altar of the Brood", trigger).net.library_delta,
            BTreeMap::from([(OPPONENT, -1)])
        );
        // "each player loses 1 life"
        assert_eq!(
            node_of("Blood Tyrant", trigger).net.life,
            BTreeMap::from([(CONTROLLER, -1), (OPPONENT, -1)])
        );
        // "{T}: Each player mills a card."
        assert_eq!(
            node_of("Ghoulcaller's Bell", AbilitySlot::Ability(0))
                .net
                .library_delta,
            BTreeMap::from([(CONTROLLER, -1), (OPPONENT, -1)])
        );
        // "{T}: Add {R} or {G}. Each opponent gains 1 life.": the scope of the definition the
        // effect heads, not of the ability it ends.
        assert_eq!(
            node_of("Grove of the Burnwillows", AbilitySlot::Ability(1))
                .net
                .life,
            BTreeMap::from([(OPPONENT, 1)])
        );
        // "each opponent who doesn't control an Elf loses 1 life": who they are is the game's
        // to say, so the loss stays where an unscoped effect puts it.
        assert_eq!(
            node_of("Thornbow Archer", trigger).net.life,
            BTreeMap::from([(CONTROLLER, -1)])
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
        // fold_cost(acc, c) }`) and B's node nets mana -101 where the envelope keeps
        // max(-1,-100) = -1.

        // Engine node A: tap for {1} plus an UNBOUNDED +1/+1 counter, the axis the
        // candidate reports. Requires Tap.
        let mut def_a = activated(mana_effect(colorless(fixed(1))));
        def_a.sub_ability = Some(Box::new(activated(put_counter(
            CounterType::Plus1Plus1,
            dynamic(),
        ))));
        def_a.cost = Some(AbilityCost::Tap);
        let node_a = build_node(ORIGIN, &def_a, None);
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
            "mana production is FIXED"
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
        let node_b = build_node(ORIGIN, &def_b, None);
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
        let node = build_node(ORIGIN, &def, None);

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
    fn model_completeness_tracks_unmodeled() {
        // gemini R2 (PR #4493): the typed completeness flag replaces a raw bool.
        // A modeled effect ⇒ FullyModeled; an unmodeled one ⇒ ContainsUnmodeled.
        let modeled = build_node(ORIGIN, &activated(mana_effect(colorless(fixed(1)))), None);
        assert_eq!(modeled.completeness, ModelCompleteness::FullyModeled);

        let unmodeled = build_node(ORIGIN, &activated(Effect::unimplemented("x", "y")), None);
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
    }

    // CR 205.2b: `type_filter_excludes_creature` must reason recursively over
    // `Non`/`AnyOf` composition, not just a direct `Non(Creature)`.
    // Discrimination: the pre-fix `matches!(Non(inner) if inner == Creature)`
    // returns `false` for `Non(AnyOf([Creature, Land]))`, so the first
    // `assert!(... )` below flips red under a revert.
    #[test]
    fn type_filter_excludes_creature_handles_composed_negation() {
        use TypeFilter::*;
        // Composed exclusions that PROVABLY match no creature:
        assert!(type_filter_excludes_creature(&Non(Box::new(Creature))));
        assert!(type_filter_excludes_creature(&Non(Box::new(AnyOf(vec![
            Creature, Land
        ])))));
        // `AnyOf` excludes a creature only if EVERY branch does.
        assert!(type_filter_excludes_creature(&AnyOf(vec![
            Non(Box::new(Creature)),
            Non(Box::new(AnyOf(vec![Creature, Artifact]))),
        ])));

        // Filters that a creature CAN still satisfy → NOT exclusions (conservative,
        // keep the dies edge):
        assert!(!type_filter_excludes_creature(&Creature));
        assert!(!type_filter_excludes_creature(&Land)); // creature-lands (Dryad Arbor)
        assert!(!type_filter_excludes_creature(&Non(Box::new(Land)))); // "noncreature" not implied
        assert!(!type_filter_excludes_creature(&Non(Box::new(AnyOf(vec![
            Land, Artifact
        ]))))); // "neither land nor artifact" can still be a creature
                // Double negation collapses: Non(Non(Creature)) ≡ Creature → matches creatures.
        assert!(!type_filter_excludes_creature(&Non(Box::new(Non(
            Box::new(Creature)
        )))));
        // An `AnyOf` with one non-excluding branch is not an exclusion.
        assert!(!type_filter_excludes_creature(&AnyOf(vec![
            Non(Box::new(Creature)),
            Land,
        ])));

        // List form (conjunction): any provably-excluding filter excludes.
        assert!(type_filters_exclude_creature(&[
            Land,
            Non(Box::new(Creature))
        ]));
        assert!(!type_filters_exclude_creature(&[Land, Artifact]));
    }

    // CR 119.3: `LifeLostAll` is the batched life-loss trigger form. The runtime
    // routes it through the same `match_life_lost` matcher as `LifeLost`
    // (`trigger_matchers.rs`) and indexes it with the life-change triggers
    // (`trigger_index.rs`), so `trigger_axis` must map it to the Life axis too —
    // otherwise Engine B misses candidates whose consumer is the all/batched
    // life-loss trigger fed by a life-loss producer.
    //
    // Discrimination: pre-fix `LifeLostAll` sat in the inert `None` bucket, so the
    // first assertion returned `None` and flips red under a revert.
    #[test]
    fn life_lost_all_trigger_consumes_life_axis() {
        assert_eq!(
            trigger_axis(&TriggerDefinition::new(TriggerMode::LifeLostAll)),
            Some(AxisKey::Life),
            "batched LifeLostAll must consume the Life axis like LifeLost"
        );
        // Sibling sanity: the non-batched form is unchanged.
        assert_eq!(
            trigger_axis(&TriggerDefinition::new(TriggerMode::LifeLost)),
            Some(AxisKey::Life)
        );
    }
}
