//! `ResourceVector`: the monotone resource axes a net-progress loop can pump,
//! plus the resource-projected loop equality that distinguishes a beneficial
//! (CR 732.2) loop from a mandatory-draw (CR 104.4b / CR 732.4) loop.
//!
//! # Why a *separate* comparison from `loop_states_equal`
//!
//! CR 104.4b: a loop of *mandatory* actions that repeats a sequence "with no way
//! to stop" is a draw. The engine's existing `loop_states_equal` answers exactly
//! that question: it treats two states as the same loop point only when life,
//! damage, counters, and mana also match — because a mandatory loop that keeps
//! changing those values is not truly repeating and is *not* a draw.
//!
//! CR 732.2a: a player may instead take a *shortcut* through a loop "that repeats
//! a specified number of times". This is how a *beneficial* loop terminates: it
//! makes net progress on some resource each cycle (deal 1 more damage, add 1 more
//! mana, mill 1 more card), so the board returns to an identical configuration
//! while a resource counter strictly increases. Detecting that requires the
//! **complement** of `loop_states_equal`: board/zones/tap-state identical, but the
//! monotone resources allowed to differ.
//!
//! [`ResourceVector`] is the typed catalogue of those monotone axes;
//! [`loop_states_equal_modulo_resources`] is the projected comparison.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::analysis::decision_template::DecisionSlot;
use crate::game::game_object::GameObject;
use crate::types::ability::{ActivationRestriction, DamageModification};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::CounterType;
use crate::types::game_state::{loop_states_equal, GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::{CardId, ObjectId, TriggerFiring};
use crate::types::mana::ManaType;
use crate::types::phase::Phase;
use crate::types::player::{Player, PlayerId};
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// CR 732.2a: the metered spend the resolution probe charges against.
///
/// A NESTED module, not a new file, and that nesting is the enforcement: Rust
/// privacy is module-scoped, so a budget defined beside its consumers could be
/// re-constructed by any same-module code and its spend would be invisible to
/// the meter. Defining it one module down lets the constructor be narrowed
/// without also narrowing it away from this module's own wrappers.
mod verdict_memo {
    use std::collections::BTreeMap;

    use crate::game::engine::{CapAuthority, EntryPinSlots};
    use crate::game::resolution_prompt::ResolutionChoiceFreedom;
    use crate::types::game_state::{GameState, StackEntry};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    /// The shipped probe cap, per classification run.
    ///
    /// RE-DERIVED FROM MEASUREMENT. The retired `12` was fitted to per-mint charge
    /// counts (dellian 2–4 / F4 1) taken over the CURRENT STACK only, before the
    /// verdict door existed. Over the door's derived `(frame, entry)` population the
    /// announcement gate mints keys too, and the measured demand at the corpus's one
    /// offering beat — dina `beat=19`, `ring=3 stack=10`, driven through `apply()` —
    /// is **13 charges** (`asks=13`, one charge per key), i.e. `12` starved the
    /// acceptance offer by exactly one.
    ///
    /// `26` is `2 ×` that measured demand: an offering beat may carry double the
    /// observed key population (≈ a 20-entry stack at the measured 1.3 keys/entry)
    /// and still certify. The ceiling is not arbitrary either — it stays far below
    /// the unexempted sweep the frozen exemption exists to prevent, whose demand
    /// measures **96–107** on dellian's `ring=16 stack≈176` beats, so an unexempted
    /// full-stack classification still exhausts and still refuses fail-closed.
    pub(crate) const PROBE_BUDGET: u32 = 26;

    /// CR 732.2a: cost is a COVERAGE knob, never a soundness knob — an
    /// unaffordable probe degrades to honest-red (no certificate, no offer),
    /// never to a wrong certificate and never to an unbounded stall.
    ///
    /// **Derive list pinned to exactly `#[derive(Debug)]`** — no `Default`, no
    /// `Clone`/`Copy`. A derived constructor recompiles the construction escape
    /// with `new` untouched: the derived value is a zero-cap always-denying
    /// budget, which is fail-closed but meter-invisible and relief-disabling.
    #[derive(Debug)]
    pub(crate) struct ProbeBudget {
        remaining: u32,
        /// Latched by [`ProbeBudget::try_charge_one`] so an exhaustion can be
        /// ATTRIBUTED rather than inferred from a zero remainder. Read in
        /// production by [`PeriodVerdicts::denied`], which the metered mint
        /// carries out in its `MintMeter`.
        denied: bool,
    }

    impl ProbeBudget {
        /// MODULE-PRIVATE. Every budget is born inside this module, so the
        /// container that owns the meter is the only thing that can start a
        /// spend — a fresh budget compiled beside a consumer would be a spend
        /// the mint's meter never sees. Re-adding a
        /// `ProbeBudget::new(PROBE_BUDGET)` call in `analysis::resource` is
        /// E0624, which is the closure stated as a compile fact.
        fn new(cap: u32) -> Self {
            Self {
                remaining: cap,
                denied: false,
            }
        }

        /// `false` ⇒ exhausted, and the exhaustion fact is latched so it can be
        /// attributed rather than inferred from a zero remainder.
        pub(crate) fn try_charge_one(&mut self) -> bool {
            if self.remaining == 0 {
                self.denied = true;
                return false;
            }
            self.remaining -= 1;
            true
        }

        /// Did any charge get denied against this budget?
        pub(crate) fn denied(&self) -> bool {
            self.denied
        }

        /// TEST-ONLY constructor. `new` is module-private so no site outside
        /// this module can mint a budget whose spend would be invisible to a
        /// meter; a `cfg(test)` door cannot widen that, because it does not
        /// exist in a production build.
        #[cfg(test)]
        pub(crate) fn for_test(cap: u32) -> Self {
            Self::new(cap)
        }
    }

    /// A frame's position in ONE container's OWN `frames` table.
    ///
    /// The field is private and there is no public constructor and no
    /// `From<usize>`: a `FrameIx` can only be minted by
    /// [`PeriodVerdicts::frame_ix`], which resolves a `&GameState` by POINTER
    /// IDENTITY against the very table [`PeriodVerdicts::verdict`] indexes. No
    /// index arithmetic exists anywhere on the mint path, so the
    /// window-relative off-by-`idx` class is unconstructible rather than merely
    /// unobserved — forging `FrameIx(3)` one module out is E0603.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub(crate) struct FrameIx(usize);

    /// Everything ONE `(frame, entry)` pair answers, computed on first demand by
    /// [`PeriodVerdicts::verdict`] and memoized for the rest of the mint.
    ///
    /// Every field is produced by a function whose signature takes NO published
    /// slots, which is what removes the key-completeness obligation: a slot list
    /// is a *window* fact and would make the memo key incomplete, so a fourth
    /// field that reads one is a design error rather than an addition.
    pub(crate) struct EntryVerdict {
        /// THE MINT, CACHED — `entry_publishes_pin_slots(frame, entry,
        /// proposer)` with the CONTAINER's bound proposer. A proposer-less
        /// (`unproven`) container computes `None` for every key: nothing is
        /// published when no offer binds a proposer, which is the mint's own
        /// answer rather than an invented one.
        pub(crate) published: Option<EntryPinSlots>,
        /// `stack_entry_resolution_choice_freedom(frame, entry, budget)` on the
        /// ability AS IT STANDS.
        pub(crate) primary: ResolutionChoiceFreedom,
        /// CR 603.5: the optional-cleared re-classification the relief used to
        /// compute inline. `None` for an entry whose mint publishes no `may`
        /// slot, and for one whose resolution scope cannot bind (CR 603.4).
        pub(crate) residual: Option<ResolutionChoiceFreedom>,
    }

    /// CR 732.2a: the ONE per-mint verdict door — a TOTAL, `(FrameIx,
    /// ObjectId)`-keyed, compute-on-miss memo that OWNS the probe budget.
    ///
    /// Totality is scoped honestly: [`PeriodVerdicts::verdict`] is total over
    /// every `FrameIx` this container minted, and MINTING
    /// ([`PeriodVerdicts::frame_ix`]) is the membership question — it returns
    /// `Option` and every consumer treats `None` as a refusal, so a frame
    /// outside the period costs a certificate, never a wrong one.
    pub(crate) struct PeriodVerdicts<'a> {
        /// `ring ++ [current]` — PRIVATE and not indexable, so a `FrameIx`
        /// cannot be turned back into a board by the parent module.
        frames: Vec<&'a GameState>,
        memo: BTreeMap<(FrameIx, ObjectId), EntryVerdict>,
        /// PRIVATE: the field route to a fresh spend is E0616 from the parent
        /// module, and the construction route is shut by `new`'s module
        /// privacy above.
        budget: ProbeBudget,
        /// The cap this container was born at, so `spent()` is a difference
        /// rather than a second counter that could drift from the budget.
        cap: u32,
        /// The THIRD argument of the memoized mint, fixed at construction, so
        /// the EFFECTIVE key is `(proposer, FrameIx, ObjectId)` with the first
        /// component constant per container.
        proposer: Option<PlayerId>,
        conjunct6_asks: u32,
        conjunct6_frozen_skips: u32,
        conjunct4_scans: u32,
    }

    impl<'a> PeriodVerdicts<'a> {
        fn build(
            ring: &[&'a GameState],
            current: &'a GameState,
            proposer: Option<PlayerId>,
            cap: u32,
        ) -> Self {
            let mut frames: Vec<&'a GameState> = ring.to_vec();
            frames.push(current);
            Self {
                frames,
                memo: BTreeMap::new(),
                budget: ProbeBudget::new(cap),
                cap,
                proposer,
                conjunct6_asks: 0,
                conjunct6_frozen_skips: 0,
                conjunct4_scans: 0,
            }
        }

        /// The default-cap constructor, bound to the mint's own proposer.
        ///
        /// `pub(super)` and not `pub(crate)`: every in-scope construction site
        /// lives in `analysis::resource` or a descendant, while an out-of-file
        /// fresh container — whose spend the mint's meter would never see — is
        /// E0624. The OFFER path constructs through
        /// [`PeriodVerdicts::for_period_with_cap`] instead, because
        /// `verdict_memo` cannot mint the `CapAuthority` that door demands.
        ///
        /// Every U3 caller is a `#[cfg(test)]` site, so the plain lib target sees
        /// this as dead (U1's `denied()` precedent). Its production reader is the
        /// still-unwritten R22 row; keeping the gate `not(test)`-scoped means that
        /// row lands with no annotation churn.
        #[cfg_attr(not(test), allow(dead_code))]
        pub(super) fn for_period(
            ring: &[&'a GameState],
            current: &'a GameState,
            proposer: PlayerId,
        ) -> Self {
            Self::build(ring, current, Some(proposer), PROBE_BUDGET)
        }

        /// The cap-parameterised twin — the ONLY budget-raise/lower channel,
        /// and the reason it is safe is CAPABILITY rather than visibility: the
        /// final parameter is a token whose tuple constructor is private to
        /// `game::engine`, the metered seam's own module, so a fresh
        /// arbitrary-cap container built anywhere else is E0603.
        ///
        /// `for_period` is this function at `cap = PROBE_BUDGET`; both construct
        /// through the module-private `ProbeBudget::new`.
        pub(crate) fn for_period_with_cap(
            ring: &[&'a GameState],
            current: &'a GameState,
            proposer: PlayerId,
            cap: u32,
            _auth: CapAuthority,
        ) -> Self {
            Self::build(ring, current, Some(proposer), cap)
        }

        /// Every other path (including the unscoped sibling): frames =
        /// `[current]`, NO proposer ⇒ nothing published ⇒ no relief. That is
        /// byte-identical to the pre-change unproven behaviour, where relief
        /// died on `scope.pinned == None` before any mint call.
        pub(super) fn unproven(current: &'a GameState) -> Self {
            Self::build(&[], current, None, PROBE_BUDGET)
        }

        /// THE ONLY `FrameIx` MINT. Resolves a frame to its position in
        /// `self.frames` by `std::ptr::eq` — the same table `verdict` reads, so
        /// the returned index is correct BY IDENTITY, not by arithmetic.
        /// `rposition`, so the only conceivable duplicate (one pointer appearing
        /// twice) resolves newest; a duplicate pointer is the same state, hence
        /// the same verdict.
        ///
        /// `None` ⇒ the frame is not in this period ⇒ the CALLER refuses. That
        /// is what makes "the memo's last frame IS the caller's current"
        /// structural rather than a `debug_assert` compiled out of release.
        pub(crate) fn frame_ix(&self, frame: &GameState) -> Option<FrameIx> {
            self.frames
                .iter()
                .rposition(|f| std::ptr::eq(*f, frame))
                .map(FrameIx)
        }

        /// THE ONE DOOR. Computes on miss against `self.frames[f.0]` — the memo,
        /// never the caller, converts `FrameIx` back to a board — charges the
        /// OWNED budget, and memoizes. Total over minted keys: it returns a
        /// value, never an `Option`, so there is no miss contract to get wrong.
        pub(crate) fn verdict(&mut self, f: FrameIx, entry: &StackEntry) -> &EntryVerdict {
            let key = (f, entry.id);
            if !self.memo.contains_key(&key) {
                let frame = self.frames[f.0];
                let published = self
                    .proposer
                    .and_then(|p| crate::game::engine::entry_publishes_pin_slots(frame, entry, p));
                let primary =
                    super::stack_entry_resolution_choice_freedom(frame, entry, &mut self.budget);
                // TWO bases need the optional-cleared re-classification, and they are
                // MUTUALLY EXCLUSIVE by construction: `entry_publishes_pin_slots` guard (b)
                // withholds the `may` slot exactly when a stored auto-choice already answers
                // the CR 603.5 gate, so an auto-answered entry publishes NOTHING and would
                // otherwise carry `residual: None` — the structural reason gate (6) could not
                // relieve it. Computing the residual on the auto basis too is what makes
                // `auto_may_choice_relief` able to read the same memo the pin relief reads.
                let residual = if published.as_ref().and_then(|p| p.may.as_ref()).is_some()
                    || matches!(
                        super::auto_may_answer_for(frame, entry),
                        Some(crate::types::game_state::AutoMayChoice::Accept)
                    ) {
                    super::optional_cleared_classification(frame, entry, &mut self.budget)
                } else {
                    None
                };
                self.memo.insert(
                    key,
                    EntryVerdict {
                        published,
                        primary,
                        residual,
                    },
                );
            }
            &self.memo[&key]
        }

        /// Read-only: the bound proposer, for the relief-side agreement guard.
        pub(crate) fn proposer(&self) -> Option<PlayerId> {
            self.proposer
        }

        /// THE METER: charges taken against this container's one budget.
        pub(crate) fn spent(&self) -> u32 {
            self.cap - self.budget.remaining
        }

        /// At least one charge was REFUSED — the exhaustion fact, attributed
        /// rather than inferred from a zero remainder.
        pub(crate) fn denied(&self) -> bool {
            self.budget.denied()
        }

        /// Item (6) loop bodies that reached the verdict door.
        pub(crate) fn conjunct6_asks(&self) -> u32 {
            self.conjunct6_asks
        }
        /// Frozen `continue`s taken in item (6) — the exemption, counted.
        pub(crate) fn conjunct6_frozen_skips(&self) -> u32 {
            self.conjunct6_frozen_skips
        }
        /// Calls of `stack_entry_reads_projected_resource` from item (4)'s
        /// `.any()`, which is the only body change item (4) takes.
        pub(crate) fn conjunct4_scans(&self) -> u32 {
            self.conjunct4_scans
        }
        pub(crate) fn note_conjunct6_ask(&mut self) {
            self.conjunct6_asks += 1;
        }
        pub(crate) fn note_conjunct6_frozen_skip(&mut self) {
            self.conjunct6_frozen_skips += 1;
        }
        pub(crate) fn note_conjunct4_scan(&mut self) {
            self.conjunct4_scans += 1;
        }
    }
}

pub(crate) use verdict_memo::{FrameIx, PeriodVerdicts, ProbeBudget, PROBE_BUDGET};

/// WUBRG + colorless, the canonical index order used by [`ResourceVector::mana`].
///
/// Matches `ManaColor::ALL` (WUBRG) with colorless appended, so index `i` of the
/// mana array is `MANA_INDEX[i]`.
const MANA_INDEX: [ManaType; 6] = [
    ManaType::White,
    ManaType::Blue,
    ManaType::Black,
    ManaType::Red,
    ManaType::Green,
    ManaType::Colorless,
];

/// CR 122.1: classification of the object/player a counter sits on, so a counter
/// axis is keyed by *what kind of thing accumulates it* (a +1/+1 loop on a
/// creature is a different unbounded resource than loyalty on a planeswalker).
///
/// Typed rather than stringly so the win-classifier can `match` exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ObjectClass {
    /// CR 302: a creature on the battlefield.
    Creature,
    /// CR 306: a planeswalker on the battlefield.
    Planeswalker,
    /// CR 310: a battle on the battlefield.
    Battle,
    /// CR 119 / CR 122: a player (poison, energy, experience, …).
    Player,
    /// Any other counter-bearing object (artifact, enchantment, land, …).
    Other,
}

/// CR 122.1: analysis-layer classification of a counter kind.
///
/// The engine's [`CounterType`] is intentionally **not** reused as a map key
/// here: it derives neither `Ord` (required for `BTreeMap` keys) nor a small
/// closed set — it carries `Generic(String)`, `Keyword(KeywordKind)`, and
/// parameterized `PowerToughness { .. }` variants. Adding `Ord` to that
/// crate-wide enum (and transitively to `KeywordKind`) to satisfy one analysis
/// map would be a far larger, non-additive change. Instead this module owns a
/// small `Ord` classification of the counter dimensions the corpus cares about
/// (CR 122.1: +1/+1, loyalty, poison, …) and folds the long tail into `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CounterClass {
    /// CR 122.1a: a +1/+1 counter.
    Plus1Plus1,
    /// CR 122.1a: a -1/-1 counter.
    Minus1Minus1,
    /// CR 306.5b: a loyalty counter on a planeswalker.
    Loyalty,
    /// CR 310.4c: a defense counter on a battle.
    Defense,
    /// CR 122.1 + CR 704.5c: a poison counter on a player (10 ⇒ that player loses).
    Poison,
    /// CR 122.1: an energy counter ({E}) in a player's energy reserve.
    Energy,
    /// Any other counter kind (charge, lore, time, keyword, generic, …).
    Other,
}

impl CounterClass {
    /// Map an engine [`CounterType`] to its analysis classification.
    pub(crate) fn from_counter_type(ct: &CounterType) -> CounterClass {
        match ct {
            CounterType::Plus1Plus1 => CounterClass::Plus1Plus1,
            CounterType::Minus1Minus1 => CounterClass::Minus1Minus1,
            CounterType::Loyalty => CounterClass::Loyalty,
            CounterType::Defense => CounterClass::Defense,
            _ => CounterClass::Other,
        }
    }
}

/// A non-counter, non-mana trigger/event family whose firings a loop can pump
/// without changing the board (the canonical example is proliferate, but also
/// magecraft, constellation, etc.). Typed rather than stringly.
///
/// CR 701.x keyword-action and CR 603.x triggered-ability families. These counts
/// are **not** directly readable from a `GameState` snapshot — they are events,
/// not stored totals — so [`ResourceVector::snapshot`] always leaves
/// [`ResourceVector::generic_triggers`] empty and the simulation harness (PR-1)
/// feeds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TriggerKind {
    /// CR 701.34: proliferate (the keyword action a loop can pump mana-neutrally).
    Proliferate,
    /// CR 207.2c + CR 603: magecraft — an ability word (no individual CR entry)
    /// for a triggered ability that fires on casting/copying an instant or sorcery.
    Magecraft,
    /// CR 207.2c + CR 603: constellation — an ability word for a triggered
    /// ability that fires when an enchantment enters under your control.
    Constellation,
    /// CR 207.2c + CR 603: landfall — an ability word for a triggered ability
    /// that fires when a land enters under your control.
    Landfall,
    /// Any other tracked trigger/keyword-action family.
    Other,
}

/// A vector of the **monotone** resources an infinite loop can pump.
///
/// "Monotone" = a beneficial loop only ever drives these in one direction within
/// a cycle (it gains mana/life/damage/tokens/triggers; a *consumed* axis like
/// mana or life may also be spent, which is why net-progress is tested as a
/// *delta* over a full cycle, not per step).
///
/// # Two population sources
///
/// 1. **State-readable** (filled by [`ResourceVector::snapshot`]): absolute
///    levels the engine stores directly — floating mana, per-player life,
///    library sizes, and counters on objects/players.
/// 2. **Event-fed** (left zero by `snapshot`, populated externally by the PR-1
///    harness): counts of events the engine does not retain as a running total
///    readable from a single `GameState` — damage dealt, tokens created, cards
///    drawn, casts, and trigger firings. Each such field is documented below.
///
/// Compare two snapshots with [`ResourceVector::delta`] to get the per-cycle
/// change; [`ResourceVector::is_net_progress`] then decides whether the cycle is
/// a beneficial (CR 732.2) loop.
///
/// `Serialize`/`Deserialize` exist because a per-cycle delta rides
/// [`PeriodicDelta`] on the `WaitingFor::LoopShortcut` /
/// `WaitingFor::RespondToShortcut` wire. Every map whose key is NOT string-like
/// needs an adaptor to get there — see [`map_key_pairs`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceVector {
    /// CR 106.1: floating mana by color, indexed `[W, U, B, R, G, C]` (see
    /// [`MANA_INDEX`]). Summed across all players' pools. **State-readable.**
    pub mana: [i64; 6],

    /// CR 119.1: per-player life total. **State-readable.**
    #[serde(with = "map_key_pairs")]
    pub life: BTreeMap<PlayerId, i64>,

    /// CR 120.1: cumulative damage *dealt to* each player this analysis window.
    /// Damage is an event, not a stored total. **Event-fed** (left empty by
    /// `snapshot`).
    #[serde(with = "map_key_pairs")]
    pub damage_dealt: BTreeMap<PlayerId, i64>,

    /// CR 401: per-player library size, as a signed delta-friendly count.
    /// Positive = larger library. Mill loops drive this negative.
    /// **State-readable** (absolute library size at snapshot time).
    #[serde(with = "map_key_pairs")]
    pub library_delta: BTreeMap<PlayerId, i64>,

    /// CR 122.1 + CR 704.5c: poison counters keyed by VICTIM `PlayerId` (10 ⇒ that
    /// player loses). Per-victim so a multiplayer poison ∞ attributes the loss to the
    /// afflicted seat, not the loop's controller. **State-readable.**
    #[serde(with = "map_key_pairs")]
    pub poison: BTreeMap<PlayerId, i64>,

    /// CR 111: tokens created this analysis window. **Event-fed.**
    pub tokens_created: i64,

    /// CR 121: cards drawn this analysis window. **Event-fed.**
    pub cards_drawn: i64,

    /// CR 601: spells cast this analysis window (storm / cast-count loops).
    /// **Event-fed.**
    pub casts_this_step: i64,

    /// CR 207.2c + CR 603: landfall triggers this window (landfall is an ability
    /// word for a land-enters triggered ability). **Event-fed.**
    pub landfall_triggers: i64,

    /// CR 500.8 + CR 506.1: extra combat phases CREATED this turn (begin-combat
    /// phases entered as extras plus those still queued in `state.extra_phases`).
    /// **State-readable** — computed by `snapshot` from the per-turn combat tally
    /// and queued extra phases.
    pub combat_phases: i64,

    /// CR 500.7: extra turns created this window, fed from the
    /// `EffectResolved{ExtraTurn}` creation event (not natural `TurnStarted`).
    /// **Event-fed.** NOTE: the scheduled "take an extra turn after this one"
    /// turn-control path (`turns.rs` `grant_extra_turn_after`) pushes onto
    /// `state.extra_turns` WITHOUT emitting `EffectResolved{ExtraTurn}`, so that
    /// less-common class is not counted on this axis — an honest coverage gap, not
    /// a regression.
    pub extra_turns: i64,

    /// CR 700.4 + CR 603.6c: "dies" (leaves-the-battlefield-to-graveyard)
    /// triggers this window. **Event-fed.**
    pub death_triggers: i64,
    /// CR 603.6a: enters-the-battlefield triggers this window. **Event-fed.**
    pub etb_triggers: i64,
    /// CR 603.6c: leaves-the-battlefield triggers this window. **Event-fed.**
    pub ltb_triggers: i64,
    /// CR 701.21: sacrifice triggers this window. **Event-fed.**
    pub sac_triggers: i64,

    /// CR 122.1: counters by `(kind, object class)`. Includes +1/+1, loyalty,
    /// and poison (poison/energy are keyed under [`ObjectClass::Player`]).
    /// **State-readable.**
    #[serde(with = "map_key_pairs")]
    pub counters: BTreeMap<(CounterClass, ObjectClass), i64>,

    /// Generic trigger/keyword-action firings by family (proliferate, magecraft,
    /// …) — the mana-neutral axis a proliferate loop pumps. **Event-fed.**
    pub generic_triggers: BTreeMap<TriggerKind, i64>,
}

/// Serde adaptor for every [`ResourceVector`] map whose key is not string-like:
/// [`ResourceVector::counters`] (a `(CounterClass, ObjectClass)` TUPLE key) and the four
/// [`PlayerId`]-keyed maps. Ride the wire as a pair SEQUENCE — the shape
/// `ShortcutDecisionSchema.points` already uses — so there is no map key to encode.
///
/// Not reusing `types::game_state::tuple_key_map` (the repo's other adaptor, which
/// stringifies instead): it is monomorphic over `HashMap<(ObjectId, usize), u32>`.
///
/// ⚠ **THE `PlayerId` MAPS ARE NOT OPTIONAL, contrary to what this doc claimed before.**
/// The struck form read "the two sibling maps need no adaptor — `PlayerId` is a newtype
/// over an integer … which `serde_json` accepts as keys". That is true of `to_string` /
/// `from_str` and of `to_value` / `from_value` in ISOLATION, which is why the direct
/// `periodic_delta_survives_the_serde_json_wire` arm passed and gave false confidence —
/// but it is FALSE on the production path, for a reason that is about the ENCLOSING type,
/// not this one:
///
/// * `WaitingFor` is `#[serde(tag = "type", content = "data")]` (ADJACENTLY TAGGED), so
///   its payload is buffered through serde's private `Content` before being handed to the
///   variant. `Content` represents every map key as a STRING.
/// * `PlayerId` is `#[serde(transparent)]` over `u8`, so it asks for a `u8` and gets a
///   string ⇒ `invalid type: string "0", expected u8`.
/// * `PersistedGameState::deserialize` funnels EVERY persisted decode through
///   `serde_json::Value` and then `serde_json::from_value` — including the production
///   WASM restore at `crates/engine-wasm/src/lib.rs`'s `from_str::<PersistedGameState>`.
///   So a saved game whose `RespondToShortcut` proposal carried a populated `per_cycle`
///   would fail to restore.
///
/// MEASURED, all four combinations, on `serde_json` 1.0.149: bare `BTreeMap<PlayerId, i64>`
/// is `Ok` for `from_str`/`from_value` standalone and for `from_str` under an adjacently
/// tagged enum, and `Err("invalid type: string \"0\", expected u8")` for `from_value` under
/// one — the exact error text this repo had already recorded in
/// `tests/integration/loop_shortcut.rs`. With this adaptor all four are `Ok`.
///
/// [`ResourceVector::generic_triggers`] deliberately keeps its bare map: `TriggerKind` is a
/// unit-variant enum, so its key is genuinely a string and it was measured `Ok` through the
/// same adjacently-tagged `from_value` path that breaks `PlayerId`.
mod map_key_pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub(super) fn serialize<S, K, V>(map: &BTreeMap<K, V>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        K: Serialize + Ord,
        V: Serialize,
    {
        map.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        D: Deserializer<'de>,
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
    {
        Ok(Vec::<(K, V)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

/// CR 732.2a: the resource signature of ONE repetition of a certified loop — what the
/// offer publishes so a bounded drive can check that each committed cycle actually
/// conformed, and so the CR 704 count bound has a per-period magnitude to divide by.
///
/// The `Vec` victim term (rather than a `BTreeMap` keyed by [`DecisionSlot`]) is
/// deliberate: a struct map key hits exactly the `serde_json` restriction
/// [`map_key_pairs`] exists for, and the single consumer
/// ([`ResourceVector::elimination_bounds`]) collects at its call site.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodicDelta {
    /// How many RETAINED RING FRAMES one repetition spans. DERIVED on both certification
    /// bases — from the certifying prior's index in the ring for direct recurrence, and as
    /// `k` for a signature derived by [`ring_delta_signature`]. It is NOT "1 for direct
    /// recurrence": that was a hardcode until fix round 1, and it was measured wrong (the
    /// `interactive_3p_subset_lethal_does_not_crown` fixture's repetition spans TWO frames,
    /// a gain-life resolution then a lose-life one).
    ///
    /// CONSUMER: `game::engine::drive_one_shortcut_cycle` DELIMITS a committed cycle by this
    /// count. It has to, for the class [`ring_delta_signature`] certifies: that basis proves
    /// a periodic DELTA, not a recurring board, so the drive's board-recurrence predicates are
    /// false at every settle beat and `Fixed(n)`'s `n` would otherwise be structurally inert.
    /// The count is measured the same way it is minted, and that now takes TWO sites to
    /// state: `record_loop_detect_sample` is called from the settle sampler in
    /// `game::engine::pass_priority_once_with_pipeline` AND from the forced-window answer
    /// site in `game::engine::apply_action`. `drive_one_shortcut_cycle` covers both — it
    /// steps `pass_priority_once_with_pipeline` on its priority beats and answers every other
    /// prompt through `inject_pinned_answer`. Of its FOUR arms, three end in `apply_action`
    /// (`OrderTriggers`, `TriggerTargetSelection`, `OptionalEffectChoice`) and the fourth is
    /// `_ => Err(RecastAbort)`, which returns before any frame advance — as do the early `Err`
    /// exits inside the two template arms. So every path that returns `Ok` HAS dispatched
    /// `apply_action`, and every path that does not aborts the drive rather than advancing it
    /// uncounted.
    ///
    /// The drive advances `frames_this_cycle` in BOTH of ITS OWN arms — the active-player
    /// `Priority` arm and the `inject_pinned_answer` arm, not to be confused with the four
    /// above — each keyed on the ring's back allocation actually changing, so mint and measure
    /// stay one-to-one. Before the second site existed this doc claimed a single call site;
    /// that premise is dead, and the count would silently read a HALF period if only one of
    /// the drive's two arms counted.
    ///
    /// Named for its unit on purpose: `game::engine::shortcut_drive_period` maps a
    /// TEMPLATE to a repeat count, which is a different quantity in the same subsystem.
    pub frames_per_period: u32,
    /// The whole-game resource change across one repetition, measured from the very
    /// frame pair that certified it.
    pub delta: ResourceVector,
    /// CR 704.5a: per ANNOUNCED target slot, the life magnitude one repetition charges
    /// to whichever player that slot's declaration names. EMPTY for the untargeted class,
    /// where the victims are already visible in `delta.life`.
    ///
    /// ANNOUNCED, not PUBLISHED, and the distinction is load-bearing rather than pedantic:
    /// CR 732.2a withholds a decision point for an announcement the PROPOSER does not make
    /// (`game::engine::TargetAnnouncement::NotProposerChoice` — its own doc enumerates the
    /// three routes: a single legal assignment, a CR 601.2c `target_chooser` seated on another
    /// player, or a non-`Chosen` `TargetSelectionMode`), but CR 704.5a charges that victim all
    /// the same. A slot present here with no matching `ShortcutDecisionSchema` point is
    /// therefore CORRECT and expected, not a schema/certificate mismatch. Deriving this from
    /// the published points instead let the withhold silently raise the bound.
    pub victim_slot: Vec<(DecisionSlot, i64)>,
}

impl ResourceVector {
    /// Snapshot the **state-readable** resource levels directly out of a
    /// `GameState`: floating mana, per-player life, per-player library size, and
    /// counters on every object (battlefield) and player.
    ///
    /// Event-fed fields (damage, tokens, draws, casts, all `*_triggers`, and
    /// [`Self::generic_triggers`]) are left at their `Default` (zero/empty); the
    /// PR-1 harness feeds them from the event stream.
    pub fn snapshot(state: &GameState) -> ResourceVector {
        let mut v = ResourceVector::default();

        // CR 106.1: floating mana, summed across every player's pool.
        for player in &state.players {
            for (i, color) in MANA_INDEX.iter().enumerate() {
                v.mana[i] += player.mana_pool.count_color(*color) as i64;
            }
            // CR 119.1: per-player life.
            v.life.insert(player.id, player.life as i64);
            // CR 401: per-player library size.
            v.library_delta
                .insert(player.id, player.library.len() as i64);
            // CR 704.5c: poison counters, keyed by the VICTIM's `PlayerId` (10 ⇒ that
            // player loses) — mirrors the per-player `life`/`library_delta` maps above.
            v.poison.insert(player.id, player.poison_counters as i64);
            // CR 122.1: energy reserve.
            if player.energy > 0 {
                v.counters.insert(
                    (CounterClass::Energy, ObjectClass::Player),
                    player.energy as i64,
                );
            }
        }

        // CR 122.1: counters on battlefield objects, keyed by counter kind and
        // the bearer's object class.
        for id in &state.battlefield {
            let Some(object) = state.objects.get(id) else {
                continue;
            };
            let class = object_class(object.card_types.core_types.as_slice());
            for (ct, count) in &object.counters {
                let key = (CounterClass::from_counter_type(ct), class);
                *v.counters.entry(key).or_insert(0) += *count as i64;
            }
        }

        // CR 500.8 + CR 506.1 + CR 500.1: extra COMBAT phases created this turn.
        // CR 506.1 / CR 500.1: a turn has exactly one natural combat phase, so
        // `combat_phases_started_this_turn` (every begin-combat ENTERED this turn,
        // natural + extra) minus that one natural combat yields extra combats
        // already entered; the `Phase::BeginCombat` entries still queued in
        // `state.extra_phases` (CR 500.8) add extra combats created but not yet
        // entered. The two terms are disjoint — `advance_phase` removes an extra
        // phase from `state.extra_phases` before entering it — so a consumed extra
        // combat is counted by the first term, a pending one by the second, never
        // both. This is "extra combats created", monotone within the turn and
        // independent of consumption timing, so a self-sustaining extra-combat loop
        // does not net to zero. NOTE: `combat_phases_started_this_turn` is engine
        // bookkeeping that resets each turn (in `start_next_turn`), so across a turn
        // boundary this axis can read negative under `delta`; that is a benign
        // false-NEGATIVE for a `Gained` axis (CR 732.2a `is_net_progress` only vetoes
        // on negative `Consumed` axes), never a false-positive.
        let entered_extra_combats = state.combat_phases_started_this_turn.saturating_sub(1) as i64;
        let queued_extra_combats = state
            .extra_phases
            .iter()
            .filter(|extra_phase| extra_phase.phase == Phase::BeginCombat)
            .count() as i64;
        v.combat_phases = entered_extra_combats + queued_extra_combats;

        v
    }

    /// Component-wise `after - before`. For map-backed axes, missing keys are
    /// treated as `0`, and the result keeps any key present on either side.
    ///
    /// The result is the per-cycle change to feed [`Self::is_net_progress`].
    pub fn delta(before: &ResourceVector, after: &ResourceVector) -> ResourceVector {
        let mut mana = [0i64; 6];
        for (i, slot) in mana.iter_mut().enumerate() {
            *slot = after.mana[i] - before.mana[i];
        }
        ResourceVector {
            mana,
            life: map_delta(&before.life, &after.life),
            damage_dealt: map_delta(&before.damage_dealt, &after.damage_dealt),
            library_delta: map_delta(&before.library_delta, &after.library_delta),
            poison: map_delta(&before.poison, &after.poison),
            tokens_created: after.tokens_created - before.tokens_created,
            cards_drawn: after.cards_drawn - before.cards_drawn,
            casts_this_step: after.casts_this_step - before.casts_this_step,
            landfall_triggers: after.landfall_triggers - before.landfall_triggers,
            combat_phases: after.combat_phases - before.combat_phases,
            extra_turns: after.extra_turns - before.extra_turns,
            death_triggers: after.death_triggers - before.death_triggers,
            etb_triggers: after.etb_triggers - before.etb_triggers,
            ltb_triggers: after.ltb_triggers - before.ltb_triggers,
            sac_triggers: after.sac_triggers - before.sac_triggers,
            counters: map_delta(&before.counters, &after.counters),
            generic_triggers: map_delta(&before.generic_triggers, &after.generic_triggers),
        }
    }

    /// Iterate every scalar component of this vector as a signed value, paired
    /// with whether that axis is **consumed** (may legitimately be spent inside a
    /// beneficial loop, e.g. mana and life) — see [`Self::is_net_progress`].
    fn components(&self) -> impl Iterator<Item = (Component, i64)> + '_ {
        let mana = self
            .mana
            .iter()
            .map(|&n| (Component::Consumed, n))
            .collect::<Vec<_>>();
        let life = self.life.values().map(|&n| (Component::Consumed, n));
        let library = self.library_delta.values().map(|&n| (Component::Gained, n));
        let damage = self.damage_dealt.values().map(|&n| (Component::Gained, n));
        // CR 704.5c: poison is a Gained axis (monotone rising toward the 10-loss), so a
        // poison-pumping loop stays net-progress.
        let poison = self.poison.values().map(|&n| (Component::Gained, n));
        let counters = self.counters.values().map(|&n| (Component::Gained, n));
        let triggers = self
            .generic_triggers
            .values()
            .map(|&n| (Component::Gained, n));
        let scalars = [
            self.tokens_created,
            self.cards_drawn,
            self.casts_this_step,
            self.landfall_triggers,
            self.combat_phases,
            self.extra_turns,
            self.death_triggers,
            self.etb_triggers,
            self.ltb_triggers,
            self.sac_triggers,
        ]
        .map(|n| (Component::Gained, n));

        mana.into_iter()
            .chain(life)
            .chain(library)
            .chain(damage)
            .chain(poison)
            .chain(counters)
            .chain(triggers)
            .chain(scalars)
    }

    /// CR 732.2a: is this delta a **net-progress** cycle — the signature of a
    /// beneficial loop that should be shortcut rather than drawn?
    ///
    /// True iff:
    /// 1. at least one component strictly increased (the loop makes progress
    ///    each cycle), and
    /// 2. no **consumed** component (mana, life) is net-negative — a loop that
    ///    spends more mana/life than it makes is not sustainable and would stop
    ///    on its own (so it is not an infinite net-progress loop).
    ///
    /// `Gained` axes (damage, tokens, draws, counters, triggers, library) are
    /// allowed to be negative on a *given* axis (e.g. a mill loop drives
    /// `library_delta` negative — that is the win, not a violation); only the
    /// *consumed* axes constrain sustainability. A mill loop still satisfies (1)
    /// via some other axis (or via a negative library being the unbounded
    /// resource — callers read [`Self::unbounded_components`] for that).
    ///
    /// CR 121.4 + CR 704.5b: a *pure*-mill loop whose only changing axis is a
    /// negative `library_delta` also counts as net-progress here — emptying a
    /// library is the win even though no axis strictly increased.
    pub fn is_net_progress(&self) -> bool {
        let mut any_increase = false;
        for (component, value) in self.components() {
            match component {
                Component::Consumed if value < 0 => return false,
                _ => {}
            }
            if value > 0 {
                any_increase = true;
            }
        }
        // CR 121.4 + CR 704.5b: a pure-mill loop is net-progress even though its
        // only changing axis (`library_delta`) is *negative* — driving a library
        // toward empty is the win (the opponent loses on the next attempted draw,
        // a state-based action). Recognized consistently with `unbounded_components`,
        // which surfaces `library_delta` on either sign; positive library growth is
        // already counted by the generic `value > 0` clause above, so this clause is
        // strictly additive for the negative (mill) case.
        let mills = self.library_delta.values().any(|&n| n < 0);
        any_increase || mills
    }

    /// EVERY axis this delta moved, in either direction, as a [`ResourceAxis`] tag with its
    /// signed magnitude — the unfiltered fold [`Self::unbounded_components`] narrows.
    ///
    /// Named `axis_components` because [`Self::components`] is taken by a different fold over
    /// the same fields: that one yields the [`Component`] CONSUMED/GAINED classification with
    /// no axis identity, for [`Self::is_net_progress`].
    ///
    /// The distinction from that method is the SIGN, and it is the whole reason this exists:
    /// `unbounded_components` reports only what a loop *accrues*, so a drain loop's defining
    /// term — the victim's NEGATIVE `life` — is invisible through it. A consumer that has to
    /// state what a repetition COSTS, rather than what it gains, cannot be built on that
    /// method. The one such consumer today is `game::interaction`'s CR 732.2a shortcut
    /// preview, which states the finished magnitude of a declared repeat count and would
    /// otherwise show a lethal drain as producing nothing.
    ///
    /// Order is fixed (mana, life, damage, library, poison, counters, triggers, then the
    /// scalar axes) and every map is a `BTreeMap`, so the result is deterministic.
    pub fn axis_components(&self) -> Vec<(ResourceAxis, i64)> {
        let mut out = Vec::new();
        for (i, &n) in self.mana.iter().enumerate() {
            if n != 0 {
                out.push((ResourceAxis::Mana(MANA_INDEX[i]), n));
            }
        }
        for (pid, &n) in &self.life {
            if n != 0 {
                out.push((ResourceAxis::Life(*pid), n));
            }
        }
        for (pid, &n) in &self.damage_dealt {
            if n != 0 {
                out.push((ResourceAxis::DamageDealt(*pid), n));
            }
        }
        for (pid, &n) in &self.library_delta {
            if n != 0 {
                out.push((ResourceAxis::LibraryDelta(*pid), n));
            }
        }
        for (pid, &n) in &self.poison {
            if n != 0 {
                out.push((ResourceAxis::Poison(*pid), n));
            }
        }
        for (&key, &n) in &self.counters {
            if n != 0 {
                out.push((ResourceAxis::Counter(key.0, key.1), n));
            }
        }
        for (&kind, &n) in &self.generic_triggers {
            if n != 0 {
                out.push((ResourceAxis::Trigger(kind), n));
            }
        }
        for (axis, n) in [
            (ResourceAxis::TokensCreated, self.tokens_created),
            (ResourceAxis::CardsDrawn, self.cards_drawn),
            (ResourceAxis::Casts, self.casts_this_step),
            (ResourceAxis::LandfallTriggers, self.landfall_triggers),
            (ResourceAxis::CombatPhases, self.combat_phases),
            (ResourceAxis::ExtraTurns, self.extra_turns),
            (ResourceAxis::DeathTriggers, self.death_triggers),
            (ResourceAxis::EtbTriggers, self.etb_triggers),
            (ResourceAxis::LtbTriggers, self.ltb_triggers),
            (ResourceAxis::SacTriggers, self.sac_triggers),
        ] {
            if n != 0 {
                out.push((axis, n));
            }
        }
        out
    }

    /// The component axes that strictly increased over this delta — the
    /// candidate **unbounded** resources a `WinKind` classifier (PR-2) reads to
    /// name the loop's win condition. A mill axis surfaces here as a negative
    /// `library_delta`, so it is reported separately via its sign.
    ///
    /// Returns each increasing axis as a [`ResourceAxis`] tag with its signed
    /// magnitude.
    ///
    /// CR 401: the `LibraryDelta` exemption is what keeps a mill loop — unbounded
    /// *downward* on library size — in the result while every other axis is required to
    /// have risen.
    ///
    /// CR 704.5c: rising poison on a victim is an unbounded loss axis — and unlike mill it
    /// needs no exemption, because poison RISES toward the ten-counter loss, so `Poison(p)`
    /// is carried by the `n > 0` term itself. RELOCATED, not re-derived: this annotation sat
    /// above the poison arm of this method's own loop until the `axis_components` split moved
    /// that loop out, and it belongs beside the CR 401 term because the pair is what states
    /// WHICH loss axes survive the filter and why. Re-verified against
    /// `docs/MagicCompRules.txt`: "704.5c If a player has ten or more poison counters, that
    /// player loses the game."
    pub fn unbounded_components(&self) -> Vec<(ResourceAxis, i64)> {
        self.axis_components()
            .into_iter()
            .filter(|&(axis, n)| n > 0 || matches!(axis, ResourceAxis::LibraryDelta(_)))
            .collect()
    }

    /// CR 732.2a + CR 704.5a / CR 704.5c / CR 104.3c + CR 121.4: the largest number of
    /// times this per-period delta may legally be repeated in one shortcut proposal.
    ///
    /// # The convention, and why it stops STRICTLY SHORT
    ///
    /// `N` is the largest count such that after each of the `N` cycles **no living player
    /// has crossed a CR 704 loss threshold**. CR 732.2a forbids a shortcut that contains a
    /// conditional action and requires its ending point to be a place a player would
    /// receive priority; CR 704.3 checks state-based actions whenever a player would get
    /// priority, and a cycle contains several such points. A mid-sequence CR 704.5a death
    /// therefore makes the remaining declared choices unmakeable — CR 800.4a removes the
    /// seat — which is both a conditional action and an illegal proposal. So the bound is
    /// `headroom / magnitude` with headroom measured to *one short of* the threshold.
    ///
    /// | axis | threshold | headroom for a living `p` |
    /// |---|---|---|
    /// | life | CR 704.5a (0 or less life) | `life[p] - 1` |
    /// | poison | CR 704.5c (ten or more counters) | `9 - poison[p]` |
    /// | library | CR 104.3c + CR 121.4 (draw from empty) | `library[p].len()` |
    ///
    /// # Aggregation per DECLARABLE victim
    ///
    /// `declarable_victims` is the union of the ANNOUNCED target slots' legal player targets
    /// — EMPTY for the untargeted class. `slot_magnitude` is the per-period life loss the
    /// certificate attributed to each announced slot. A declaration may aim **every** slot
    /// at **one** opponent, so a declarable victim's life magnitude is the SUM over all
    /// slots; that is what makes an all-slots-on-one-seat declaration bounded by
    /// construction rather than by a cross-slot check in `validate_pins`.
    ///
    /// ANNOUNCED, NOT PUBLISHED, and the caller
    /// (`game::engine::bounded_cycle_charged_targets_for_window`) supplies it that way on
    /// purpose. CR 732.2a withholds a decision point when the announcement is FORCED — the
    /// player makes no choice — but CR 704.5a charges that victim regardless of who chose it.
    /// Feeding this the PUBLISHED point set instead dropped a forced victim into the `else`
    /// arm below and RAISED the bound; on a victim whose measured period nets a life GAIN it
    /// disarmed the life axis at `MAX_SHORTCUT_CYCLES` outright.
    ///
    /// PRECISELY WHAT IS IMPLEMENTED, and how it differs from the specified rule: this
    /// sums **every** positive `slot_magnitude` and charges that one total `S` to **every**
    /// member of `declarable_victims`. The specified rule is `S(p) = Σ over slots s with
    /// p ∈ s.legal_targets` — a per-victim sum. The two coincide exactly when every slot
    /// can reach every declarable victim, which is the only shape reachable today
    /// (`declarable_victims` arrives as the UNION of the slots' legal targets, and the
    /// per-slot sets are not passed in at all — the signature carries no per-slot target
    /// information, so the per-victim sum is not computable here). Where they differ —
    /// a slot that can only reach seat A, another that can only reach seat B — this
    /// charges A with A+B and B with A+B, i.e. it OVER-charges, which yields a SMALLER
    /// bound. Conservative, therefore safe, and deliberately so: this is the fail-closed
    /// approximation of the specified rule, not the rule itself. **No current test
    /// discriminates the two** (every case's slots share identical legal-target sets), so
    /// do not read the battery as evidence for the exact rule. Threading per-slot
    /// `legal_targets` in (replacing `slot_magnitude: &BTreeMap<DecisionSlot, i64>` with a
    /// per-slot `(legal_targets, magnitude)` pairing) is what turns this into the exact
    /// §4.2 rule; it would only ever RAISE the bound, so it cannot invalidate an offer
    /// this form already permitted.
    ///
    /// The observed per-period loss and the declared slot magnitude are combined
    /// ADDITIVELY, with the observed term floored at zero: `observed.max(0) + S`. Where the
    /// two measure the SAME drain — the ring observed the loss the slot causes — the sum
    /// DOUBLE-COUNTS and over-charges, returning a smaller bound than strictly necessary
    /// (measured: a one-slot drain on a 16-life seat yields **7**, where `max` yielded 15).
    /// **7 is the shipped value and it is right**: this signature cannot prove that the
    /// observed loss and the slot magnitude are the same drain, so the over-charge is a
    /// PRECISION cost, never unsoundness.
    ///
    /// # SOUNDNESS — unconditional, and what the clamp is for
    ///
    /// The `max` form this replaced was **CORRECT ONLY IF `L_unattributed(p) == 0`** for
    /// every declarable victim — only if every non-proposer loss in the measured period was
    /// attributable to a published slot. That premise is **DISCHARGED BY CONSTRUCTION**
    /// here: the sum no longer needs it. A victim carrying an untargeted drain of 1 **and**
    /// a re-aimable slot of magnitude 1 has a true per-period loss of **2**; `max` returned
    /// **1**, overstating the bound 2× and permitting an in-proposal elimination
    /// (CR 704.5a) inside a proposed shortcut — exactly the conditional action CR 732.2a
    /// forbids. `max` fails OPEN; this form fails CLOSED, which is this repo's convention.
    ///
    /// The **`.max(0)` clamp is load-bearing and not optional.** `observed_life_loss`
    /// negates `self.life`, a per-period NET delta, so its sign is UNCONSTRAINED: a victim
    /// who nets a life GAIN yields a negative value. Unclamped, `observed + S` can be `<= 0`,
    /// the `narrow` closure never fires (its guard is `magnitude > 0`), and the life axis is
    /// silently DISARMED at `MAX_SHORTCUT_CYCLES` — a fail-open in the change whose purpose
    /// is closing one. Clamped, a net gain contributes nothing and cannot credit against the
    /// slot magnitude either (CR 119.3: each gain and loss adjusts the total as it happens;
    /// the net says nothing about order).
    ///
    /// `declared_life_magnitude >= 0` is a **CONSTRUCTION** fact, not an assumption: its
    /// initializer filters `*m > 0` and sums, and the empty sum is `0`. With that, for
    /// `observed >= 0` the sum is `>= max(observed, S)`, and for `observed < 0` it equals
    /// `S == max(observed, S)` exactly — so this magnitude dominates the `max` form on EVERY
    /// input, and `narrow` is monotone non-increasing in its divisor. The bound can only
    /// SHRINK.
    ///
    /// `elimination_bounds_mixed_loss_charges_both_terms` (case (n), split out so its
    /// revert-probe is reachable) DISCRIMINATES: `1` under `max`, `0` here. It supersedes
    /// the earlier note that every
    /// case had `S == 0` or `L_unattributed == 0` and that the battery was therefore
    /// non-discriminating on this axis.
    ///
    /// Option (ii) — threading per-slot `(legal_targets, magnitude)` pairs — repairs `S(p)`
    /// only and supplies no attribution of *observed* loss to slots, so it remains the open
    /// PRECISION upgrade rather than a soundness prerequisite.
    ///
    /// The netting residual is a property of `self.life` being a per-period **net**
    /// `delta()` output, and is identical under either operator.
    ///
    /// TREE-SCOPED: the first production consumer lands in a successor branch. This bound is
    /// made fail-closed AHEAD of that consumer rather than in it, and **does not depend on
    /// that branch's producer guard**.
    ///
    /// # Uniform over EVERY living player, including the proposer
    ///
    /// There is deliberately no `p == proposer => unbounded` case: `net_progress_for` reads
    /// only the proposer's mana and life, so it is blind to the proposer's own poison and
    /// to intra-cycle life dips. A proposer who drains themselves is bounded here like
    /// anyone else. An ELIMINATED seat contributes no term at all (CR 800.4a — it is no
    /// longer in the game), so a corpse at 1 life cannot pin the bound to zero.
    ///
    /// # Per-cycle magnitude constancy is a PREMISE, not a proof
    ///
    /// The bound extrapolates one measured period. Do NOT add a monotone-magnitude
    /// conjunct to "fix" that — it would reject every 2-frame window. The backstops are
    /// conformance (a cycle whose magnitude changed stops committing) and the live
    /// elimination guard during the drive, never an extrapolated total.
    ///
    /// Clamped to `MAX_SHORTCUT_CYCLES`. A return of `0` means no legal repetition exists
    /// and the caller must not offer; callers require `N >= 1`.
    /// CR 704.5a: the per-period life loss ONE published pin slot may charge to whichever
    /// seat its declaration names — the `slot_magnitude` term
    /// [`ResourceVector::elimination_bounds`] divides the headroom by.
    ///
    /// **MAX over seats, not SUM, and not the observed spread.** A pin is a
    /// STATE-INDEPENDENT designation (CR 732.2a), so a declaration may aim *every*
    /// iteration of a slot at *one* seat. Charging what the observed — unpinned — iteration
    /// happened to spread around would UNDER-charge and overstate the bound, which fails
    /// OPEN. Charging the sum over seats is not a loss any single seat can suffer from one
    /// slot; it over-charges, which only SHRINKS the bound and is the fail-closed direction
    /// this repo takes when the two disagree.
    ///
    /// Life GAINS contribute nothing (`(-n).max(0)`), so a proposer gaining 5 while three
    /// opponents lose 1, 2 and 3 yields 3 — never 5, and never 6.
    ///
    /// Extracted from `game::engine::try_offer_bounded_cycle_shortcut` so the max-vs-sum fork
    /// has a callable seam. ⚠ THE NOTE THAT STOOD HERE — *"`victim_slot` is empty on every
    /// trajectory that offers today … no fixture reaches it"* — IS FALSIFIED, and is replaced
    /// rather than softened: once the answer-beat sampling site in `apply_action` announces
    /// the entries a FORCED pre-priority window puts on the stack, a CR 608.2b `Targets`
    /// declaration is announced like any other and `victim_slot` is NON-EMPTY on the F4
    /// boards. `worst_seat_life_loss_is_the_max_seat_never_the_sum` is therefore no longer the
    /// only discriminator: the real-dump rows re-derive this value through
    /// `elimination_bounds` (`r1_the_bounded_offer_fires_on_the_real_f4_dump`), and
    /// `b5f_the_declared_term_can_suppress_an_otherwise_legal_offer` measures it flipping a
    /// live offer to `NoNarrowedLegalCount`.
    pub(crate) fn worst_seat_life_loss(&self) -> i64 {
        self.life.values().map(|&n| (-n).max(0)).max().unwrap_or(0)
    }

    // The first production consumer is `game::engine::try_offer_bounded_cycle_shortcut`.
    pub(crate) fn elimination_bounds(
        &self,
        state: &GameState,
        declarable_victims: &[PlayerId],
        slot_magnitude: &BTreeMap<DecisionSlot, i64>,
    ) -> u32 {
        let cap = crate::game::engine::MAX_SHORTCUT_CYCLES as i64;
        // Every published slot is assumed reachable to every declarable victim, so ONE
        // total is charged to each of them (see "PRECISELY WHAT IS IMPLEMENTED" above:
        // the conservative, over-charging approximation of the per-victim sum).
        let declared_life_magnitude: i64 =
            slot_magnitude.values().copied().filter(|m| *m > 0).sum();

        let mut bound = cap;
        let mut narrow = |headroom: i64, magnitude: i64| {
            if magnitude > 0 {
                bound = bound.min(headroom.max(0) / magnitude);
            }
        };

        for p in &state.players {
            // CR 800.4a: an eliminated seat has left the game and constrains nothing.
            if p.is_eliminated {
                continue;
            }
            // CR 704.5a. A negative life delta is the per-period loss.
            let observed_life_loss = -self.life.get(&p.id).copied().unwrap_or(0);
            let life_magnitude = if declarable_victims.contains(&p.id) {
                // CR 704.5a (MagicCompRules.txt:5492) + CR 732.2a
                // (MagicCompRules.txt:6372). Combined
                // ADDITIVELY, with the OBSERVED term floored at zero. `max` is correct only
                // if `L_unattributed(p) == 0` — every non-proposer loss in the measured
                // period attributable to a published slot — and this signature carries no
                // per-slot victim attribution with which to discharge that premise. A
                // victim carrying an untargeted drain of 1 AND a re-aimable slot of
                // magnitude 1 loses 2 per period; `max` returns 1, overstating the bound
                // and permitting an in-proposal elimination — the conditional action
                // CR 732.2a forbids.
                //
                // TIGHT **given the information in this signature**: with `d` the slot
                // loss actually delivered to `p`, the worst case is `observed + (S - d)`
                // for `0 <= d <= S`, whose supremum over the unattributable `d` is
                // `observed + S`.
                //
                // WHY `.max(0)`, AND WHY IT IS NOT OPTIONAL. `observed_life_loss` negates
                // `self.life`, a per-period NET delta (`ResourceVector::life`, produced by
                // `ResourceVector::delta` via `map_delta`), so its
                // sign is UNCONSTRAINED: a victim who nets a life GAIN yields a negative
                // value. Unclamped, `observed + S` can then be <= 0, the `narrow` closure
                // never fires (its guard is `magnitude > 0`), and the life axis is silently
                // DISARMED at MAX_SHORTCUT_CYCLES. Clamped, a net gain contributes nothing
                // and cannot credit against the slot magnitude either (CR 119.3,
                // MagicCompRules.txt:1065: each gain and loss adjusts the total as it
                // happens; the net says nothing about order).
                //
                // FAIL-CLOSED OVER THE WHOLE DOMAIN, not merely where both terms are
                // positive. `declared_life_magnitude` is `>= 0` by construction — its
                // initializer filters `*m > 0` and sums, and the empty sum is 0. For
                // `observed >= 0`, `observed + S >= max(observed, S)`; for `observed < 0`
                // it equals `S == max(observed, S)` exactly. So this magnitude is >= the
                // `max` form on EVERY input, and `narrow` is monotone non-increasing in its
                // divisor (non-negative numerator), so the returned bound can only SHRINK.
                //
                // Where `observed` and `S` measure the SAME drain this DOUBLE-COUNTS and
                // over-charges (precision loss, never unsoundness) — case (m) in
                // `elimination_bounds_conventions` is that shape, 15 -> 7. Accepted: it
                // errs toward refusal, and this repo's convention is fail-closed. The
                // precision upgrade is per-slot `(legal_targets, magnitude)` attribution.
                //
                // NOT BOUNDED BY THIS OPERATOR, stated plainly: intra-cycle dips. A period
                // that drains 5 and lifelinks 7 reports `observed = -2` while dipping below
                // `life - 5` mid-cycle; this charges `0 + S`. That blindness is a property
                // of the NET INPUT and is identical under `max` — the operator swap neither
                // introduces nor repairs it. The backstops are conformance and the live
                // elimination guard during the drive.
                observed_life_loss.max(0) + declared_life_magnitude
            } else {
                observed_life_loss
            };
            narrow(p.life as i64 - 1, life_magnitude);
            // CR 704.5c. A positive poison delta is the per-period gain.
            narrow(
                9 - p.poison_counters as i64,
                self.poison.get(&p.id).copied().unwrap_or(0),
            );
            // CR 104.3c + CR 121.4. A negative library delta is the per-period drain.
            narrow(
                p.library.len() as i64,
                -self.library_delta.get(&p.id).copied().unwrap_or(0),
            );
        }

        bound.clamp(0, cap) as u32
    }

    /// CR 732.2a: **controller-scoped** net-progress — the single authority shared
    /// by Engine A ([`crate::analysis::detect_loop`]) and Engine B
    /// ([`crate::analysis::candidate_cycles`]). Returns true iff the cycle makes
    /// unbounded progress on ≥1 axis without leaving the loop's controller with an
    /// unsustainable net deficit on a *consumed* axis (their own life or mana).
    ///
    /// Distinct from [`Self::is_net_progress`] (PR-0) only in *who* the
    /// consumed-axis constraint applies to: the controller's life going negative
    /// is unsustainable (false), but an *opponent's* life/library going negative
    /// is the drain/mill win (progress). Engine B layers an `unbounded_production`
    /// override on top of this base check for dynamic production (HIGH-1).
    pub(crate) fn net_progress_for(&self, controller: PlayerId) -> bool {
        // CR 106.1: a loop that net-spends mana across the whole pool is not
        // sustainable. Mana is not attributed per player in the summed `mana`
        // array, so any net-negative color is a controller-side deficit.
        if self.mana.iter().any(|&n| n < 0) {
            return false;
        }
        // CR 119: the controller losing life across the cycle is unsustainable.
        for (pid, &n) in &self.life {
            if *pid == controller && n < 0 {
                return false;
            }
        }
        !self.unbounded_axes_for(controller).is_empty()
    }

    /// CR 732.2a + CR 704.5a: the unbounded axes of this delta with the
    /// opponent-vs-controller sign rules a win classifier needs. Builds on
    /// [`Self::unbounded_components`] (every strictly-positive axis plus any
    /// nonzero library) and additionally surfaces an **opponent's life loss**
    /// (negative life on a non-controller) as the drain win axis —
    /// `unbounded_components` only reports positive life (lifegain), so a pure
    /// drain loop would otherwise name no axis. Single authority shared by Engine
    /// A and Engine B.
    pub(crate) fn unbounded_axes_for(&self, controller: PlayerId) -> Vec<ResourceAxis> {
        let mut out: Vec<ResourceAxis> = self
            .unbounded_components()
            .into_iter()
            .map(|(axis, _)| axis)
            .collect();
        // CR 704.5a: an opponent's life driven *down* each cycle is the drain win.
        for (pid, &n) in &self.life {
            if n < 0 && *pid != controller {
                let axis = ResourceAxis::Life(*pid);
                if !out.contains(&axis) {
                    out.push(axis);
                }
            }
        }
        out
    }
}

/// Whether a resource axis is *consumed* (spendable inside a loop) or purely
/// *gained*. Consumed axes constrain loop sustainability; see
/// [`ResourceVector::is_net_progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Component {
    Consumed,
    Gained,
}

/// A tagged, named resource axis — the typed identity of one unbounded resource,
/// used by the (PR-2) `WinKind` classifier to describe a loop certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ResourceAxis {
    Mana(ManaType),
    Life(PlayerId),
    DamageDealt(PlayerId),
    LibraryDelta(PlayerId),
    Counter(CounterClass, ObjectClass),
    Trigger(TriggerKind),
    TokensCreated,
    CardsDrawn,
    Casts,
    LandfallTriggers,
    CombatPhases,
    ExtraTurns,
    DeathTriggers,
    EtbTriggers,
    LtbTriggers,
    SacTriggers,
    /// CR 704.5c: poison counters on a player (10 ⇒ that player loses). Appended at
    /// the END to keep the derived `Ord` discriminant of every earlier variant stable.
    Poison(PlayerId),
}

/// CR 122.1: classify a counter-bearing object by its core types.
pub(crate) fn object_class(core_types: &[CoreType]) -> ObjectClass {
    if core_types.contains(&CoreType::Creature) {
        ObjectClass::Creature
    } else if core_types.contains(&CoreType::Planeswalker) {
        ObjectClass::Planeswalker
    } else if core_types.contains(&CoreType::Battle) {
        ObjectClass::Battle
    } else {
        ObjectClass::Other
    }
}

/// Component-wise `after - before` for an ordered map, retaining every key on
/// either side and dropping entries that net to zero.
fn map_delta<K: Ord + Copy>(
    before: &BTreeMap<K, i64>,
    after: &BTreeMap<K, i64>,
) -> BTreeMap<K, i64> {
    let mut out = BTreeMap::new();
    for (&k, &a) in after {
        let b = before.get(&k).copied().unwrap_or(0);
        let d = a - b;
        if d != 0 {
            out.insert(k, d);
        }
    }
    for (&k, &b) in before {
        if !after.contains_key(&k) && b != 0 {
            out.insert(k, -b);
        }
    }
    out
}

/// CR 732.2a: the per-period resource signature of the RETAINED RING, derived — the
/// second certification basis for a bounded cycle offer, and the one that consults no
/// **board** predicate (objects / zones / tap-state) at all.
///
/// Searches `k` in `1..=(frames - 1) / 2`, smallest first, for a period whose consecutive
/// frame-deltas repeat, and certifies only a period it has observed **twice** (`2k` deltas
/// ⇒ `2k + 1` frames). `k` is an OUTPUT, never an input: no period constant exists in this
/// subsystem — `game::engine::shortcut_drive_period` derives its period from the template
/// schedule and `LOOP_DETECT_RING_CAP` merely CAPS how large a derivable `k` can be
/// (16 frames ⇒ `k <= 7`).
///
/// Fail-closed in FOUR places. Fewer than `2k + 1` frames for every candidate `k` ⇒ `None`
/// (a period seen once is a coincidence, not a signature). A smallest repeating period whose
/// delta is the zero vector ⇒ `None`, because every multiple of it is zero too and a cycle
/// that moves no resource states no CR 704 threshold to bound. A certifying window that is
/// not TURN-POSITION invariant ⇒ `None` (the CR 703.1 conjunct below). Reading the RING only
/// — never the live `state` — keeps the compared frames homogeneous: every ring frame is a
/// `normalize_for_loop` snapshot taken at `WaitingFor::Priority{active_player}`. That holds
/// across BOTH of `game::engine`'s `record_loop_detect_sample` sites (the settle sampler in
/// `pass_priority_once_with_pipeline` and the forced-window answer site in `apply_action`),
/// because the two gate on the same `wf` conjunct — the homogeneity argument no longer rests
/// on there being one site, it rests on that shared conjunct. Meanwhile the live state is not
/// normalized. It does not consult a board predicate, but it DOES require the frames it
/// compares to be homogeneous in turn position, which is what "homogeneous" above now means
/// in full.
///
/// ⚠ SCOPE OF THAT REQUIREMENT — what this function READS, versus where the frames' sameness
/// comes from. It reads exactly two things: `ResourceVector::snapshot(&f.normalized)` and
/// `window_scope_from_cover_frames(..).phase_invariant`, and `phase_invariant` is
/// `turn_number` + `phase` + `extra_phases.is_empty()`. The sampler gate that mints the frames
/// also makes them homogeneous in `waiting_for`/`priority_player`, but THIS function never
/// looks at those two — basis A does, via `loop_states_equal_modulo_resources` ⇒
/// `loop_states_equal` ⇒ `impl PartialEq for GameState`. Do not cite `ring_delta_signature` as
/// the consumer of either field.
pub(crate) fn ring_delta_signature(state: &GameState) -> Option<(u32, ResourceVector)> {
    let frames = state.loop_detect_ring.len();
    // 2k + 1 with k >= 1.
    if frames < 3 {
        return None;
    }
    let snaps: Vec<ResourceVector> = state
        .loop_detect_ring
        .iter()
        .map(|f| ResourceVector::snapshot(&f.normalized))
        .collect();
    let deltas: Vec<ResourceVector> = snaps
        .windows(2)
        .map(|w| ResourceVector::delta(&w[0], &w[1]))
        .collect();
    for k in 1..=(frames - 1) / 2 {
        // The MOST RECENT 2k deltas: a stale repeat in an older stretch of the ring says
        // nothing about the period the loop is running now.
        let recent = &deltas[deltas.len() - 2 * k..];
        if recent[..k] != recent[k..] {
            continue;
        }
        let per_period = ResourceVector::delta(&snaps[frames - 1 - k], &snaps[frames - 1]);
        // A cycle that moves no resource states no CR 704 threshold to bound, so it
        // supplies no per-period magnitude and is refused.
        //
        // This is deliberately a WHOLE-SEARCH refusal, and the struck justification for
        // it was WRONG. It read "smallest repeating period, so every longer one is a
        // whole number of copies of this one — a zero here cannot become non-zero at a
        // larger `k`". The repetition test above inspects only the most recent `2k`
        // deltas, which does NOT establish that the whole ring is periodic with period
        // `k`, so a larger `k'` need not be a multiple of `k` and its per-period delta
        // can be non-zero. Counter-example over 8 frames (deltas `d1..d7`, oldest
        // first): at `k = 1` the last two deltas are equal and zero, so this returns
        // `None`; at `k' = 3` the test compares `[d1,d2,d3]` against `[d4,d5,d6]`, and
        // `d5 = d6 = 0` forces `d2 = d3 = 0` while leaving `d4` unconstrained, so the
        // `k' = 3` period is `d4 + d5 + d6 = d4`, which can be non-zero.
        //
        // The BEHAVIOUR is still the safe direction — refusing outright costs a missed
        // offer, never a wrong one — so this is a comment defect, not a soundness
        // defect. It is corrected rather than deleted because the false claim is the
        // kind a later reader would lean on to justify widening the search while keeping
        // the early return, or to replace the search with a single-`k` probe. If that
        // missed class ever needs to certify, `continue` is sound here for the same
        // reason the return is safe: each candidate `k` is validated independently.
        if per_period == ResourceVector::default() {
            return None;
        }
        // CR 703.1 + CR 703.3: turn-based actions "happen automatically when certain steps
        // or phases begin, or when each step and phase ends", and CR 703.2 says they are
        // "not controlled by any player". CR 732.2a licenses a shortcut only over "a
        // sequence of game choices, for all players" — so a period whose repetition is paved
        // by step/phase boundaries is not a sequence that rule can describe. A 2-player
        // draw-go board is exactly periodic in `library_delta` and in an upkeep life ticker,
        // and without this conjunct that turn structure certifies as a "loop".
        //
        // Basis A cannot make that mistake: `loop_states_equal` delegates to
        // `impl PartialEq for GameState`, which compares `turn_number`, `active_player` and
        // `phase`, and neither `normalize_for_loop` nor `project_out_resources` neutralizes
        // any of the three — the deliberate, ratified design recorded at
        // `types::game_state::WaitingFor::is_forced_cascade_window`'s doc. Basis B compares
        // only resource deltas, so it escaped that discipline silently; this restores parity.
        // It is NOT a new policy and NOT a claim that shortcuts may not cross turns —
        // CR 732.2a says verbatim that a shortcut "may even cross multiple turns". What is
        // refused is a cross-turn certification by the BOARD-BLIND basis.
        //
        // KNOWINGLY ACCEPTED FALSE NEGATIVE, and it is the price of reusing the shipped
        // authority instead of forking a second turn-position predicate:
        // `window_scope_from_cover_frames` requires `extra_phases.is_empty()` on BOTH frames
        // (CR 500.8 — effects can add phases to a turn), not merely equal counts. So a
        // legitimate WITHIN-turn loop running while an extra phase is queued (the
        // extra-combat class) mints no basis-B offer. That is the fail-closed direction — a
        // missed offer, never a wrong one. If that class ever needs to certify, widen
        // `window_scope_from_cover_frames` ITSELF, where both suppressing firewall callers
        // see the change too; do not add a second local test here.
        let window: Vec<&GameState> = state
            .loop_detect_ring
            .iter()
            .skip(frames - (2 * k + 1))
            .map(|f| &f.normalized)
            .collect();
        if !window.windows(2).all(|w| {
            window_scope_from_cover_frames(w[0], w[1], None, None)
                .phase_invariant
                .is_some()
        }) {
            return None;
        }
        return Some((k as u32, per_period));
    }
    None
}

/// CR 732.2a vs CR 104.4b: the **complement** of the engine's strict loop
/// equality (`types::game_state::loop_states_equal`).
///
/// `loop_states_equal` treats two states as the same loop point only when life,
/// damage, counters, power/toughness, loyalty, and mana also match — correct for
/// a *mandatory* loop, which is a draw (CR 104.4b / CR 732.4) only if it truly
/// repeats with nothing changing.
///
/// This function answers the opposite question for a *beneficial* loop
/// (CR 732.2a, the shortcut): are the two states identical in **board, zones, and
/// tap-state**, allowing the monotone resources to differ? It is built directly
/// on `normalize_for_loop` (so it inherits the exact volatile-field exclusions
/// the strict path uses) and then additionally projects out the monotone
/// resources before delegating to `loop_states_equal`:
///
/// - per-player `life`, `mana_pool`, and the per-turn resource trackers
///   (life gained/lost, cards drawn, tokens, …) the strict `PartialEq` compares;
/// - per-object `damage_marked` and `counters` (and the counter-derived
///   `power`/`toughness`/`loyalty`/`defense`), so a +1/+1 or loyalty pump loop is
///   recognized as the same board.
///
/// Everything else — controller, zone, tapped, attachments, names, object count,
/// stack, phase, priority — must still match exactly, so a genuine board change
/// (an extra permanent, a different tap state, a moved card) returns `false`.
///
/// # Inherited extrapolation assumption (R1-B2 honesty; behavior UNCHANGED here)
///
/// This constant-depth path extrapolates the per-cycle resource delta over an
/// unbounded number of cycles WITHOUT a syntactic guard on either the on-stack or
/// the off-stack fire-time read surface — it trusts that a board-equal-modulo-
/// resources recurrence keeps reproducing the same delta. That premise is
/// refutable in principle (a dormant intervening-if / static / replacement that
/// reads a projected resource could arm mid-extrapolation), but the shipped 2p
/// drain detection depends on this behavior and it is regression-pinned, so it is
/// left as-is. The NEW growing-cascade path
/// ([`loop_states_cover_modulo_growth`]) closes both read surfaces by construction
/// rather than inheriting this assumption.
pub fn loop_states_equal_modulo_resources(a: &GameState, b: &GameState) -> bool {
    let pa = project_out_resources(a);
    let pb = project_out_resources(b);
    // CR 606.3: the per-object loyalty-activation count is the authoritative
    // once-per-turn-per-permanent gate, but `objects_content_eq` does NOT compare it
    // (and `normalize_for_loop` does not zero it), so a loyalty loop is invisible to
    // `loop_states_equal`. Compare it analysis-locally (do NOT widen the strict
    // comparator, do NOT zero the field) so a loop that re-activates a loyalty
    // ability (count k -> k+1) compares UNEQUAL and is not falsely certified.
    // F1 (PR-7 Phase 4d-ii / P7 v3): `last_loop_action_sequence` is EXCLUDED from `impl PartialEq
    // for GameState` (`loop_states_equal` never compares it) and NOT cleared by
    // `project_out_resources`, so compare it explicitly here (fail-closed) — a heterogeneous or
    // reordered period is caught (order-sensitive `Vec` `PartialEq`), a homogeneous period's
    // invariant sequence compares equal. `[] == []` for every non-loop-action state ⇒ zero
    // regression to existing loop-equality tests.
    loop_states_equal(&pa, &pb)
        && loyalty_activation_counts_match(&pa, &pb)
        && pa.last_loop_action_sequence == pb.last_loop_action_sequence
}

/// CR 606.3: per-object `loyalty_activations_this_turn` equality across two
/// projected states. Transparent for non-loyalty loops (all-zero counts compare
/// equal); discriminating for loyalty loops (the count grows each activation).
/// `loop_states_equal` already requires identical object sets before this runs, so
/// iterating one side's objects and comparing shared ids is symmetric.
fn loyalty_activation_counts_match(a: &GameState, b: &GameState) -> bool {
    a.objects.iter().all(|(id, oa)| {
        b.objects
            .get(id)
            .is_none_or(|ob| oa.loyalty_activations_this_turn == ob.loyalty_activations_this_turn)
    })
}

/// CR 110.1: a permanent is a card or token on the battlefield — this captures one such
/// permanent that persists at a loop's fixpoint (a residual board object, NOT a
/// [`ResourceAxis`] scalar). Identity via `oracle_id` (cross-incarnation stable,
/// CR 400.7-proof) so a later materialization phase can recreate it; `controller` +
/// `tapped` are the split B4 must preserve (the "+1 untapped").
// PR-7 Phase 3: serde-derived because it rides inside `LoopCertificate.residual_board_delta`,
// which serializes into `WaitingFor::LoopShortcut`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidualPermanent {
    pub oracle_id: String,
    pub controller: PlayerId,
    pub tapped: bool,
    // ponytail: counters/attachments deferred — YAGNI until a materializer consumes
    // them; add when the first consumer needs them, not before.
}

/// CR 110.1: the loop-invariant, non-recycled remainder of battlefield permanents for
/// ONE cycle — the concrete permanents present at the fixpoint that are NOT part of the
/// repeating consumed/produced pair (e.g. the one untapped creature that seeds each
/// tap). EMPTY for a constant-depth or stack-growth loop (their battlefields are
/// identical by construction). Non-empty only once an object-growth detection path feeds
/// [`board_delta`] non-identical battlefields.
// PR-7 Phase 3: serde-derived — serializes into `WaitingFor::LoopShortcut`'s certificate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BoardDelta {
    /// Battlefield permanents present in `after` but not `before` (by `ObjectId`).
    pub added: Vec<ResidualPermanent>,
    /// Battlefield permanents present in `before` but not `after`.
    pub removed: Vec<ResidualPermanent>,
}

/// Pure set-difference producer — analysis plumbing, deliberately UN-annotated per
/// CLAUDE.md ("don't annotate serialization or plumbing — only code that implements a
/// rule"): it computes `after − before` over battlefield permanents (the CR 110.1
/// concept lives on the types it produces, [`BoardDelta`]/[`ResidualPermanent`], not on
/// this diff). Iterates `state.objects.values()` filtered to `Zone::Battlefield`, keyed
/// by `ObjectId`. `oracle_id` is read from `obj.printed_ref.oracle_id` (falls back to an
/// empty string when absent — tokens without a printed ref). PURE.
pub fn board_delta(before: &GameState, after: &GameState) -> BoardDelta {
    fn battlefield_ids(state: &GameState) -> HashSet<ObjectId> {
        state
            .objects
            .values()
            .filter(|o| o.zone == crate::types::zones::Zone::Battlefield)
            .map(|o| o.id)
            .collect()
    }
    fn residual(state: &GameState, id: ObjectId) -> Option<ResidualPermanent> {
        state.objects.get(&id).map(|o| ResidualPermanent {
            oracle_id: o
                .printed_ref
                .as_ref()
                .map(|p| p.oracle_id.clone())
                .unwrap_or_default(),
            controller: o.controller,
            tapped: o.tapped,
        })
    }

    let before_ids = battlefield_ids(before);
    let after_ids = battlefield_ids(after);
    let added = after_ids
        .iter()
        .filter(|id| !before_ids.contains(id))
        .filter_map(|&id| residual(after, id))
        .collect();
    let removed = before_ids
        .iter()
        .filter(|id| !after_ids.contains(id))
        .filter_map(|&id| residual(before, id))
        .collect();
    BoardDelta { added, removed }
}

/// CR 732.2a: WHICH certificate a window's touch is derived under.
///
/// The frozen exemption's extrapolation limb needs BOTH a board-level premise
/// that the certified period cannot SHRINK the stack (P2) AND the Karp–Miller
/// read-surface guard that makes the fast-forward the repetition of the observed
/// period (P4). Exactly one certifying disjunct supplies both, so the exemption
/// is keyed to the DISJUNCT rather than to the basis — as a type rather than a
/// call-site convention, because a convention an executor drops compiles and is
/// fail-OPEN.
///
/// Three variants, two of which behave identically today, and that is
/// deliberate: collapsing the equality disjunct onto [`PeriodCertification::
/// ResourceSignatureOnly`] would make the type lie about provenance (equality
/// DOES consult a board predicate), and a mislabel is the fail-open re-entry
/// path — the obvious future "fix" for an equality pair tagged
/// `ResourceSignatureOnly` is to retag it `BoardCovered`.
/// `pub` rather than `pub(crate)` for ONE reason, named so a future reader does not
/// widen it further: it is the type of [`crate::game::engine::MintMeter`]'s
/// `certification` field, the only surface on which the certifying disjunct is
/// observable at all. It is not serialized, not a variant on any gated engine enum,
/// and no card-data export reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodCertification {
    /// Basis A, COVER disjunct — the pair passed
    /// [`loop_states_cover_modulo_growth_pinned`]. P2: item (2)'s `stack_covers`
    /// ⇒ strictly growing depth. P4: items (4)/(5) ⇒ no current-stack entry and
    /// no live fire-time condition reads a still-projected axis. This is the
    /// ONLY value under which `frozen_ids` is non-empty.
    BoardCovered,
    /// Basis A, EQUALITY disjunct — the pair passed
    /// [`loop_states_equal_modulo_resources`]. P2 holds (the stack is compared
    /// exactly ⇒ constant depth) but P4 does NOT: that predicate has no items
    /// (4)/(5), and this crate says so in its own words — the Karp–Miller NOTE
    /// above [`loop_states_cover_modulo_growth`] ("makes the SAME extrapolation
    /// with NONE of these") and that predicate's own inherited-assumption
    /// section. A replacement that arms mid-extrapolation is exactly the route
    /// P4 forecloses, so `frozen_ids` is forced EMPTY here. The shipped
    /// constant-depth 2p drain detection is unaffected: this value narrows only
    /// the NEW subtraction.
    BoardEqualOnly,
    /// Basis B — [`ring_delta_signature`] only, which by its own doc "does not
    /// consult a board predicate". Neither P2 nor P4 ⇒ `frozen_ids` is forced
    /// EMPTY and the resolution gate scans every current-stack entry, exactly as
    /// before this change.
    ResourceSignatureOnly,
}

/// CR 732.2a + CR 608.1: what ONE certified period actually announced, and which
/// current-stack entries the window proves it never touched.
///
/// Derived from the retained ring window `[cert_prior .. current]` — never from
/// the offer-beat stack snapshot. Its frames and entries are the ring sample's
/// **LIVE** halves, never `normalize_for_loop()` products: normalization zeroes
/// `next_object_id` and strips trigger identity, so a normalized frame is a
/// CR 104.4b comparand and not an evaluation board.
///
/// Fail-closed both ways: an id the window cannot prove frozen is TOUCHED
/// (scanned/refused as before), and an entry the window never shows announcing
/// is not enumerable for pins (the offer under-publishes ⇒ the choice gate
/// refuses ⇒ no offer).
#[derive(Debug)]
pub(crate) struct PeriodTouch<'a> {
    /// `(carrying_frame, entry)` for every entry that ANNOUNCED inside the
    /// window: present on frame `i`'s stack, absent from frame `i-1`'s (by
    /// `StackEntry.id`). The carrying frame is the state the entry is evaluated
    /// against — CR 603.3d announcement choices are a property of the board the
    /// ability was put on the stack against.
    pub(crate) announced: Vec<(&'a GameState, &'a StackEntry)>,
    /// Entry ids of `current.stack` that the window proves FROZEN: the same id
    /// at the same index in EVERY window frame AND in `current`. CR 608.1: only
    /// the top of the stack resolves, so an entry that held its `(index, id)`
    /// through the whole window neither announced nor resolved in it.
    ///
    /// NON-EMPTY ONLY UNDER [`PeriodCertification::BoardCovered`]: the
    /// subtraction is the one fail-open half of this type, and the other two
    /// certificates do not supply the premises it rests on.
    pub(crate) frozen_ids: BTreeSet<ObjectId>,
}

/// CR 732.2a: derive one window's [`PeriodTouch`] under the certificate its
/// caller actually holds.
///
/// `window` is `ring_live[cert_idx..]`, oldest first; the observed frame
/// sequence is `window ++ [current]`. `announced` is IDENTICAL on all three
/// certificate values — widening the announced set is the fail-CLOSED direction
/// — and only the frozen subtraction is keyed.
pub(crate) fn certified_period_touch<'a>(
    window: &[&'a GameState],
    current: &'a GameState,
    cert: PeriodCertification,
) -> PeriodTouch<'a> {
    if window.is_empty() {
        // CR 608.1 + CR 732.2a: with NO window frame there is no transition to
        // observe, so there is no frozen proof and no observed period; the
        // honest degenerate reading is "every current entry may announce",
        // which is exactly the snapshot mint this function's alias replaces.
        return PeriodTouch {
            announced: current.stack.iter().map(|e| (current, e)).collect(),
            frozen_ids: BTreeSet::new(),
        };
    }

    let mut announced: Vec<(&'a GameState, &'a StackEntry)> = Vec::new();
    let mut prev_ids: HashSet<ObjectId> = window[0].stack.iter().map(|e| e.id).collect();
    for frame in window
        .iter()
        .skip(1)
        .copied()
        .chain(std::iter::once(current))
    {
        for entry in &frame.stack {
            if !prev_ids.contains(&entry.id) {
                announced.push((frame, entry));
            }
        }
        prev_ids = frame.stack.iter().map(|e| e.id).collect();
    }

    // The fail-open subtraction, and the ONLY thing the certificate keys. Taken
    // BEFORE the frozen walk, so a non-cover certificate never pays it either.
    if !matches!(cert, PeriodCertification::BoardCovered) {
        return PeriodTouch {
            announced,
            frozen_ids: BTreeSet::new(),
        };
    }

    let frozen_ids = current
        .stack
        .iter()
        .enumerate()
        .filter(|(index, entry)| {
            window
                .iter()
                .all(|frame| frame.stack.get(*index).map(|e| e.id) == Some(entry.id))
        })
        .map(|(_, entry)| entry.id)
        .collect();
    PeriodTouch {
        announced,
        frozen_ids,
    }
}

/// CR 603.5 + CR 603.4 + CR 608.2k: re-classify ONE entry's ability with its
/// published "may" gate discharged, on the board `resolve_top` would hand the
/// resolver — this entry off the stack, resolution scope bound.
///
/// The ONE classifier answers the counterfactual; this module never
/// re-implements the six independent reasons the classifier returns `MayPrompt`
/// for. `None` ⇒ not a triggered ability, or the resolution scope cannot bind,
/// both of which are refusals rather than relief.
fn optional_cleared_classification(
    frame: &GameState,
    entry: &StackEntry,
    budget: &mut ProbeBudget,
) -> Option<crate::game::resolution_prompt::ResolutionChoiceFreedom> {
    let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
        return None;
    };
    let mut without_may_gate = (**ability).clone();
    without_may_gate.optional = false;
    // CR 732.2a: the same cheap-precondition-before-the-clone rule the primary
    // classifier follows. This is the SIBLING site, and it had the identical shape:
    // `frame.clone()` is a whole `GameState` copy, and a `may` trigger whose ability is
    // rejected on a pure AST gate (a non-allow-listed effect, an `UpTo` count, a modal
    // header) used to buy that copy plus a scope binding to reach a verdict that never
    // looks at the board — once per ring frame.
    //
    // EQUIVALENCE, verified rather than assumed. Hoisting changes exactly one case:
    // an entry whose scope would have FAILED to bind AND whose chain is gated now
    // returns `Some(MayPrompt)` where it previously returned `None`. `residual` has
    // exactly one reader (`optional_relief_for`), and it opens
    // `match cached.residual.as_ref()?` with a `MayPrompt => None` arm — so `None` and
    // `Some(MayPrompt)` produce the identical downstream result. Re-derived here by
    // grepping every `.residual` read in this file: one, plus one comment.
    if crate::game::resolution_prompt::chain_offers_choice(&without_may_gate) {
        return Some(crate::game::resolution_prompt::ResolutionChoiceFreedom::MayPrompt);
    }
    let mut board = frame.clone();
    board.stack.retain(|e| e.id != entry.id);
    if !crate::game::stack::bind_resolution_scope(&mut board, entry, None) {
        return None;
    }
    Some(
        crate::game::resolution_prompt::ability_resolution_choice_freedom(
            &board,
            &without_may_gate,
            budget,
        ),
    )
}

/// CR 732.2a: the facts a CALLER has PROVED about the loop window it is asking a
/// window predicate to certify. Every field is a *proof obligation discharged by the
/// caller*, never a request: a caller that has proved nothing passes
/// [`LoopWindowScope::unproven`] and gets byte-identical pre-change behaviour, so the
/// design is FAIL-CLOSED BY CONSTRUCTION — forgetting to thread a proof can only make
/// a predicate more conservative, never less.
///
/// The `_scoped` predicates below stay identity for [`LoopWindowScope::unproven`]
/// (asserted by `scoped_wrappers_are_identity`) because every guard that reads a field
/// sits inside an `if let Some(..)` / `is_some_and`. EVERY field is now read:
/// `phase_invariant` and `sole_driver` by the growing-class firewall's CR 510.2 / CR 506.1
/// and CR 117.1b guards, `cast_card_ids` by the projected firewall's CR 601.2f cost guard,
/// and `pinned` by [`loop_states_cover_modulo_growth_scoped`]'s CR 732.2a gates (3) and (6).
#[derive(Debug, Clone, Copy)]
pub(crate) struct LoopWindowScope<'a> {
    /// `Some(phase)` iff the caller proved both frames are equal on turn number AND
    /// step-granular phase (CR 500.1 turn structure / CR 506.1 combat steps /
    /// CR 510.2 the combat-damage step). `None` at any caller whose window CROSSES a
    /// phase or step boundary.
    phase_invariant: Option<Phase>,
    /// `Some(p)` iff the caller proved the whole window is driven by `p` and no other
    /// player receives priority inside the taken shortcut (CR 117.1b: a player may
    /// activate an ability only with priority; CR 732.2c: the shortcut advances to
    /// the proposed ending point once every player has accepted).
    sole_driver: Option<PlayerId>,
    /// `Some(pins)` iff the caller proved an OFFER published exactly these per-iteration
    /// choice slots. READ by [`loop_states_cover_modulo_growth_scoped`]'s gates (3)/(6).
    pinned: Option<PinnedChoices<'a>>,
    /// CR 601.2f (cost determination reads static cost modifiers): `Some(ids)` iff the
    /// caller proved the EXACT set of card ids this window casts — `Some(&[])` for a
    /// window that provably casts nothing. `None` means NO PROOF, i.e. scan everything.
    cast_card_ids: Option<&'a [CardId]>,
    /// CR 732.2a + CR 608.1: `Some(touch)` iff the caller proved WHICH pairs the
    /// certified period announced and which current-stack entries it left
    /// frozen. A Copy HANDLE, never the owned value — [`PeriodTouch`] owns a
    /// `Vec` and a `BTreeSet`, so an owned field would be E0204 against this
    /// struct's shipped `Copy` derive, while `Option<&T>` is `Copy` regardless
    /// of `T`. `None` means NO PROOF: nothing is exempt and nothing is
    /// enumerable, i.e. the pre-change width.
    period: Option<&'a PeriodTouch<'a>>,
}

/// CR 732.2a: the per-iteration choice slots ONE offer published, carried together with the
/// seat whose offer minted them.
///
/// The proposer travels WITH the slots because the relief side re-runs the MINT's own
/// per-entry acceptance test (`game::engine::entry_publishes_pin_slots`), whose first
/// conjunct is `entry.controller == proposer` — the EXTENSION POINT's precondition (c),
/// "only the acting player's own choices are pinnable". A bare slot list cannot express
/// that conjunct, and a SECOND scope field could disagree with the first; one field
/// carrying both makes the pair unable to drift.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PinnedChoices<'a> {
    /// The offer's proposer. Every slot in `slots` was minted for this seat.
    pub(crate) proposer: PlayerId,
    /// The published slots, which `decision_template::predictability_gate` then FORCES the
    /// declaration to pin. A slot listed here is a *specified* choice in CR 732.2a's sense,
    /// not a free one.
    pub(crate) slots: &'a [DecisionSlot],
}

impl LoopWindowScope<'static> {
    /// The zero-proof scope. Every 2-arg wrapper passes this, which is what makes the
    /// wrappers structurally identity rather than conditionally so.
    pub(crate) const fn unproven() -> Self {
        Self {
            phase_invariant: None,
            sole_driver: None,
            pinned: None,
            cast_card_ids: None,
            // DELIBERATE, and it is the whole meaning of this constructor: a
            // caller that has proved nothing gets byte-identical pre-change
            // behaviour. `Option<&PeriodTouch>` is const-constructible as
            // `None`, so this stays a `const fn` on `LoopWindowScope<'static>`.
            period: None,
        }
    }
}

/// CR 510.2 / CR 506.1 / CR 117.1b: the proof a cover pair carries about its own
/// window. SINGLE AUTHORITY — both suppressing firewall callers derive their scope
/// here, so the two [`LoopWindowScope`] populations can never drift apart.
///
/// `phase_invariant`: `Some(phase)` only when the frames agree on turn number AND
/// step-granular phase AND neither carries a pending extra phase (CR 500.8 can insert
/// a duplicate of the SAME phase inside one turn, which would break "equal phase ⇒
/// never left it"). Derived LOCALLY from the frames rather than read off a preceding
/// gate, so it is independent of gate ORDER — in
/// [`loop_states_cover_modulo_fodder_growth`] the firewall call PRECEDES
/// `eq_except_growable`. (`extra_turns` is deliberately NOT a conjunct: an extra TURN
/// is taken after the current one and `turn_number` is monotone, so it cannot insert a
/// duplicate phase inside a window whose frames already agree on `turn_number`.)
///
/// `sole_driver`: `Some(p)` only when BOTH frames' driving sequences are non-empty and
/// every entry in BOTH names controller `p` (CR 117.1b: a player may activate an
/// ability only with priority, and no other player receives priority inside the taken
/// shortcut). Reading only `prior` would mint `Some(p)` for a window whose other frame
/// was driven by someone else — the RELIEVING direction. An empty sequence proves
/// nothing, so it yields `None`, not "nobody drove this".
///
/// Fail-closed in every branch: a frame pair that proves nothing gets the
/// [`LoopWindowScope::unproven`] values and therefore byte-identical behaviour.
fn window_scope_from_cover_frames<'a>(
    pa: &GameState,
    pb: &GameState,
    pinned: Option<PinnedChoices<'a>>,
    period: Option<&'a PeriodTouch<'a>>,
) -> LoopWindowScope<'a> {
    // (p1) same turn, (p2) same step-granular phase, (p3) no pending extra phase in
    // either frame (CR 500.8).
    let phase_invariant = (pa.turn_number == pb.turn_number
        && pa.phase == pb.phase
        && pa.extra_phases.is_empty()
        && pb.extra_phases.is_empty())
    .then_some(pa.phase);

    // (s1) BOTH sequences non-empty; (s2) one controller across BOTH sequences. Both conjuncts
    // are exactly [`GameState::loop_period_controller`] applied per frame — "whose period is
    // this", the single authority every routing site reads — with the two answers required to
    // agree. Stating it that way rather than re-deriving `first().controller` + `all()` here is
    // the point of hoisting that authority: a two-frame twin of the same question cannot drift
    // from the one-frame form it duplicates.
    let sole_driver = pa
        .loop_period_controller()
        .filter(|driver| pb.loop_period_controller() == Some(*driver));

    LoopWindowScope {
        phase_invariant,
        sole_driver,
        pinned,
        // 2b's axis (the PROJECTED covers), derived at its own call site.
        cast_card_ids: None,
        // From the parameter: the SINGLE scope authority must be able to carry
        // the period proof, or the cover disjunct's own caller would have to
        // assemble a scope itself — which is exactly what the private fields
        // exist to prevent.
        period,
    }
}

/// CR 732.2a: is this stack entry's ANNOUNCEMENT-time target choice (gate (3)) already
/// SPECIFIED by a slot the offer published?
///
/// The acceptance test is NOT re-derived here: it is the mint's own,
/// [`crate::game::engine::entry_publishes_pin_slots`], called for this one entry with the
/// pins' own proposer. That is what keeps the relief predicate from being coarser than the
/// mint predicate — controller (precondition (c)), entry kind, target shape and the
/// CR 400.7 incarnation binding are all one function, so a slot can never be *matched*
/// here on terms it was not *minted* on.
///
/// SCOPE OF DISCHARGE. The caller's relief is a bare `continue`, so a `true` here skips
/// ALL FOUR facts [`stack_entry_has_no_ordering_input`] rejects on, while the pin answers
/// exactly one of them (the target). Three of the other three are ability facts the mint
/// itself now refuses to publish on (`multi_target` / `distribution` /
/// `target_constraints`). The fourth, `pending_trigger_entry` (CR 603.3c mid-construction),
/// is a property of THIS state rather than of the offer's schema, so it is enforced HERE —
/// the mint is documented a function of the BOARD, never of the PROMPT (it reads many
/// `GameState` fields; what it must never read is a prompt-coupled one), and
/// `pending_trigger_entry` is set exactly while a `TriggerTargetSelection` prompt is up.
/// That makes this predicate strictly NARROWER than the mint's, which
/// is the safe direction; the forbidden direction is coarser.
///
/// Fail-closed in every branch: no published pins, a non-qualifying entry, a missing source
/// object, or a mid-construction entry ⇒ not pinned ⇒ the gate that called this keeps
/// rejecting.
/// THE BOARD IS THE PAIR'S CARRYING FRAME — the identical `&GameState` the caller
/// handed [`PeriodVerdicts::frame_ix`] to mint `f`, never a second board. That is
/// what makes the two halves agree by construction: the cached `published` is
/// `entry_publishes_pin_slots(frames[f], entry, proposer)`, so the mint half and
/// the CR 603.3c half read ONE board per key. Dropping the board (and with it the
/// CR 603.3c conjunct) would COMPILE and would be fail-OPEN, which is why it is a
/// parameter rather than something a `FrameIx` is expected to recover:
/// `PeriodVerdicts.frames` is private to `verdict_memo`.
fn entry_target_choice_is_pinned(
    board: &GameState,
    f: FrameIx,
    entry: &StackEntry,
    verdicts: &mut PeriodVerdicts<'_>,
    scope: LoopWindowScope<'_>,
) -> bool {
    let Some(pins) = scope.pinned else {
        return false;
    };
    // A container bound to one proposer can never relieve pins minted for
    // another seat: the cached `published` IS the mint's answer for the
    // container's own proposer, so consuming it under different pins would be
    // reading a verdict minted for someone else.
    if verdicts.proposer() != Some(pins.proposer) {
        return false;
    }
    if board.pending_trigger_entry == Some(entry.id) {
        return false;
    }
    // CR 601.2c: a may-only entry (shape (B)) publishes NO target slot, so it is not
    // target-relieved here — its announcement freedom comes from
    // `stack_entry_has_no_ordering_input`'s own `targets.is_empty()` arm instead. Requiring
    // `Some` keeps this predicate strictly narrower than the mint, never coarser.
    verdicts
        .verdict(f, entry)
        .published
        .as_ref()
        .and_then(|e| e.target.as_ref())
        .is_some_and(|target| pins.slots.contains(target))
}

/// CR 732.2a + CR 603.5: is this entry's RESOLUTION-time `MayPrompt` (gate (6)) fully
/// explained by an optional gate the offer published — and if so, what verdict does the
/// entry carry once that one axis is discharged?
///
/// `Some(residual)` ⇒ relieved, and `residual` is the classification the entry would have
/// had WITHOUT its CR 603.5 gate, which the caller must go on to gate exactly like any
/// unpinned entry's (a pinned "may" says nothing about the CR 616.1 replacement surface
/// for whichever event classes the residual names). `None` ⇒ no relief.
///
/// ATTRIBUTION is the load-bearing part. `ability_resolution_choice_freedom` returns
/// `MayPrompt` for SIX independent reasons (`game/ability_scan.rs:6534-6560` plus the
/// sub/else effect join), and the offer publishes a `MayChoice` point for exactly ONE of
/// them — `ability.optional`. So relief requires both published slots to be pinned AND the
/// same ability, re-classified with `optional` cleared, to come back choice-free: an
/// `unless_pay`, a resolution-time target chooser, a modal header, a controller-choice
/// repeat, or a CR 701.34a proliferate sub-ability keeps returning `MayPrompt` and gets no
/// relief, because no published pin specifies it.
/// It performs NO classification of its own and calls the mint not at all: both
/// the published slots and the optional-cleared residual are read through the ONE
/// door, which is what keeps the relief from being a second, drifting authority.
fn pinned_may_choice_relief(
    f: FrameIx,
    entry: &StackEntry,
    verdicts: &mut PeriodVerdicts<'_>,
    scope: LoopWindowScope<'_>,
) -> Option<crate::game::resolution_prompt::ResolutionChoiceFreedom> {
    use crate::game::resolution_prompt::ResolutionChoiceFreedom;
    let pins = scope.pinned?;
    // Fail-closed agreement guard: relief may only consume a verdict minted for
    // the seat whose offer published these pins.
    if verdicts.proposer() != Some(pins.proposer) {
        return None;
    }
    let cached = verdicts.verdict(f, entry);
    let published = cached.published.as_ref()?;
    let may = published.may.as_ref()?;
    // Strictly the mint's own facts, never coarser (CR 603.5): the `may` slot must be
    // pinned, and the target slot must be pinned WHEN THERE IS ONE. Shape (B) publishes
    // `target: None` — an entry that announces no choice has none for a pin to leave
    // unspecified — so demanding a pinned target there would refuse relief the mint's own
    // schema fully describes.
    if !pins.slots.contains(may)
        || published
            .target
            .as_ref()
            .is_some_and(|target| !pins.slots.contains(target))
    {
        return None;
    }
    match cached.residual.as_ref()? {
        ResolutionChoiceFreedom::MayPrompt => None,
        residual @ ResolutionChoiceFreedom::FreeUnlessReplacements(_) => Some(residual.clone()),
    }
}

/// CR 603.5: is this entry's optional trigger ALREADY ANSWERED by a stored "don't ask
/// again" auto-choice — and with which answer?
///
/// ADOPTION C. This used to re-derive the CR 603.5 gate's conjunct set here, which made it a
/// THIRD copy: it asked the three repeat-shape predicates and the recipient authority
/// directly, and — measured — it OMITTED both `optional_for` and the feasibility probe, so it
/// called a may "answered" on two ability shapes where the gate never reads the store at all.
/// It now delegates the whole question to `effects::stored_may_answer`, the consumer half of
/// the one authority `resolve_chain_body`'s own branch and the mint's guard (b) both take.
///
/// NOT keyed to the proposer, and that survives the adoption: the gate asks whoever
/// `optional_prompt_player` names, and a stored answer specifies that choice regardless of
/// whose it is — a per-iteration window that never opens is specified for every seat at once
/// (CR 732.2a).
///
/// `None` ⇒ no up-front gate opens, or it will PROMPT, or the ability carries no
/// `may_trigger_origin` for a preference to key on. All are unspecified windows, which is the
/// fail-closed direction.
fn auto_may_answer_for(
    frame: &GameState,
    entry: &StackEntry,
) -> Option<crate::types::game_state::AutoMayChoice> {
    let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
        return None;
    };
    crate::game::effects::stored_may_answer(frame, ability)
}

/// CR 732.2a + CR 603.5: relief basis ONE for gate (6)'s `MayPrompt` — the may this entry
/// announces is answered by a stored auto-choice, so the shortcut opens no window there.
///
/// The SIBLING of [`pinned_may_choice_relief`] and deliberately its identical shape: both
/// discharge exactly the `ability.optional` axis and both hand back the SAME
/// optional-cleared residual for the caller to go on gating, because a specified `may`
/// says nothing about the CR 616.1 replacement surface of the events it then proposes.
/// The five OTHER reasons `ability_resolution_choice_freedom` returns `MayPrompt` — an
/// `unless_pay`, a resolution-time target chooser, a modal header, a controller-choice
/// repeat, a CR 701.34a proliferate sub-ability — keep returning `MayPrompt` as the
/// residual and get no relief here, exactly as they get none from a pin.
///
/// ONLY `Accept` IS RELIEVED, and the asymmetry is not caution — it is what the residual
/// MEANS. `optional_cleared_classification` re-classifies the ability as if it RESOLVED
/// with its gate discharged, which is what a stored `Accept` produces. A stored `Decline`
/// is equally prompt-free but produces the opposite board, so that residual would be a
/// claim about events the shortcut never proposes. (It also cannot arise in a certified
/// period: a declined `may` contributes none of the per-cycle delta the ring recurrence
/// was built from.)
fn auto_may_choice_relief(
    frame: &GameState,
    f: FrameIx,
    entry: &StackEntry,
    verdicts: &mut PeriodVerdicts<'_>,
) -> Option<crate::game::resolution_prompt::ResolutionChoiceFreedom> {
    use crate::game::resolution_prompt::ResolutionChoiceFreedom;
    use crate::types::game_state::AutoMayChoice;
    if !matches!(auto_may_answer_for(frame, entry)?, AutoMayChoice::Accept) {
        return None;
    }
    match verdicts.verdict(f, entry).residual.as_ref()? {
        ResolutionChoiceFreedom::MayPrompt => None,
        residual @ ResolutionChoiceFreedom::FreeUnlessReplacements(_) => Some(residual.clone()),
    }
}

/// Karp–Miller-style ω-acceleration (Karp–Miller 1969; Finkel et al. 2021), sound
/// GIVEN the in-loop transition relation — the WHOLE beat: top-of-stack resolution
/// (CR 608.1) with its resolution-time payments (CR 605.3a / CR 608.2g), trigger
/// collection (CR 603.4), replacement application (CR 614.1), static condition
/// gating (CR 604.1 / CR 613.1), SBA application (CR 704.3 / CR 704.5), and elimination
/// processing (CR 800.4a) — is invariant under the projected-out player-level
/// resources. Enforced by construction: object/board axes are STRICT-COMPARED
/// ([`object_resource_axes_match`] — SBA object reads CR 704.5f/g/i can never
/// observe hidden drift); the remaining projected set (player monotone resources +
/// journals) is scanned fail-closed on BOTH read surfaces
/// ([`stack_entry_reads_projected_resource`] on every current-stack entry,
/// [`fire_time_conditions_read_projected_resource`] on every live
/// trigger/replacement/static definition); player-life SBAs are the modeled outcome
/// itself (controller non-dip + all-fallers-simultaneous, so the first CR 800.4a
/// elimination is terminal per CR 104.2a); library/poison drift is firewalled to
/// `None` by the winner predicate. Depth-independence of top-of-stack resolution:
/// CR 608.1 / CR 405.5.
///
/// NOTE: the shipped constant-depth 2p path
/// ([`loop_states_equal_modulo_resources`]) makes the SAME extrapolation with NONE
/// of these — that inherited assumption is documented there, not silently claimed
/// as a theorem here.
///
/// Returns `true` iff `current` **covers** `prior`: board equal modulo the narrowed
/// projection with object resource axes strict-equal (item 1), `prior`'s normalized
/// stack order-preservingly embeds in `current`'s with strict growth confined to
/// already-occupied places (item 2), every grown place is a mandatory
/// no-ordering-input triggered ability (item 3), no current-stack entry reads a
/// still-projected resource (item 4), no live fire-time condition reads one
/// either (item 5), and no current-stack entry can open a resolution-time player
/// choice — either intrinsically or through the life-event replacement
/// environment (item 6, CR 732.2a + CR 608.2d).
pub(crate) fn loop_states_cover_modulo_growth(prior: &GameState, current: &GameState) -> bool {
    // The zero-proof container: frames = `[current]`, no proposer ⇒ nothing published ⇒ no
    // relief, which is byte-identically what an `unproven()` scope already meant. The four
    // production callers of this 2-arg entry point are therefore untouched.
    let mut verdicts = PeriodVerdicts::unproven(current);
    loop_states_cover_modulo_growth_scoped(
        prior,
        current,
        LoopWindowScope::unproven(),
        &mut verdicts,
    )
}

/// CR 732.2a "predictable results": is EVERY per-iteration choice this stack can open a
/// SPECIFIED one — published by the offer's pins, or absent altogether?
///
/// The conjunct a BOUNDED offer needs, and the reason it is not
/// [`loop_states_cover_modulo_growth_scoped`]: that predicate answers whether one frame
/// COVERS another, and its item (1) additionally requires `object_resource_axes_match`
/// STRICTLY — an axis [`loop_states_equal_modulo_resources`] deliberately projects OUT. An
/// offer certified by exact recurrence would therefore be refused by an unrelated BOARD fact
/// while its choice surface was never examined. Cover is the authority for cover; this is the
/// authority for choices.
///
/// SINGLE AUTHORITY nonetheless, shared verbatim with that predicate's gates (3) and (6) —
/// the same [`stack_entry_has_no_ordering_input`] (CR 601.2c announcement-time input, reached
/// for a triggered ability via CR 603.3d), the same
/// [`stack_entry_resolution_choice_freedom`] (CR 608.2d resolution-time prompts), the same
/// [`entry_target_choice_is_pinned`] / [`pinned_may_choice_relief`] pin relief, and the same
/// CR 616.1 [`proposed_event_prompt_cause`] environmental guard the
/// `FreeUnlessReplacements` verdict's own contract requires. Nothing is re-derived here.
///
/// THE WIDTH IS THE CERTIFIED PERIOD'S, NOT THE OFFER-BEAT STACK'S. Both loops range over
/// `touch.announced` ∪ (`state.stack` \ `touch.frozen_ids`), each pair evaluated against its
/// own carrying frame. That is WIDER than the offer-beat stack on the announced half — the
/// majority population on both measured dumps, 157/161 (F4) and 19/23 (dellian) beats carry
/// off-stack announced pairs — and NARROWER only on the proven-frozen half.
///
/// ⚠ THE JUSTIFICATION THIS DOC USED TO CARRY FOR SCANNING EVERY CURRENT-STACK ENTRY WAS
/// CORRECT FOR A FUNCTION WITH NO WINDOW, AND IS NOW SUPERSEDED BY ONE. It read: "a frozen
/// entry is one the window has NO evidence about, which is precisely the entry a grown-only
/// scan would skip. The width is right." Its PREMISE survives — without a window parameter
/// "no evidence" was all this predicate could say. With one, absence of evidence about
/// RESOLUTION becomes positive evidence about POSITION: a frozen id is PROVEN to hold the
/// same (index, id) in every certified frame and in `state`, under a certificate that forbids
/// the stack shrinking. Measured on the `dellian` 4p fixture, a growing cascade over a frozen
/// bottom prefix: up to 152 of 156 current-stack entries are exempt at the certified beat.
///
/// WHAT LICENSES THE EXEMPTION HERE, since this function establishes none of the cover
/// predicate's premises itself (its whole body is the two loops plus the CR 616.1 tail): a
/// non-empty `frozen_ids` can only have been built under [`PeriodCertification::BoardCovered`]
/// ⟺ the offer's basis-A `else if` matched ⟺ [`loop_states_cover_modulo_growth_pinned`]
/// returned `true` ⇒ that predicate's items (2)/(4)/(5) all passed UNEXEMPTED, and those run
/// strictly before this conjunct. The premises are inherited as discharged facts about the
/// same `(prior, current)` pair, never assumed. When `frozen_ids` is empty — every basis-B
/// path, every equality-certified path, the degenerate alias — every current-stack entry is
/// scanned exactly as before this change.
pub(crate) fn stack_choices_are_all_specified<'a>(
    state: &'a GameState,
    proposer: PlayerId,
    slots: &[DecisionSlot],
    touch: Option<&PeriodTouch<'a>>,
    verdicts: &mut PeriodVerdicts<'a>,
) -> bool {
    // Only `pinned` and `period` are read below; the other three proofs belong to the cover
    // axes and this predicate makes no claim about them. Written out in full so a future SIXTH
    // field is a compile error that forces a decision rather than a silent default.
    let scope = LoopWindowScope {
        phase_invariant: None,
        sole_driver: None,
        pinned: Some(PinnedChoices { proposer, slots }),
        cast_card_ids: None,
        period: touch,
    };
    // CR 732.2a: the described sequence is EVERY choice the shortcut makes, not the subset
    // that happens to sit on the stack at the offer beat. The mint's own domain is
    // `touch.announced`, so both loops range over the announced pairs UNION the current-stack
    // entries the window did not prove frozen — each pair evaluated against ITS OWN carrying
    // frame, never against the live board (a target legal on the frame but gone from `current`
    // would collapse the assignment to "forced" and relieve a choice that is not forced).
    let mut pairs: Vec<(&'a GameState, &'a StackEntry)> = Vec::new();
    if let Some(t) = touch {
        pairs.extend(t.announced.iter().copied());
    }
    for entry in &state.stack {
        // CR 608.1: an entry the window proves held its (index, id) through every certified
        // frame neither announced nor resolved in the described sequence.
        if touch.is_some_and(|t| t.frozen_ids.contains(&entry.id)) {
            verdicts.note_conjunct6_frozen_skip();
            continue;
        }
        pairs.push((state, entry));
    }

    // Announcement-time (gate (3)'s fact), CR 603.3d.
    for (frame, entry) in &pairs {
        // Fail-closed: a frame outside this container's period cannot be asked about.
        let Some(f) = verdicts.frame_ix(frame) else {
            return false;
        };
        if !(entry_target_choice_is_pinned(frame, f, entry, verdicts, scope)
            || stack_entry_has_no_ordering_input(frame, entry))
        {
            return false;
        }
    }
    // Resolution-time (gate (6)'s fact), including its paired CR 616.1 obligation.
    // The obligation is discharged PER ENTRY against the pipeline's own candidate
    // authority, on the same board the entry's events were derived on.
    for (frame, entry) in &pairs {
        let Some(f) = verdicts.frame_ix(frame) else {
            return false;
        };
        verdicts.note_conjunct6_ask();
        let primary = verdicts.verdict(f, entry).primary.clone();
        let verdict = match primary {
            crate::game::resolution_prompt::ResolutionChoiceFreedom::MayPrompt => {
                // CR 603.5: TWO bases can specify a `may`, and they are mutually exclusive
                // by construction — the mint's guard (b) publishes a `MayChoice` slot only
                // for a may that has NO stored auto-choice, and withholds it for one that
                // does. Asking the auto basis first is therefore an ordering of disjoint
                // cases, not a precedence. An auto-answered may is the MOST determined a
                // per-iteration choice can be; reading its slotless mint as "unspecified"
                // was the defect.
                let relief = auto_may_choice_relief(frame, f, entry, verdicts)
                    .or_else(|| pinned_may_choice_relief(f, entry, verdicts, scope));
                match relief {
                    Some(residual) => residual,
                    None => return false,
                }
            }
            free => free,
        };
        if !resolution_events_are_discharged(frame, verdict.clone()) {
            return false;
        }
        // CR 616.1 + CR 732.2a: the CANDIDATE-AUTHORITY half is a claim about the FUTURE —
        // the shortcut's remaining repetitions resolve under the board that exists NOW, so a
        // replacement definition that entered play after this frame was captured is invisible
        // to the frame-side discharge above. Fail-closed second discharge against `state`.
        if !std::ptr::eq(*frame, state) && !resolution_events_are_discharged(state, verdict) {
            return false;
        }
    }
    true
}

/// CR 614.1 + CR 616.1: discharge one entry's resolution verdict against the
/// replacement pipeline's own candidate authority, on the board its events were
/// derived on.
///
/// `board` is the frame carrying the resolution. BOTH halves of this discharge
/// are frame-sensitive: the events are the EVENT half and
/// `proposed_event_prompt_cause`'s first argument is the CANDIDATE-AUTHORITY
/// half — it runs `find_applicable_replacements` over that board's replacement
/// population, so handing it a different board would check one frame's events
/// against another frame's candidates.
fn resolution_events_are_discharged(
    board: &GameState,
    verdict: crate::game::resolution_prompt::ResolutionChoiceFreedom,
) -> bool {
    use crate::game::resolution_prompt::ResolutionChoiceFreedom;
    match verdict {
        ResolutionChoiceFreedom::MayPrompt => false,
        ResolutionChoiceFreedom::FreeUnlessReplacements(events) => {
            // CR 616.1: FAIL CLOSED ON AN EMPTY DERIVATION, IN EVERY BUILD.
            //
            // This was a `debug_assert!(!events.is_empty(), ..)` resting on the contract
            // that `probe_resolution` returns `Prompted` for an empty derivation. That
            // contract is real but it lives in ANOTHER module and is not enforceable from
            // here — and `debug_assert!` compiles out of release, where `any()` over an
            // empty slice is `false`, so `!any(..)` would return `true` and discharge the
            // obligation having inspected NOTHING. Fail-open is the single direction this
            // predicate exists to prevent.
            //
            // A REFUSAL, not a panic, is the right shape: every other seam in this module
            // answers an unclassifiable input with "no certificate, no offer", and the
            // refusal is what a test can pin. A `debug_assert!` here could not be covered
            // at all — it aborts the very build tests run in.
            if events.is_empty() {
                return false;
            }
            !events.iter().any(|ev| {
                !crate::game::replacement::proposed_event_prompt_cause(
                    board,
                    ev,
                    crate::game::replacement::replacement_registry(),
                )
                .is_empty()
            })
        }
    }
}

/// CR 732.2a: [`loop_states_cover_modulo_growth`] with an OFFER's published pin slots in
/// scope — the entry point `game::engine`'s bounded-cycle offer uses for both its
/// certification disjunct and its pin-coverage conjunct.
///
/// This wrapper exists so the pins reach the gates through the SINGLE AUTHORITY
/// [`window_scope_from_cover_frames`] rather than through a [`LoopWindowScope`] a caller
/// in another module assembled itself; the scope's fields are private for exactly that
/// reason. Passing `slots` empty is NOT the same as passing no proof: `Some(PinnedChoices
/// { slots: &[] })` still names a proposer whose entries the relief tests, and every such
/// test fails on an empty slot list, so an empty publication is byte-identically as strict
/// as [`LoopWindowScope::unproven`].
pub(crate) fn loop_states_cover_modulo_growth_pinned<'a>(
    prior: &GameState,
    current: &'a GameState,
    proposer: PlayerId,
    slots: &[DecisionSlot],
    touch: &PeriodTouch<'_>,
    verdicts: &mut PeriodVerdicts<'a>,
) -> bool {
    let scope = window_scope_from_cover_frames(
        prior,
        current,
        Some(PinnedChoices { proposer, slots }),
        Some(touch),
    );
    loop_states_cover_modulo_growth_scoped(prior, current, scope, verdicts)
}

/// CR 601.2f + CR 601.2a: the set of card ids this loop window's recorded driving
/// sequence touches — a SUPERSET of the true cast set (only `LoopAction::Recast`
/// genuinely casts, CR 601.2a; `Activate` and `TapLandForMana` do not), which is the
/// CONSERVATIVE direction: over-stating the cast set makes `!ids.contains(..)` false
/// more often ⇒ fewer relieved defs ⇒ more vetoes.
///
/// FAIL-CLOSED ON EMPTY, and this is the whole reason the function exists: an empty
/// `last_loop_action_sequence` means NO RECORDED PROOF, not "this window casts
/// nothing". `Some(vec![])` would assert the latter and relieve EVERY conditioned
/// self-cost static — relief in the forbidden direction. `None` = scan everything.
/// Pinned by `empty_loop_action_sequence_proves_nothing_about_casting`.
///
/// FAIL-CLOSED ON A FOREIGN PERIOD, for the same reason one level up (CR 732.2a). A recorded
/// period is evidence about the seat that recorded it and no one else, so when the caller names
/// a `proposer` only THAT seat's own period is proof of what this window casts. Otherwise an
/// opponent's choice of WHICH CARD TO ACTIVATE would select which soundness relief applies to
/// the proposer's certification — the same "relief in the forbidden direction" the emptiness
/// contract above rules out, arriving through a different door. This became reachable when the
/// bounded mint's step (1b) went seat-relative: before that, a bounded offer could not be minted
/// with any sequence present, so the question never arose.
///
/// `is_some_and`, NOT `is_some`: the proposer-less 2-arg entry
/// [`loop_states_cover_modulo_growth`] builds a `PeriodVerdicts::unproven` container used by the
/// object-growth detection covers in `analysis::loop_check`, which have no proposer to bind. When
/// the container names none, this is byte-identical to the pre-fix behaviour; requiring
/// `Some(proposer)` there would strip relief from that whole class.
fn window_cast_card_ids(state: &GameState, proposer: Option<PlayerId>) -> Option<Vec<CardId>> {
    if proposer.is_some_and(|p| state.loop_period_controller() != Some(p)) {
        return None;
    }
    let ids: Vec<CardId> = state
        .last_loop_action_sequence
        .iter()
        .map(|ctx| ctx.card_id)
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

/// Scoped sibling of [`loop_states_cover_modulo_growth`] — see [`LoopWindowScope`].
/// The `scope` parameter now carries CR 732.2a pin proofs INTO this body: gate (3)
/// skips an entry whose published target [`DecisionSlot`] the offer pinned
/// ([`entry_target_choice_is_pinned`]) and gate (6) discharges a `MayPrompt` that is
/// wholly attributable to a published CR 603.5 gate ([`pinned_may_choice_relief`]), per
/// the EXTENSION POINT's three preconditions. That is this body's own use of the
/// parameter.
///
/// The parameter is ALSO the seam for the SIBLING covers, which build their scope
/// through the single authority [`window_scope_from_cover_frames`] — its third
/// argument is `pinned`, passed `None` at both live sibling call sites today.
///
/// Not carried by the parameter: the PROJECTED conjunct (5) this body passes
/// downstream is derived LOCALLY from `current`'s own driving sequence
/// ([`window_cast_card_ids`]), and the `projected_scope` built for that call
/// deliberately holds `pinned: None` — the projected firewall is a different
/// axis and must not inherit the caller's pins.
pub(crate) fn loop_states_cover_modulo_growth_scoped<'a>(
    prior: &GameState,
    current: &'a GameState,
    scope: LoopWindowScope<'_>,
    verdicts: &mut PeriodVerdicts<'a>,
) -> bool {
    // (1) Board equal modulo the NARROWED projection AND modulo the stack, with the
    // object resource axes STRICT-COMPARED (R5-B1). Project both, clear both stacks
    // and their stack-entry-indexed firing sidecars (the stack is compared separately
    // in (2)), then require full board equality plus loyalty-activation parity plus
    // strict object damage/counter equality.
    let mut pa = project_out_resources(prior);
    let mut pb = project_out_resources(current);
    pa.stack.clear();
    pb.stack.clear();
    pa.stack_trigger_firings.clear();
    pb.stack_trigger_firings.clear();
    if !(loop_states_equal(&pa, &pb)
        && loyalty_activation_counts_match(&pa, &pb)
        && object_resource_axes_match(prior, current))
    {
        return false;
    }

    // (2) Stack coverability: order-preserving bottom-up embedding + strict growth
    // confined to places already occupied in `prior` (CR 608.1 / CR 405.5 LIFO freeze).
    let prior_stack = normalized_stack_entries(prior);
    let cur_stack = normalized_stack_entries(current);
    if !stack_covers(&prior_stack, &cur_stack) {
        return false;
    }

    // (3) Every grown place is a mandatory, no-ordering-input triggered ability.
    // Iterate the ORIGINAL current-stack entries (so the mid-construction firewall
    // sees real stack-entry ids) and check each whose normalized kind strictly grew.
    // Fail-closed: this predicate only answers about a `current` the container holds.
    let Some(f_current) = verdicts.frame_ix(current) else {
        return false;
    };
    for (orig, norm) in current.stack.iter().zip(cur_stack.iter()) {
        // CR 732.2a EXTENSION POINT (see item 6's block): a slot the OFFER publishes is
        // a SPECIFIED choice, not a free one, so its announcement-time target input is
        // no longer player ordering input.
        //
        // Deliberately NOT frozen-filtered: the PLACEMENT RULE keeps the frozen skip in
        // item (6) alone, because items (4)/(5) are what establish the premise that skip
        // consumes — reading it here would consume a premise not yet proved.
        if entry_target_choice_is_pinned(current, f_current, orig, verdicts, scope) {
            continue;
        }
        let cn = cur_stack.iter().filter(|e| *e == norm).count();
        let pn = prior_stack.iter().filter(|e| *e == norm).count();
        if cn > pn && !stack_entry_has_no_ordering_input(current, orig) {
            return false;
        }
    }

    // (4) On-stack fail-closed resource-read guard: NO entry on `current`'s stack may
    // carry an AST that reads a still-projected axis (player monotone resources +
    // journals). Object-axis readers pass — their drift breaks gate (1) instead.
    // The closure is the ONLY body change item (4) takes: it counts the scans so the
    // PLACEMENT RULE ("the frozen skip lives in item (6) and nowhere earlier") has an
    // assertable surface instead of an argued one. The population is the UNEXEMPTED
    // current stack — `frozen_ids` is deliberately not read here.
    if current.stack.iter().any(|e| {
        verdicts.note_conjunct4_scan();
        stack_entry_reads_projected_resource(e)
    }) {
        return false;
    }

    // (5) Off-stack fail-closed fire-time condition guard (the second read surface).
    // CR 601.2f: `cast_ids` is bound BEFORE `projected_scope` so NLL keeps the borrow
    // live across the call (`LoopWindowScope::cast_card_ids` is `Option<&'a [CardId]>`).
    //
    // SITE E (CR 732.2a): the window's cast-set proof is scoped to the seat this container is
    // bound to, so a period recorded by ANOTHER seat cannot select which relief applies here.
    // `verdicts.proposer()` is `None` for the proposer-less 2-arg entry, where this stays
    // byte-identical to the unscoped read.
    let cast_ids = window_cast_card_ids(current, verdicts.proposer());
    // All four fields written explicitly — no functional-update base, so there is no
    // `LoopWindowScope<'static>` -> `LoopWindowScope<'_>` variance question to reason
    // about, and a future FIFTH field is a compile error that forces a decision rather
    // than a silent default. The other three stay at their `unproven()` values: 2b's
    // axis is `projected`, and the sibling proofs belong to the sibling covers.
    let projected_scope = LoopWindowScope {
        phase_invariant: None,
        sole_driver: None,
        pinned: None,
        cast_card_ids: cast_ids.as_deref(),
        // DELIBERATE, same rationale as the `pinned: None` above: the projected firewall is
        // a different axis and must not inherit the caller's period proof.
        period: None,
    };
    if fire_time_conditions_read_projected_resource_scoped(current, projected_scope) {
        return false;
    }

    // (6) CR 732.2a + CR 608.2d: resolution-time choice gate, fail-closed, over
    // EVERY current-stack entry — the extrapolation models future resolutions the
    // window never observed (grown kinds) and re-runs observed kinds in states that
    // differ on projected axes, where a resolver's choice surface (e.g. proliferate
    // eligibility over player counters, CR 701.34a) can open a prompt that the
    // AST-level item-4 scan cannot see. Verdicts come from the ability_scan
    // classifier (pure fact-producers — rejection is decided ONLY here);
    // FreeUnlessReplacements additionally requires the CR 616.1 environmental
    // guard below, for exactly the event classes its payload names. THIS block is the
    // single gate seam for resolution-choice rejection (item 3 is untouched and gates
    // a different fact — announcement-time ordering input). Perf: O(stack × AST) +
    // O(objects × defs) via the guard — same order as items (4)/(5).
    //
    // EXTENSION POINT — pinned fixed choices (CR 732.2a): a shortcut proposal MAY
    // pre-specify choices in advance ("always choose permanent P"); only
    // CONDITIONAL actions are forbidden. A future consumer may treat a MayPrompt
    // entry as choice-free when a pin covers it, PROVIDED: (a) the pin is a
    // STATE-INDEPENDENT designation whose option remains legal at every iteration
    // of the growing state (never "the newest copy"); (b) cover-modulo-growth
    // still holds under the pinned outcomes; (c) only the acting player's own
    // choices are pinnable — opponent-choice entries remain rejectors unless EVERY
    // option preserves the certificate (the win stays forced per the
    // CR 104.2a-grounded winner predicate). Plug pins in at THIS seam as an
    // additional input; do not rewire the classifiers or spread the decision.
    //
    // PINS ARE PLUGGED IN HERE (`scope.pinned`, minted by the single authority
    // `game::engine::bounded_cycle_pin_slots`). Precondition (a) holds by construction:
    // the pins that channel carries are SEAT designations and `MayChoice` designations,
    // both state-independent (never "the newest copy"). A seat designation now has TWO
    // spellings and (a) holds for both: `TargetPin::Player` is the CR 115.10a CHOICE class,
    // while a CR 601.2c TARGET-class seat is
    // `Scheduled(TargetSchedule::Constant(Ranking::one(AnnouncementSubject::Seat(..))))` —
    // one entry, selected without reading the iteration index, so it too can never denote
    // "the newest copy". The split changes WHICH AUTHORITY judges a seat's legality, never
    // whether the designation is state-independent, which is all this precondition asks.
    // (This module reads no pin VARIANT at all — every `TargetPin::` occurrence in it is
    // prose — so no relief verdict can move with the spelling.) Precondition (c) is NOT taken on
    // trust from the mint site: [`pinned_may_choice_relief`] re-runs the mint's own
    // per-entry acceptance test — controller conjunct included — for THIS entry, so the
    // relief predicate is the mint predicate rather than a coarser sibling of it.
    // Precondition (b) is why relief is not a `continue`: the entry's RESIDUAL verdict
    // (its classification with the published CR 603.5 gate discharged) re-enters the same
    // gating an unpinned entry gets, so `FreeUnlessReplacements` still arms the
    // CR 616.1 environmental guard below for the classes it names — a pinned target/"may"
    // says nothing about whose life- or draw-event replacements might prompt.
    //
    // CR 608.1 + CR 732.2a: an entry the certified window proves FROZEN — same id at the
    // same index in every window frame and in `current` — neither announced nor resolved in
    // the described sequence, so it makes no choice there. THE FROZEN SKIP LIVES HERE AND
    // NOWHERE EARLIER: items (2)/(4)/(5) are what establish the premises it consumes, and
    // each of them returns on failure strictly above this loop, so by the time control
    // arrives the premises are discharged facts about `(prior, current)` rather than
    // assumptions about this predicate's own eventual answer.
    for entry in &current.stack {
        if scope
            .period
            .is_some_and(|t| t.frozen_ids.contains(&entry.id))
        {
            verdicts.note_conjunct6_frozen_skip();
            continue;
        }
        verdicts.note_conjunct6_ask();
        let primary = verdicts.verdict(f_current, entry).primary.clone();
        let verdict = match primary {
            crate::game::resolution_prompt::ResolutionChoiceFreedom::MayPrompt => {
                match pinned_may_choice_relief(f_current, entry, verdicts, scope) {
                    Some(residual) => residual,
                    None => return false,
                }
            }
            free => free,
        };
        if !resolution_events_are_discharged(current, verdict) {
            return false;
        }
    }

    true
}

// ===========================================================================
// PR-7 Phase 4a — offline object-growth loop detection (soundness core).
//
// The object-axis analogue of `loop_states_cover_modulo_growth`: `current`'s
// battlefield = `prior`'s + a set of INERT grown permanents G (Karp–Miller
// ω-cover on the object axis, CR 732.2a), else equal modulo the projected
// monotone resources. Certifies a cover ONLY IF no observer's per-iteration
// behavior can depend on |G| or G's members. OFFLINE: this predicate certifies
// and rejects NOTHING at runtime — it is wired only into the offline classifier
// `analysis::loop_check::detect_loop`. False-negative acceptable; false-positive
// (a wrongful CR 104.2a win) is NOT — every gate fails closed.
// ===========================================================================

/// CR 110.1: absolute-ObjectId battlefield membership. Module-level twin of
/// `board_delta`'s nested helper (the exact set the residual diff computes),
/// shared by the object-growth cover gate. PURE.
fn battlefield_ids(state: &GameState) -> HashSet<ObjectId> {
    state
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield)
        .map(|o| o.id)
        .collect()
}

/// Clone through `flush_layers` so every derived characteristic (live abilities,
/// P/T, keywords, static grants) reflects the current continuous environment
/// before any content compare or firewall scan (§5.3b MAJOR-A: flush ONCE, up
/// front, on both frames — a stale layer state could hide a |G|-scaling grant).
fn flush_clone(state: &GameState) -> GameState {
    let mut clone = state.clone();
    crate::game::layers::flush_layers(&mut clone);
    clone
}

/// CR 732.2a object-axis cover: does `current` cover `prior` by pure inert
/// battlefield growth, with no observer able to read the growth set |G|?
///
/// Mirrors `loop_states_cover_modulo_growth`'s scaffold, relaxing ONLY the board
/// axis (permits strict battlefield growth) and confining that growth to an inert,
/// unobserved class. Returns `true` iff ALL of:
/// 1″. every NON-grown object is content-equal on the §5.2c 136-field partition
///     ([`board_covers`]), each grown id confines to an inert class member already
///     in `prior`, object resource axes strict-match, and every non-object
///     GameState field is strict-equal ([`eq_except_growable`], S3);
/// 2″. every grown object is churn-inert (MAJOR-1, [`grown_objects_are_inert`]);
/// 3″. no live fire-time observer reads the growing class (§5.3a firewall, S5);
/// 4″. no cost surface references the growing class (§5.4 EXHAUSTIVE + the
///     cost-keyword keystone rejectors, CR 732.2a / §6).
pub(crate) fn loop_states_cover_modulo_object_growth(
    prior: &GameState,
    current: &GameState,
) -> bool {
    // §5.3b: flush BOTH clones once, up front, then project out the monotone
    // resources for the board/GameState equality axes.
    let pf = flush_clone(prior);
    let cf = flush_clone(current);
    let mut pa = project_out_resources(&pf);
    let mut pb = project_out_resources(&cf);
    pa.stack.clear();
    pb.stack.clear();

    // P-19: absolute-ObjectId battlefield set-difference. Growth must be PURE —
    // no battlefield object may leave (a shrink is a real board change, not ω-cover).
    let bf_prior = battlefield_ids(&pa);
    let bf_current = battlefield_ids(&pb);
    let grown_ids: HashSet<ObjectId> = bf_current.difference(&bf_prior).copied().collect();
    let shrunk: HashSet<ObjectId> = bf_prior.difference(&bf_current).copied().collect();
    if !shrunk.is_empty() {
        return false;
    }
    // Constant-depth (no growth) is the shipped `loop_states_cover_modulo_growth`
    // / `loop_states_equal_modulo_resources` job; this predicate is STRICT growth only.
    if grown_ids.is_empty() {
        return false;
    }

    // (1″) Board equal modulo the inert growth set + all non-object GameState fields.
    if !(board_covers(&pa, &pb, &grown_ids)
        && object_resource_axes_match(prior, current)
        && loyalty_activation_counts_match(&pa, &pb)
        && eq_except_growable(&pa, &pb, &grown_ids))
    {
        return false;
    }

    // (2″) Every grown object is churn-inert (scanned on the FLUSHED current so
    // layer-derived P/T / abilities / keywords are realized).
    if !grown_objects_are_inert(&cf, &grown_ids) {
        return false;
    }

    // (3″) No live fire-time observer reads the growing class (§5.3a, S5).
    // `None` class context: the offline object-growth path (`detect_loop`) has no proven
    // class set to gate ETB matchers against, so the firewall keeps its conservative veto on
    // every observer whose relief is class-keyed (byte-identical to pre-gate behavior).
    // ⚠ The window scope is NOT class-keyed: CR 117.1b (`sole_driver`) and CR 510.2 / CR 506.1
    // (`phase_invariant`) relief IS live here, so this OFFLINE classifier can now emit
    // certificates where it previously vetoed. That is the one seam this phase can widen.
    //
    // NO AUTOMATED DETECTOR WATCHES IT, stated plainly rather than implied. The
    // `cargo combo-verify` row-for-row diff was measured at ZERO sensitivity to this seam:
    // forcing this predicate to `return true` — its most restrictive possible behavior —
    // moved no corpus row at all. That zero is NOT an untested instrument: the same
    // invocation, with `detect_loop` forced to `return None`, moves 10 of the 54 rows
    // (13 confirmed / 0 failed becomes 3 confirmed / 10 failed), so the row diff can and
    // does register change. It is discriminating but not total — 3 confirmed rows survive
    // that mutation, i.e. they are certified by a path that never consults `detect_loop`.
    // WHY every row is insensitive to THIS seam has NOT been measured, and no mechanism is
    // asserted here: the liveness control establishes that the instrument works, not why
    // the seam figure is zero.
    //
    // What bounds the SHIPPED blast radius is not a detector but compile-time exclusion of
    // the CALLERS: `loop_states_cover_modulo_object_growth`'s only non-test caller is
    // `detect_loop`, whose only non-test callers live in `analysis::corpus`, which is
    // `#[cfg(any(test, feature = "combo-verify"))]` — and `combo-verify` is non-default
    // (the crate manifest declares no `default` feature at all). Precisely: `detect_loop`
    // itself still compiles into the default lib; nothing in a default build CALLS it.
    // The `cfg(test)` unit call sites of `loop_states_cover_modulo_object_growth` in this
    // file's own `mod tests` are what exercise this line at all; `cargo combo-verify`
    // remains worth running as corroboration, but it is NOT evidence about this seam.
    if fire_time_conditions_read_growing_class_scoped(
        &cf,
        None,
        window_scope_from_cover_frames(&pa, &pb, None, None),
    ) {
        return false;
    }

    // No current-stack entry reads the growing class. Both compared frames sit at a
    // clean priority window (empty projected stacks), so this is normally vacuous,
    // but stays closed under future sampling changes.
    if cf.stack.iter().any(stack_entry_reads_growing_class) {
        return false;
    }

    // (4″) No cost surface references the growing class (§5.4 + §6 keystone).
    if cost_surface_references_growing_class(&cf) {
        return false;
    }

    true
}

/// CR 110.1: two permanents are the same fodder class iff their full content is
/// equal MODULO `tapped` (a convoke/affinity loop taps one fodder member and
/// reproduces another untapped — same class, different tap state). Routes through
/// [`object_content_eq`] so the `_gameobject_partition_is_total` guard
/// (game_object.rs) governs the fodder field set — no hand-rolled field list. This
/// single point keeps the fodder compare honest as `GameObject` grows.
#[cfg_attr(not(test), allow(dead_code))] // 4d-ii wires the live/offline caller; 4d-i exercises via unit tests.
fn fodder_content_eq(a: &GameObject, b: &GameObject) -> bool {
    let mut probe = a.clone();
    probe.tapped = b.tapped;
    crate::types::game_state::object_content_eq(&probe, b)
}

/// Does `id` name a member of the fodder class in `state`? Content-derived (via
/// [`fodder_content_eq`]), NOT ObjectId — fodder tokens are not id-stable (a
/// reproduced token gets a fresh id; a tapped one keeps its id but flips `tapped`).
#[cfg_attr(not(test), allow(dead_code))] // 4d-ii wires the live/offline caller; 4d-i exercises via unit tests.
fn is_fodder(state: &GameState, id: &ObjectId, class: &GameObject) -> bool {
    state
        .objects
        .get(id)
        .is_some_and(|o| fodder_content_eq(o, class))
}

/// CR 110.1 / CR 732.2a: the winning controller's *tapped* fodder-class members —
/// the objects forming the visible "∞ pile" for an accepted object-growth loop
/// shortcut. Filters `state.battlefield` to permanents that `controller` controls,
/// are tapped, and match the fodder `class` by content (via [`fodder_content_eq`]).
///
/// Raw-vs-raw content compare is exact here: the fodder class is inert
/// (`object_content_eq` omits summoning-sickness / timestamp / entered-this-turn),
/// so no projection is needed. Only *tapped* members are the pile: a convoke/affinity
/// loop taps the fodder to pay, so the ever-growing tapped multiset is what the
/// display should show as ∞.
pub(crate) fn tapped_fodder_members(
    state: &GameState,
    controller: PlayerId,
    class: &GameObject,
) -> BTreeSet<ObjectId> {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| (id, o)))
        .filter(|(_, o)| o.controller == controller && o.tapped && fodder_content_eq(o, class))
        .map(|(id, _)| *id)
        .collect()
}

/// CR 110.1 / CR 732.2a: the fodder-axis board cover. Partitions the battlefield by
/// [`fodder_content_eq`] into a STABLE-ENGINE and a FODDER part:
///  * STABLE-ENGINE (non-fodder objects, ALL zones): id-keyed content equality via
///    [`objects_content_eq`]. This is REQUIRED, not redundant: `impl PartialEq for
///    GameState` compares only `objects.len()` (game_state.rs), so the caller's
///    `eq_except_growable` (which reuses that PartialEq) is BLIND to a stable-engine
///    content drift (tap / counter / attachment / move). This `object_content_eq`
///    compare is the SOLE authority for it — exactly as the object-growth
///    `board_covers` is the sole authority for its non-grown partition.
///  * FODDER (content == class modulo tapped): a tapped-split multiset cover (the
///    convoke/affinity loop taps one fodder member and reproduces another):
///      - `untapped_fodder(current) >= untapped_fodder(prior)` (B1 — untapped
///        reproduction preserved; a draining loop is not a sustainable ω-cover), and
///      - `total_fodder(current) > total_fodder(prior)` (STRICT object growth — this
///        predicate, like [`loop_states_cover_modulo_object_growth`], certifies
///        growth only, never a constant-depth loop).
///
/// Fodder INERTNESS is deliberately NOT checked here — it is the single
/// responsibility of the caller's `grown_objects_are_inert` (mirroring how the
/// object-growth `board_covers` leaves inertness to that same helper), so the
/// F-B7 discriminator stays non-vacuous.
#[cfg_attr(not(test), allow(dead_code))] // 4d-ii wires the live/offline caller; 4d-i exercises via unit tests.
fn board_covers_modulo_fodder(
    prior: &GameState,
    current: &GameState,
    fodder_class: &GameObject,
) -> bool {
    // STABLE-ENGINE partition: strip fodder from BOTH frames, require id-keyed content
    // equality on the remainder (all zones). Sole authority for stable content drift.
    let stable =
        |state: &GameState| -> im::HashMap<ObjectId, GameObject, rustc_hash::FxBuildHasher> {
            state
                .objects
                .iter()
                .filter(|(_, o)| !fodder_content_eq(o, fodder_class))
                .map(|(id, o)| (*id, o.clone()))
                .collect()
        };
    if !crate::types::game_state::objects_content_eq(&stable(prior), &stable(current)) {
        return false;
    }

    // FODDER partition: tapped-split multiset cover.
    let fodder_split = |state: &GameState| -> (usize, usize) {
        let mut untapped = 0usize;
        let mut total = 0usize;
        for id in &state.battlefield {
            if let Some(o) = state.objects.get(id) {
                if fodder_content_eq(o, fodder_class) {
                    total += 1;
                    if !o.tapped {
                        untapped += 1;
                    }
                }
            }
        }
        (untapped, total)
    };
    let (prior_untapped, prior_total) = fodder_split(prior);
    let (current_untapped, current_total) = fodder_split(current);
    // B1: untapped reproduction preserved.
    if current_untapped < prior_untapped {
        return false;
    }
    // STRICT growth only (mirror of the object-growth `grown_ids.is_empty()` reject).
    current_total > prior_total
}

/// CR 732.2a fodder-axis cover: does `current` cover `prior` by pure inert,
/// unobserved tapped-fodder growth (the convoke/affinity Sprout-Swarm shape)? A
/// near-clone of [`loop_states_cover_modulo_object_growth`], swapping the board
/// sub-predicate for the tapped-split multiset ([`board_covers_modulo_fodder`]) and
/// DROPPING the `cost_surface_references_growing_class` firewall (§6 keystone): the
/// fodder path is for the 4d-ii DRIVEN classifier that pays the real convoke+affinity
/// cost on a clone and measures sustainability empirically, so the offline "models no
/// cost ⇒ reject any board-scaling cost keyword" rejector does NOT apply here.
/// `detect_loop` keeps the firewall (it stays on the object-growth predicate — T-B1i
/// pins this). LIVE, not tree-scoped: called twice at `game::engine`'s `cover_ok` in
/// `try_offer_object_growth_shortcut`, itself invoked from `apply()`'s empty-stack offer
/// hook — so a change here can move a SHIPPED offer verdict. (`elimination_bounds` is the
/// genuinely tree-scoped one; this is not.)
///
/// `fodder_class` is a CONTENT authority (a representative `&GameObject`), compared
/// LIVE each call via [`fodder_content_eq`] (modulo tapped) — not latched by
/// ObjectId, because fodder tokens are not id-stable. Covers any inert fungible token
/// class (Saproling, Elf Warrior, Thopter, …), so it builds for the class not a card.
pub(crate) fn loop_states_cover_modulo_fodder_growth(
    prior: &GameState,
    current: &GameState,
    fodder_class: &GameObject,
) -> bool {
    let pf = flush_clone(prior);
    let cf = flush_clone(current);
    let mut pa = project_out_resources(&pf);
    let mut pb = project_out_resources(&cf);
    pa.stack.clear();
    pb.stack.clear();

    // Excluded set = ALL fodder ids in BOTH projected frames (the drifting/growing
    // pile). Unlike the object-growth `bf_current − bf_prior` add-set, an existing
    // untapped fodder member keeps its id but flips `tapped`, so it must be excluded
    // from strict eq and handled by the multiset compare.
    let all_fodder: HashSet<ObjectId> = pa
        .battlefield
        .iter()
        .chain(pb.battlefield.iter())
        .copied()
        .filter(|id| is_fodder(&pa, id, fodder_class) || is_fodder(&pb, id, fodder_class))
        .collect();

    // Tapped-split multiset cover on the fodder partition (B1 + strict growth).
    if !board_covers_modulo_fodder(&pa, &pb, fodder_class) {
        return false;
    }

    // Every fodder member is churn-inert (single inertness authority; scanned on the
    // FLUSHED current so layer-derived P/T / abilities / keywords are realized).
    if !grown_objects_are_inert(&cf, &all_fodder) {
        return false;
    }

    // No live off-stack / on-stack observer reads the growing class. Pass the WHOLE proven
    // fodder class so the firewall's block(1) can skip an ETB observer whose matcher provably
    // excludes EVERY member of it (CR 603.6a). There is deliberately no representative to
    // choose: relief is universally quantified over the class, so no member-selection rule
    // (and no CR 110.5b tiebreak) is needed or sound here. Order-independence: the
    // member-quantified predicates are pure state reads, so `HashSet` iteration order moves
    // only the short-circuit point, never the verdict. The ids are projection-stable, so they
    // resolve against the flushed-current `cf` the firewall scans; an empty set never relieves
    // (the `!is_empty()` guards) → conservative veto preserved.
    // ponytail: O(observers x |G|), short-circuiting on the first non-excluding member. If |G|
    // ever measures hot, hoist the member-independent conjuncts out of the per-member loop.
    let class_members: HashSet<ObjectId> = all_fodder
        .iter()
        .copied()
        .filter(|id| cf.objects.contains_key(id))
        .collect();
    if fire_time_conditions_read_growing_class_scoped(
        &cf,
        Some(&class_members),
        window_scope_from_cover_frames(&pa, &pb, None, None),
    ) {
        return false;
    }
    if cf.stack.iter().any(stack_entry_reads_growing_class) {
        return false;
    }

    // Non-object GameState fields (journals, monarch, delayed triggers, …) + the
    // object COUNT, grown pile stripped. NOTE: `GameState::PartialEq` compares only
    // `objects.len()`, so stable-engine object CONTENT is covered by
    // `board_covers_modulo_fodder`'s `objects_content_eq` above, not here.
    if !eq_except_growable(&pa, &pb, &all_fodder) {
        return false;
    }

    // CR 606.3 fail-safe legality gate (§5): a fodder loop that ALSO re-activates a
    // loyalty ability must not certify. Transparent (all-zero) for the target class.
    if !loyalty_activation_counts_match(&pa, &pb) {
        return false;
    }

    true
}

// ===========================================================================
// PR-7 — preserved-`Generic`-counter growth cover (the proliferate/charge axis).
//
// The counter analogue of `loop_states_cover_modulo_object_growth`: `current`'s
// board equals `prior`'s except that one or more PRESERVED `Generic` object
// counters (charge / burden / oil / …) strictly grew across the cycle — the
// signature of a proliferate loop pumping Pentad Prism's charge counter or The
// One Ring's burden counter (CR 122.1). `Generic` is the ONLY growable axis: the
// monotone counters (+1/+1, loyalty, defense) are already projected out by
// `project_out_resources`, and the remaining preserved counters (stun / shield /
// keyword / time / fade / age / lore) are SBA- or duration-gating, so a loop that
// touches one is making a real board change, not a monotone pump.
// ===========================================================================

/// CR 122.1: direction a candidate loop drives PRESERVED `Generic` object counters
/// (charge / burden / oil) across one cycle. `Generic` is the only growable axis
/// here — see `classify_generic_counter_growth` for the per-type partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CounterGrowthDisposition {
    /// ≥1 `Generic` counter strictly rose and none fell — the ω-cover candidate.
    StrictGrowth,
    /// No `Generic` counter moved — a constant-depth loop, the equality path's job.
    Stable,
    /// Some `Generic` counter fell — an ∞-consume trap; fail-closed reject.
    Consumed,
}

/// CR 122.1: is `ct` a PRESERVED `Generic` object counter — the ONLY growable axis
/// of the counter-growth cover (charge / burden / oil / quest)? This `match` IS the
/// SINGLE-SOURCE per-`CounterType` classification table, WILDCARD-FREE by
/// construction, so a new `CounterType` variant will not compile until it is
/// explicitly classified here. Scoped to the ω-COVER DIRECTION GATE alone
/// (`classify_generic_counter_growth`) — it is NOT the display partition. Sharing one
/// partition between the cover and the ∞ display channel WAS the bug: it made every
/// non-`Generic` beneficial counter loop (+1/+1, loyalty, defense) collapse correctly
/// while rendering no `∞` pill at all. The display and batched-collapse channels use
/// `counter_is_beneficial_materializable` instead, and the two partitions are now
/// deliberately different rather than accidentally shared. Kept in lockstep with
/// `CounterType::is_monotone_loop_resource`, which governs the projection: monotone
/// P/T / loyalty / defense counters are `project_out_resources`'d away, the
/// non-`Generic` preserved counters gate SBAs/durations and so must compare
/// strict-equal, and only `Generic` is a pure pumped marker.
fn generic_counter_is_growable(ct: &CounterType) -> bool {
    match ct {
        // CR 122.1: a `Generic` marker is a pure pumped resource (charge /
        // burden / oil / quest) — the only growable axis of this cover.
        CounterType::Generic(_) => true,
        // CR 122.1a + CR 613.4c / CR 306.5b / CR 310.4c: monotone P/T,
        // loyalty, and defense counters are projected out of loop-equality
        // by `project_out_resources`, so their growth is not this axis.
        CounterType::Plus1Plus1
        | CounterType::Minus1Minus1
        | CounterType::PowerToughness { .. }
        | CounterType::Loyalty
        | CounterType::Defense => false,
        // CR 122.1b/c/d, 702.62a/63a, 702.32a, 702.24a, 714.3: preserved
        // but SBA-/duration-gating (keyword / stun / shield / time / fade /
        // age / lore) — a loop that moves one is a real board change, so it
        // must compare strict-equal, never be equalized away as "growth".
        CounterType::Keyword(_)
        | CounterType::Stun
        | CounterType::Lore
        | CounterType::Time
        | CounterType::Fade
        | CounterType::Age
        | CounterType::Shield
        | CounterType::Finality => false,
    }
}

/// CR 122.1: classify how a cycle drives PRESERVED `Generic` object counters, using
/// the wildcard-free `generic_counter_is_growable` partition.
///
/// `Consumed` takes precedence over `StrictGrowth` (any decrease anywhere ⇒
/// `Consumed`, even if a different counter grew) — fail-closed against a loop that
/// both spends and makes a finite `Generic` counter.
fn classify_generic_counter_growth(
    prior: &GameState,
    current: &GameState,
) -> CounterGrowthDisposition {
    let mut any_growth = false;
    for (id, po) in prior.objects.iter() {
        // A set difference (an object present on only one side) is caught by the
        // downstream `loop_states_equal_modulo_resources` object-set compare; here
        // we only classify counter movement on SHARED objects.
        let Some(co) = current.objects.get(id) else {
            continue;
        };
        for ct in po.counters.keys().chain(co.counters.keys()) {
            if !generic_counter_is_growable(ct) {
                continue;
            }
            let (b, a) = (
                po.counters.get(ct).copied().unwrap_or(0),
                co.counters.get(ct).copied().unwrap_or(0),
            );
            if a < b {
                return CounterGrowthDisposition::Consumed;
            }
            if a > b {
                any_growth = true;
            }
        }
    }
    if any_growth {
        CounterGrowthDisposition::StrictGrowth
    } else {
        CounterGrowthDisposition::Stable
    }
}

/// CR 122.1 + CR 732.2a: the wildcard-free partition of `CounterType`s whose per-cycle
/// growth is a BENEFICIAL persistent artifact materializable N×δ at the CR 500.5 boundary
/// (the batched-collapse path). SEPARATE from `generic_counter_is_growable` (the cover
/// partition, unchanged): the cover only equalizes `Generic` markers, but +1/+1 / loyalty
/// / defense counters are projected out by `project_out_resources` and are equally
/// materializable. A new `CounterType` variant will not compile until classified here.
pub(crate) fn counter_is_beneficial_materializable(ct: &CounterType) -> bool {
    match ct {
        // CR 122.1: pure markers (charge / burden / oil / quest) — beneficial, monotone.
        CounterType::Generic(_) => true,
        // CR 122.1a + CR 613.4c: a +1/+1 counter is beneficial P/T growth.
        CounterType::Plus1Plus1 => true,
        // CR 306.5b: loyalty counters (proliferate-reachable planeswalker growth).
        CounterType::Loyalty => true,
        // CR 310.4c: defense counters (proliferate-reachable battle growth).
        CounterType::Defense => true,
        // CR 704.5f + CR 122.1a: a -1/-1 counter kills via toughness ≤ 0 — a loss axis (SBA),
        // never a beneficial materialization.
        CounterType::Minus1Minus1 => false,
        // CR 122.1a + CR 613.4c: asymmetric / possibly-harmful, sign-dependent, rare —
        // non-materialized (ponytail: upgrade only if a real +X/+Y-counter growth loop appears).
        CounterType::PowerToughness { .. } => false,
        // CR 122.1b/c/d/h + CR 702.32a + CR 702.24a + CR 714.3: SBA-/duration-gating counters
        // (keyword / stun / lore / time / fade / age / shield / finality) — a loop moving one
        // is a real board change, never a beneficial materialization.
        CounterType::Stun
        | CounterType::Lore
        | CounterType::Time
        | CounterType::Fade
        | CounterType::Age
        | CounterType::Shield
        | CounterType::Finality
        | CounterType::Keyword(_) => false,
    }
}

/// CR 122.1 + CR 732.2a: THE per-object counter derivation of an accepted period — the
/// `(ObjectId, CounterType, delta)` triples whose BENEFICIAL-materializable counters strictly
/// grew across it (`current` vs `prior`), feeding BOTH the batched-collapse δ stash AND (projected
/// to `(object, counter)`) the `∞` DISPLAY channel. ONE derivation, two consumers, so the pills
/// and the growth that lands cannot disagree; the display channel used to run its own Generic-only
/// diff, which is why beneficial non-`Generic` loops collapsed without ever rendering `∞`.
/// Partitioned by `counter_is_beneficial_materializable` (`Generic(_)` / +1/+1 / loyalty /
/// defense), deliberately WIDER than the ω-cover's `generic_counter_is_growable`. Iterates the
/// CURRENT side (strict growth ⇒ the grown counter is present in `current`); only SHARED objects
/// contribute (a fresh object is caught by the object-set cover, not this axis).
pub(crate) fn grown_beneficial_counter_deltas(
    prior: &GameState,
    current: &GameState,
) -> Vec<(ObjectId, CounterType, u32)> {
    let mut deltas = Vec::new();
    for (id, co) in current.objects.iter() {
        let Some(po) = prior.objects.get(id) else {
            continue;
        };
        for (ct, &a) in co.counters.iter() {
            if !counter_is_beneficial_materializable(ct) {
                continue;
            }
            let b = po.counters.get(ct).copied().unwrap_or(0);
            if a > b {
                deltas.push((*id, ct.clone(), a - b));
            }
        }
    }
    deltas
}

/// CR 119.3 + CR 732.2a: the per-player life GAIN (`> 0`) across one accepted period
/// (`current` vs `prior`) — the batched-collapse δ source for the life axis. A life LOSS
/// stays a loss/SBA axis (CR 704.5a) and is not returned. Mirrors the counter δ source:
/// snapshot the per-cycle delta once, multiply by the controller-named N at the boundary.
pub(crate) fn grown_life_deltas(prior: &GameState, current: &GameState) -> Vec<(PlayerId, u32)> {
    let mut deltas = Vec::new();
    for after in &current.players {
        let before_life = prior
            .players
            .iter()
            .find(|p| p.id == after.id)
            .map(|p| p.life)
            .unwrap_or(after.life);
        let gained = after.life - before_life;
        if gained > 0 {
            deltas.push((after.id, gained as u32));
        }
    }
    deltas
}

/// CR 122.1: return a clone of `current` with every SHARED object's `Generic`
/// counter counts overwritten by `prior`'s — the projection that lets a strict-
/// `Generic`-growth cover reuse the constant-depth equality path. ONLY `Generic`
/// counts are touched: monotone counters are projected out downstream, and the
/// other preserved counters are left intact so a consumed shield/stun still breaks
/// equality (the `Consumed`/`Stable` gate already rejected pure-`Generic` motion in
/// the wrong direction). Objects present on only one side keep their counters and
/// are caught by the downstream object-set compare.
fn equalize_generic_counters(prior: &GameState, current: &GameState) -> GameState {
    let mut eq = current.clone();
    for (id, co) in eq.objects.iter_mut() {
        if let Some(po) = prior.objects.get(id) {
            co.counters
                .retain(|ct, _| !matches!(ct, CounterType::Generic(_)));
            for (ct, n) in po
                .counters
                .iter()
                .filter(|(ct, _)| matches!(ct, CounterType::Generic(_)))
            {
                co.counters.insert(ct.clone(), *n);
            }
        }
    }
    eq
}

/// CR 122.1 + CR 732.2a: does `current` cover `prior` by pure PRESERVED-`Generic`
/// counter growth — the proliferate/charge (Pentad Prism) and burden (The One
/// Ring) ω-cover shape? Returns `true` iff (i) ≥1 `Generic` object counter strictly
/// grew and none fell across the cycle, and (ii) equalizing those `Generic` counts
/// back to `prior`'s makes the two boards equal-modulo-resources.
///
/// # Fail-closed direction (strict growth ONLY)
///
/// `Stable` (no `Generic` motion) is rejected — a constant-depth loop is the
/// existing `loop_states_equal_modulo_resources` path's job, not this one.
/// `Consumed` (any `Generic` counter fell) is rejected — a loop that spends a
/// finite `Generic` counter is not an unbounded pump but an ∞-consume trap, and
/// the extrapolation would be unsound. Only `StrictGrowth` proceeds.
///
/// # New `Generic`-counter projection axis (bounded by revocability, below)
///
/// This predicate rides the FIREWALL-FREE constant-depth
/// `loop_states_equal_modulo_resources` (which requires normalized-stack EQUALITY),
/// NOT the object-growth cover's stack-clearing Karp–Miller path. It therefore
/// inherits that base's documented dormant-condition extrapolation assumption
/// (a dormant intervening-if / static / replacement reading a projected resource
/// could arm mid-extrapolation). Beyond that inherited surface, `equalize_generic_counters`
/// projects out a `Generic` object-counter axis the base itself does NOT project
/// (the base projects player consumables + monotone object counters only) — so a
/// dormant condition reading a GROWING `Generic` counter (e.g. "as long as ~ has
/// three or more charge counters, …") is a genuinely-new projected-axis observer
/// this predicate introduces. That is sound here not by parity but by the
/// revocability bound below: the sole consequence is an Advantage-classed offer /
/// revocable mark, never a `GameOver`, so any such mis-extrapolation is a
/// declinable / revocable over-claim, not a wrongful game-end.
///
/// # Revocability bound (why an over-claim is safe)
///
/// Both wirings of this predicate — the offline `detect_loop` Advantage
/// certification and the live `interactive_loop_bridge` Path-C capability mark —
/// never crown a `GameOver`. A charge/burden growth loop classifies
/// `WinKind::Advantage` (CR 104.4b: an optional loop is not a draw), so an
/// over-claim is a declinable shortcut OFFER / a revocable unbounded-capability
/// mark, never a wrongful game-end. It is deliberately NOT wired into any
/// Path-A/Path-B (GameOver-capable) seam.
///
/// # General over preserved-`Generic` growth
///
/// The axis is the `Generic` counter class, not one card: Pentad Prism (charge)
/// and The One Ring (burden) are the SAME cover, so One-Ring's growth cover is
/// discharged by this predicate — no per-card sibling needed.
pub(crate) fn loop_states_cover_modulo_counter_growth(
    prior: &GameState,
    current: &GameState,
) -> bool {
    if classify_generic_counter_growth(prior, current) != CounterGrowthDisposition::StrictGrowth {
        return false;
    }
    loop_states_equal_modulo_resources(prior, &equalize_generic_counters(prior, current))
}

/// CR 110.1 + CR 613.1b: the object-axis board cover. Every NON-grown object (the
/// shared-id complement over ALL zones) is content-equal via `object_content_eq`
/// (the §5.2c 136-field partition); every grown battlefield object confines to an
/// inert class member already present in `prior`'s battlefield — the Karp–Miller
/// repetition guarantee (growth of an EXISTING inert class, not a never-observed
/// 0→1 introduction). Absolute ObjectId: `normalize_for_loop` zeroes
/// `next_object_id` but does not renumber existing ids.
fn board_covers(prior: &GameState, current: &GameState, grown: &HashSet<ObjectId>) -> bool {
    // Non-grown content equality: strip grown ids from `current`, then require
    // id-keyed content equality with `prior`. A stray extra object in ANY zone (or
    // a content drift on a shared object) fails the `objects_content_eq` len/all
    // check — fail-safe.
    let current_nongrown: im::HashMap<ObjectId, GameObject, rustc_hash::FxBuildHasher> = current
        .objects
        .iter()
        .filter(|(id, _)| !grown.contains(id))
        .map(|(id, o)| (*id, o.clone()))
        .collect();
    if !crate::types::game_state::objects_content_eq(&prior.objects, &current_nongrown) {
        return false;
    }
    // Inert-class confine: every grown object matches (by content) an inert object
    // already on `prior`'s battlefield.
    grown.iter().all(|gid| {
        let Some(gobj) = current.objects.get(gid) else {
            return false;
        };
        prior.battlefield.iter().any(|pid| {
            prior.objects.get(pid).is_some_and(|pobj| {
                object_is_inert(pobj) && crate::types::game_state::object_content_eq(gobj, pobj)
            })
        })
    })
}

/// CR 732.2a MAJOR-1: is `o` a churn-inert permanent — one whose presence cannot
/// change any observer's per-iteration behavior no matter how many copies exist?
/// Requires: NO functioning triggered / static / replacement definitions (so no
/// CDA P/T either — CDAs are characteristic-defining STATICS, CR 604.3), NO
/// activated ability (an activatable lever the extrapolation cannot bound), NO
/// keywords (a keyword can be an SBA-relevant characteristic or a cost lever), NO
/// counters (CR 704.5: every +1/+1 / -1/-1 / loyalty / stun counter feeds an SBA
/// or P/T), and non-legendary + non-`world` (CR 704.5j/k uniqueness SBAs read
/// them). Fail-safe: any doubt ⇒ not inert ⇒ reject.
fn object_is_inert(o: &GameObject) -> bool {
    o.trigger_definitions.iter_all().next().is_none()
        && o.static_definitions.iter_all().next().is_none()
        && o.replacement_definitions.iter_all().next().is_none()
        && !o
            .abilities
            .iter()
            .any(|a| a.kind == crate::types::ability::AbilityKind::Activated)
        && o.keywords.is_empty()
        && o.counters.is_empty()
        && !o.card_types.supertypes.contains(&Supertype::Legendary)
        && !o.card_types.supertypes.contains(&Supertype::World)
}

/// CR 732.2a MAJOR-1: every grown object is churn-inert.
fn grown_objects_are_inert(current: &GameState, grown: &HashSet<ObjectId>) -> bool {
    grown
        .iter()
        .all(|id| current.objects.get(id).is_some_and(object_is_inert))
}

/// BLOCKER-S3: every NON-object GameState field is strict-equal across the two
/// projected frames. Reuses `impl PartialEq for GameState` wholesale (the
/// `_gamestate_partition_is_total` guard keeps that reuse honest as fields are
/// added): strip the grown ids from both object maps and clear the battlefield
/// ordering + stack (the grown ids live there; those axes are covered by
/// `board_covers` / the stack gate), so PartialEq's `objects.len()` + every other
/// non-object field (delayed-trigger stores, journals, monarch, …) compares the
/// growth-invariant remainder. A hidden per-cycle accumulator here fails the compare.
fn eq_except_growable(pa: &GameState, pb: &GameState, grown: &HashSet<ObjectId>) -> bool {
    let mut a = pa.clone();
    let mut b = pb.clone();
    for id in grown {
        a.objects.remove(id);
        b.objects.remove(id);
    }
    a.battlefield.clear(); // allow-raw-zone: clears a discarded comparison CLONE for loop-cover equality (fn takes &GameState, mutates a local clone) - not a gameplay zone event
    b.battlefield.clear(); // allow-raw-zone: clears a discarded comparison CLONE for loop-cover equality (fn takes &GameState, mutates a local clone) - not a gameplay zone event
    a.stack.clear();
    b.stack.clear();
    // Rebase-adaptation (ONE-SIDED-SAFETY): compare the new upstream scalar
    // `post_replacement_token_substitution_count` here even though upstream's
    // `impl PartialEq for GameState` excludes it. Excluding a COUNT from the cover gate
    // is the fail-DANGEROUS direction (a growing count could let two cycles compare EQUAL
    // → false CR 732.2a certification); COMPARING it is fail-safe. It is provably `None` at
    // every loop sample beat (cleared in effects/mod.rs whenever `waiting_for == Priority`
    // — the sample gate itself), and on the only path that could leave it `Some` it is a
    // DIRECT assignment of a CopyTokenOf substitution's fixed count (constant across a real
    // copy-token loop's iterations), so comparing it can never suppress a legitimate loop's
    // detection. (The self-referential incarnation field `resolution_source_relatch` is the
    // opposite case — it VARIES per iteration at the sample beat, so it MUST stay excluded,
    // like a timestamp; see the `_gamestate_partition_is_total` note.)
    // F1 (PR-7 Phase 4d-ii / P7 v3, ONE-SIDED-SAFETY): compare `last_loop_action_sequence` here
    // even though `impl PartialEq for GameState` excludes it. Excluding a decision context whose
    // elements are loop-INVARIANT (unit-variant ConvokeMode, cross-incarnation-stable CardId,
    // constant controller/from_zone/uses_buyback across a homogeneous period) is the
    // fail-DANGEROUS direction — a HETEROGENEOUS / reordered sequence (alternating uses_buyback /
    // from_zone, or a different activation order) whose board coincidentally covers would compare
    // EQUAL under exclusion and be falsely certified an infinite CR 732.2a shortcut. COMPARING
    // (order-sensitive `Vec` `PartialEq`) catches the differing sequence and rejects. It is `[]`
    // at every non-loop-action sample beat, so this never suppresses a legitimate loop's detection
    // (this IS the sole discriminator — the custom PartialEq omits it).
    a == b
        && a.post_replacement_token_substitution_count
            == b.post_replacement_token_substitution_count
        && a.last_loop_action_sequence == b.last_loop_action_sequence
}

/// CR 732.2a + CR 608.2h + CR 608.2i + CR 608.2j: does this trigger's `execute` body observe the
/// growing class ONLY through a battlefield-entry-ledger condition whose filter PROVABLY
/// cannot count `class_member`? Returns `true` iff so — then the read's value is
/// invariant across the loop's growth and the observer does not observe the loop.
///
/// SOUNDNESS rests on the SAME disjointness premise as
/// `etb_observer_provably_excludes_class` (the GAP-1 doc on this function's caller): the
/// fodder is the only class that changes across the covered cycle, guaranteed IN ORDER by
/// `game::engine::derived_fodder_class` — which also has a second, display-only caller;
/// the soundness-bearing one is inside the fodder-cover arm — then
/// `board_covers_modulo_fodder` at its ONLY call site, which PRECEDES this call. Do not
/// reorder that gate after the firewall.
///
/// WHAT THE ONE-REPRESENTATIVE TEST ESTABLISHES, AND WHAT IT DOES NOT (a measured bound,
/// not a generalisation proof — an earlier draft asserted the generalisation and it was
/// FALSE). Fodder membership is `fodder_content_eq`, which routes through
/// `object_content_eq` (`types/game_state.rs`). That function compares exactly
/// 32 `GameObject` fields and does NOT compare `card_types`, `color` or `keywords`.
/// `BattlefieldEntryRecord` (`types/game_state.rs`) has exactly 8 fields, no
/// `..`: object_id / name / core_types / subtypes / supertypes / colors / keywords /
/// controller.
///   COVERED by the fodder relation:  `name`, `controller`.
///   NOT COVERED:                     `core_types`, `subtypes`, `supertypes`, `colors`,
///                                    `keywords` — and this matcher reads every one of
///                                    them (restrictions.rs:493 type, :502 color,
///                                    :507 keyword).
///   `object_id` differs by construction and feeds exactly one predicate,
///   `FilterProp::Another` (restrictions.rs:514), whose verdict is invariant across
///   fodder members because none of them is the ability source.
/// ⇒ ESTABLISHED: the representative's exclusion carries to every fodder member that
///   agrees with it on those five uncompared record fields.
/// ⇒ NOT ESTABLISHED: that fodder members must so agree. Two objects can be
///   `fodder_content_eq` — hence both in the growing class — while differing in exactly
///   the fields this matcher tests. The residual is a member whose
///   type/subtype/supertype/colour/keyword set diverges under an effect that moves none
///   of the 32 compared fields, against a filter reading the diverged field. That is
///   relief for a class whose later members the observer DOES count — the one direction
///   #4603 forbids — so it is a STATED residual, not an accepted one.
/// ⇒ MEASURED, PER AXIS, EACH COUNT WITH ITS POPULATION PREDICATE. Population: all 60
///   live `QuantityRef::BattlefieldEntriesThisTurn` refs in `data/card-data.json` sha256
///   f6dfbe98… (recursively 68 `Typed` leaves; NONE has an empty `type_filters`).
///   - `keywords`: `FilterProp::WithKeyword` is 0/60 — but that is a PROP count, NOT a
///     `keywords`-axis count. `TypeFilter::Subtype` also reads `record.keywords`
///     (restrictions.rs:452, the CR 702.73a Changeling branch), and 18 of the 79
///     type-filter entries are `Subtype`.
///   - `core_types`: read by the other 61 of the 79 entries — Creature 17, Artifact 11,
///     Permanent 11, Non(Land) 11, Land 9, Planeswalker 2.
///   - `subtypes` + `supertypes`: read by those same 18 `Subtype` entries.
///   - `colors`: `FilterProp::HasColor` is 1/60, LIVE.
///   - filter-level `controller` is 0/60 and IRRELEVANT: `controller` IS one of the 32
///     compared fields, so it cannot diverge inside a fodder class at all.
///
///   ⇒ FOUR of the five uncompared record fields are read VERDICT-BEARINGLY by a live
///   filter on today's pool. THE RESIDUAL IS REACHABLE, NOT LATENT. The fifth,
///   `supertypes`, is argument-read but verdict-inert (its only consumer is gated on the
///   subtype being `Host`, and none of the 18 live subtype values is `Host`);
///   over-stating it as read is the CONSERVATIVE direction. What is NOT measured and NOT
///   excluded is the other half: whether a per-member characteristic-changing effect
///   exists that moves NONE of the 32 compared fields (`name` among them).
///   Undischarged, deliberately. Re-derive if `data/card-data.json` is regenerated.
/// DO NOT restate this as "all fodder members' records differ only in `object_id`". That
/// sentence is false, and it was shipped once already as the closure of a review finding.
///
/// ⛔ ARG-EQUIVALENCE PIN — THE LOAD-BEARING SOUNDNESS PREMISE, AND THE REASON THERE IS
/// NO SEPARATE "is this filter evaluable?" CONJUNCT. This predicate must call
/// `battlefield_entry_matches_filter` with arguments EQUIVALENT to the resolver's own
/// call at game/quantity.rs:3426-3432 (inside `resolve_per_player_scalar`,
/// game/quantity.rs:5354; the whole `BattlefieldEntriesThisTurn` resolver arm is
/// :3411-3436) — same record source, same `filter`, the ability controller for `player`,
/// the same `all_creature_types`, and `Some(<source object id>)`.
///
/// GIVEN THAT, THE INVARIANT IS: this predicate asks THE SAME MATCHER the resolver will
/// ask, about the NEW class member. A `false` verdict therefore means each member the
/// loop creates contributes 0 TO THE TALLY WHATEVER THE TALLY'S ABSOLUTE VALUE IS —
/// invariance under growth, which is all the soundness argument needs. Do NOT restate
/// this as "an unanswerable filter makes the tally a constant 0": restrictions.rs's
/// `ledger_filter_is_evaluable` doc does say that, but restrictions.rs:519-526 documents
/// the exception in the same file — under `TargetFilter::Or` an unsupported leaf turns a
/// LOUD constant 0 into a SILENT PARTIAL COUNT, and `Or` is live in this class (4 of 60
/// refs). Invariance-under-growth is `Or`-proof; constant-0 is not. Relieving an
/// unanswerable filter is therefore CORRECT, not merely harmless, and gating on
/// `ledger_filter_is_evaluable` would refuse a sound relief (measured benefit 0/60,
/// measured cost 0/60). Asserted by `ledger_exclusion_is_precise_and_fail_closed` arms
/// (vi) and (vii). If the argument shapes ever diverge, this pin is what breaks first —
/// do not "simplify" the call by dropping `source.id` or by substituting the scoped
/// player for the controller.
///
/// NOT A VISITOR, deliberately (#4603 error direction): an INCOMPLETE `QuantityRef`
/// collector is unsound HERE, because "every collected read excludes" is vacuously true
/// over a set that missed one. Instead, FOUR fail-closed conjuncts, each of which keeps
/// the conservative veto whenever it cannot prove its half:
///   (0) NO ACTIVATION RESTRICTIONS on this def: `exec.activation_restrictions.is_empty()`.
///       LOAD-BEARING, and conjunct (a) does NOT cover it — `ability_definition_axes`
///       destructures `activation_restrictions: _` (ability_scan.rs:4238), so the scan is
///       BLIND to it and the clone-and-rescan would return `false` even with a
///       class-MATCHING `ActivationRestriction::RequiresCondition` on the same def.
///       Measured cost: ZERO — no trigger `execute` in the card pool carries any
///       (positive control: 3195 on `abilities`).
///   (a) SOLE-SOURCE by single-field clone-and-rescan: clone the def, set
///       `condition = None`, and re-run `ability_definition_reads_sibling_mutable_for_loop`.
///       Only if THAT is `false` is `condition` the def's only sibling read — so no effect
///       body, cost, sub-ability or other field hides a second read this predicate never
///       looked at.
///   (b) SHAPE by a SINGLE-LEVEL pattern match with `_ => false`. No recursion, therefore
///       no totality obligation: a compound (`And`/`Or`/`Not`), an rhs-position read, a
///       non-`QuantityCheck` variant, or a non-`BattlefieldEntriesThisTurn` ref all fall
///       to `_` and KEEP the veto. `rhs` must be `Fixed` so it cannot smuggle a second
///       board read.
///   (c) EXCLUSION delegated verbatim to the ledger's own fire-time matcher
///       `restrictions::battlefield_entry_matches_filter` — the SAME matcher, with the
///       SAME arguments (see the ARG-EQUIVALENCE PIN), that
///       `QuantityRef::BattlefieldEntriesThisTurn` resolves through. NOT
///       `matches_target_filter`: game/quantity.rs:1069-1085 documents that it is not a
///       superset of the ledger matcher (entry-time snapshot vs live object), so its
///       `false` can coexist with a fire-time `true` — relief in the forbidden direction.
///       The resolver's scoped-player test is a separate AND conjunct
///       (game/quantity.rs:3425), so a `false` here excludes the member for EVERY scoped
///       player and no `PlayerScope` resolution is required.
fn execute_ledger_condition_provably_excludes_class(
    exec: &crate::types::ability::AbilityDefinition,
    state: &GameState,
    class_member: ObjectId,
    source: &GameObject,
) -> bool {
    use crate::types::ability::{AbilityCondition, QuantityExpr, QuantityRef};

    // (0) the firewall is BLIND to activation restrictions (ability_scan.rs:4238) —
    // fail closed.
    if !exec.activation_restrictions.is_empty() {
        return false;
    }
    // (a) sole-source by single-field clone-and-rescan.
    let mut probe = exec.clone();
    probe.condition = None;
    if crate::game::ability_scan::ability_definition_reads_sibling_mutable_for_loop(&probe) {
        return false;
    }
    // (b) shape — single level, `_ => false` via let-else.
    let Some(AbilityCondition::QuantityCheck {
        lhs:
            QuantityExpr::Ref {
                qty: QuantityRef::BattlefieldEntriesThisTurn { filter, .. },
            },
        rhs: QuantityExpr::Fixed { .. },
        ..
    }) = exec.condition.as_ref()
    else {
        return false;
    };
    // (c) exclusion — fail-closed if the member is gone from the scanned frame.
    //     ARG-EQUIVALENCE PIN: these five arguments mirror game/quantity.rs:3426-3432.
    let Some(member_obj) = state.objects.get(&class_member) else {
        return false;
    };
    let probe_record = crate::game::restrictions::battlefield_entry_record_for(member_obj);
    // The `std::iter::once` is LOAD-BEARING: it guarantees the iterator is never empty,
    // so `.all()` cannot be vacuously `true` — the classic fail-open shape for an
    // `.all()` guard. Do not "optimise" it away when a real record exists. Both
    // authorities are required because the class member is chosen from `all_fodder` and
    // can be a pre-existing object that never went through `record_battlefield_entry`
    // (so real-records-only would be inert), while a Layer-4 type change can make the
    // live object differ from its genuine entry-time snapshot (so synthesized-only would
    // ignore the real record).
    std::iter::once(&probe_record)
        .chain(
            state
                .battlefield_entries_this_turn
                .iter()
                .filter(|r| r.object_id == class_member),
        )
        .all(|r| {
            !crate::game::restrictions::battlefield_entry_matches_filter(
                r,
                filter,
                source.controller,
                &state.all_creature_types,
                Some(source.id),
            )
        })
}

/// §5.3a firewall (BLOCKER-S1 + S5 + MAJOR-A): does ANY live off-stack fire-time
/// observer read the growing class (the axis-2 `sibling` read)? Scans, on the
/// FLUSHED current: (1) trigger conditions AND `execute` bodies; (2) [S5] EVERY
/// ability def on a functioning battlefield permanent regardless of `kind`; (3)
/// replacement conditions AND bodies; (4) condition-gated statics — condition plus
/// any live continuous modification (default-CONSERVATIVE: no
/// scan_continuous_modification walker exists, and an anthem/P-T grant applies to
/// and scales with the growing class); (5) transient continuous effects; (5b)
/// granted-keyword synthesized triggers; (6) the S3 belt over pending/delayed
/// ability-body stores. Fail-closed on every surface it cannot classify.
fn fire_time_conditions_read_growing_class(
    state: &GameState,
    class_members: Option<&HashSet<ObjectId>>,
) -> bool {
    fire_time_conditions_read_growing_class_scoped(
        state,
        class_members,
        LoopWindowScope::unproven(),
    )
}

/// Scoped sibling of [`fire_time_conditions_read_growing_class`] — see
/// [`LoopWindowScope`]. Reads `scope.phase_invariant` (CR 510.2 / CR 506.1, blocks (1)
/// and (5b)) and `scope.sole_driver` (CR 117.1b, block (2)); every such guard sits
/// inside an `if let Some(..)`, so [`LoopWindowScope::unproven`] still reaches none of
/// them and the 2-arg wrapper stays identity (`scoped_wrappers_are_identity`).
fn fire_time_conditions_read_growing_class_scoped(
    state: &GameState,
    class_members: Option<&HashSet<ObjectId>>,
    scope: LoopWindowScope<'_>,
) -> bool {
    use crate::game::ability_scan as scan;
    // (1) Trigger fire-time conditions (CR 603.4) AND effect bodies.
    for obj in state.objects.values() {
        for active in crate::game::functioning_abilities::active_trigger_definitions(state, obj) {
            let def = active.definition;
            // CR 603.4 / CR 113.6: only a trigger that FUNCTIONS in its source's
            // current zone can fire during the loop and read the growing class.
            // `active_trigger_definitions` does NOT zone-gate (it returns a card's
            // printed triggers in any zone), so a permanent's "another permanent
            // enters" trigger on a card sitting in the library / hand / graveyard
            // (empty `trigger_zones` ⇒ battlefield-only) would be scanned as a live
            // observer of the loop's token creation — a false positive that
            // suppresses the offer (regression test
            // `object_growth_library_observer_does_not_suppress_offer`: Kodama of the
            // East Tree in P0's library). Gate on the SAME zone-of-function predicate
            // the trigger pipeline uses; block (5b)'s `granted_keyword_triggers_in_zone`
            // already applies it.
            if !crate::game::triggers::trigger_definition_functions_in_zone(def, obj.zone) {
                continue;
            }
            // CR 510.2 / CR 506.1: a trigger whose event cannot occur in the window's
            // invariant phase never fires inside the loop, so it does not observe the
            // growing class. Fail-closed: `phase_invariant: None` (the caller proved
            // nothing) keeps the conservative veto.
            if let Some(phase) = scope.phase_invariant {
                if crate::game::triggers::trigger_event_unreachable_in_phase(def, phase) {
                    continue;
                }
            }
            // CR 603.2 / CR 603.6a: an enters-the-battlefield observer whose entry matcher
            // PROVABLY excludes EVERY member of `class_members` never fires on the loop's
            // per-cycle token creation, so it does NOT observe the loop — skip it rather than
            // veto. GAP-1 (soundness + ordering, load-bearing): this is sound only because the
            // fodder is the ONLY class that changed across the covered cycle, guaranteed IN ORDER
            // by (a) `game::engine::derived_fodder_class`'s single-new-battlefield-object rule
            // on the FIRST accept-time frame pair — that fn also has a second, display-only
            // caller; the soundness-bearing one is inside the fodder-cover arm — and (b)
            // `board_covers_modulo_fodder`'s all-zones stable-partition content-equality, at
            // its ONLY call site, on the SECOND cover frame pair, which PRECEDES this firewall
            // call. Do not reorder that gate
            // after the firewall. GAP-2 (block(1)-ONLY, deliberate FAIL-CLOSED residual): only
            // this printed-trigger surface is gated. Block (5b)'s
            // `granted_keyword_triggers_in_zone` (`game/triggers.rs`) CAN synthesize granted ETB
            // triggers carrying matchers; a granted ETB observer disjoint from the fodder stays
            // UN-gated and still conservatively vetoes. That is a scoping choice (fail-closed),
            // not an impossibility claim — the other surfaces (statics/anthems that scale with
            // |G| continuously, activated bodies that fire on activation, pending stores) do not
            // fire on the fodder *entering* via a `valid_card` matcher, so gating them would be
            // unsound.
            if let Some(members) = class_members {
                // CR 603.6a (MagicCompRules.txt:2599): relief requires the entry matcher to
                // provably exclude EVERY member of the growing class, not one representative.
                // The one-representative test was unsound in the ACCEPTING direction: this
                // function's own doc measures that fodder equivalence
                // (`object_content_eq`, `types/game_state.rs`, 32 compared fields) does NOT
                // compare `card_types`, `color` or `keywords`, so two members can differ on
                // exactly the axes a `valid_card` matcher reads.
                // `!is_empty()` is LOAD-BEARING and mirrors the `std::iter::once` guard in
                // `execute_ledger_condition_provably_excludes_class`: an empty set must not
                // make `.all()` vacuously true. NOTE the def-kind test lives INSIDE the closure
                // (`etb_observer_provably_excludes_class` opens with
                // `matches!(def.mode, ChangesZone | ChangesZoneAll)`), and `Iterator::all`
                // on an empty set returns `true` WITHOUT invoking it — so without this
                // guard the `continue` fires for every def of every mode.
                // Order-independence: both member-quantified predicates are pure state
                // reads, so `HashSet` iteration order moves only the short-circuit point,
                // never the verdict.
                if !members.is_empty()
                    && members.iter().all(|&member| {
                        crate::game::triggers::etb_observer_provably_excludes_class(
                            def, state, member, obj.id,
                        )
                    })
                {
                    continue;
                }
            }
            // The trigger CONDITION stays CONSERVATIVE: an intervening-if reads the
            // triggering EVENT (never a growing-class census in scope), so promoting
            // it would not help and only widens the Conservative surface.
            if def
                .condition
                .as_ref()
                .is_some_and(scan::trigger_condition_reads_sibling_mutable)
            {
                return true;
            }
            // P3 (DEFERRED-8): the trigger EFFECT BODY is scanned in LoopFirewall mode
            // (`..._for_loop`), the SAME descending walk block-(2) already applies to
            // battlefield ability bodies (the walk's verdict depends only on def
            // content, not provenance). This is what lets Intruder Alarm's `untap all
            // creatures` (a `SetTapState{Typed{Creature}}` body) relax under the
            // CR 732.2a `Typed`-precision firewall so the canary can OFFER.
            if let Some(exec) = def.execute.as_ref() {
                // CR 608.2h + CR 608.2i + CR 608.2j: a ledger read whose filter provably
                // cannot count the growing fodder has a value invariant across the loop's
                // growth, so this def does not observe the loop — skip it rather than veto.
                // Fail-closed on `class_members: None` (the OFFLINE cover passes `None` and
                // is therefore untouched BY this narrowing — note that the CR 117.1b /
                // CR 510.2 scope guards above are NOT class_members-gated and DO reach it).
                if scan::ability_definition_reads_sibling_mutable_for_loop(exec)
                    && !class_members.is_some_and(|members| {
                        !members.is_empty()
                            && members.iter().all(|&m| {
                                execute_ledger_condition_provably_excludes_class(
                                    exec, state, m, obj,
                                )
                            })
                    })
                {
                    return true;
                }
            }
        }
    }
    // (2) S5: EVERY ability def on a functioning battlefield permanent, any kind.
    // ponytail: this ability-BODY scan is scoped to the battlefield (CR 113.6
    // (MagicCompRules.txt:771): "Abilities of all other objects usually function only
    // while that object is on the battlefield"), so an OFF-battlefield source's
    // |G|-reading activated-ability effect body is unscanned. Reachability is very
    // low and the dominant failure mode — a |G|-scaled monotone pump — keeps the loop
    // unbounded (not a false COVER on unboundedness). Upgrade path: 4a-live / B3 must
    // widen this scan (or gate on activation zone) if a non-battlefield |G|-exact-win
    // source ever becomes reachable. The off-battlefield COST surface is already
    // all-zones (`cost_surface_references_growing_class`); only effect bodies are
    // battlefield-scoped here.
    for obj in state.objects.values() {
        if obj.zone != Zone::Battlefield || obj.is_phased_out() {
            continue;
        }
        if obj.abilities.iter().any(|ability| {
            // CR 117.1b + CR 732.2c: no player but the sole driver receives priority
            // inside the taken shortcut, so a FOREIGN-controlled activated ability
            // cannot be activated during the window and cannot read the growing class.
            // CR 605.3a bounds this: a mana ability is activatable outside the priority
            // rule (while another player casts a spell or activates an ability), so it
            // is NOT relieved and keeps vetoing.
            // PER-ABILITY, never per-object: another surface on the same object (a
            // trigger body, block (1)) must keep vetoing.
            // Fail-closed on `sole_driver: None` (the caller proved nothing).
            let relieved = scope.sole_driver.is_some_and(|driver| {
                // CR 117.1b (MagicCompRules.txt:930) is a statement about ACTIVATED
                // abilities only: "a player may activate an activated ability any time
                // they have priority". A `Spell`/`BeginGame`/`Database`/`Mulligan`-kind
                // def is not reached through the priority rule at all, so a priority-based
                // rationale can say nothing about it and must not relieve it. Same
                // authority `layers.rs` uses to decide "this def is activatable".
                //
                // Measured on `data/card-data.json` (name-keyed object, 35 516 keys,
                // 22 634 `abilities[]` entries): 9 797 of them are NOT `Activated`
                // (`{Spell 9768, BeginGame 27, Mulligan 2}`), so this conjunct is not a
                // no-op. Narrowing to entries that syntactically carry one of the 17
                // `sibling: true` `QuantityRef` tags in `ability_scan.rs`: 1 465
                // entries, 769 of them non-`Activated`. That 1 465/769 pair is an
                // ESTIMATE of the at-risk class, NOT a bound in either direction — the
                // predicate over-counts (a tagged ref need not reach the scan's sibling
                // axis) and under-counts (the scan also flags sibling reads from
                // non-`QuantityRef` surfaces and from every `Axes::CONSERVATIVE` subtree).
                ability.kind == crate::types::ability::AbilityKind::Activated
                    && obj.controller != driver
                    && !crate::game::mana_abilities::is_mana_ability(ability)
                    // CR 602.2 (MagicCompRules.txt:2527): "Only an object's controller (or
                    // its owner, if it doesn't have a controller) can activate its
                    // activated ability UNLESS THE OBJECT SPECIFICALLY SAYS OTHERWISE."
                    // `activator_filter` is that "otherwise": with `All` or `Opponent` the
                    // SOLE DRIVER may activate this FOREIGN permanent's ability while
                    // holding priority inside the window, so `obj.controller != driver`
                    // does not imply unreachability.
                    //
                    // Fail closed on ANY `Some(..)`, never on an enumeration of the two
                    // widening variants. `PlayerFilter` (`types/ability.rs`) carries
                    // dozens of variants and keeps growing; enumerating would make THIS
                    // site assert that every OTHER variant leaves a foreign ability
                    // unreachable — a claim nothing forces anyone to re-verify when the
                    // next variant lands. (Deliberately no count here: a hardcoded
                    // number goes stale silently.) `is_none()` asserts nothing about
                    // any variant: it keys on CR 602.2's own predicate, whether the object
                    // says otherwise AT ALL. Note `player_may_begin_activating`'s
                    // `Some(_) => player == source_controller` catch-all (`casting.rs`)
                    // NARROWS an unmodeled variant to controller-only, so that surface is a
                    // silent under-model of a future widening variant and must not be
                    // inherited here. LATENT on today's pool, deliberately: 45 defs carry
                    // `activator_filter`, 0 of which are growing-class-read candidates.
                    && ability.activator_filter.is_none()
            });
            !relieved && scan::ability_definition_reads_sibling_mutable_for_loop(ability)
        }) {
            return true;
        }
    }
    // (3) Replacement conditions AND bodies (CR 614.1).
    for (_, obj, def) in crate::game::functioning_abilities::active_replacements(state) {
        // CR 614.1 / CR 113.6: `active_replacements` is deliberately all-zones (its
        // callers restrict); a replacement that watches other permanents entering
        // the battlefield functions from the battlefield (or a command-zone emblem),
        // never from a card in the library / hand / graveyard. Scanning an off-zone
        // card's replacement as a loop observer is the same false positive as block
        // (1); restrict to the zones a battlefield-event replacement functions in.
        if !matches!(obj.zone, Zone::Battlefield | Zone::Command) {
            continue;
        }
        if def
            .condition
            .as_ref()
            .is_some_and(scan::replacement_condition_reads_sibling_mutable)
        {
            return true;
        }
        if def
            .runtime_execute
            .as_ref()
            .is_some_and(|a| scan::ability_reads_sibling_mutable(a))
        {
            return true;
        }
        if def
            .execute
            .as_ref()
            .is_some_and(|a| scan::ability_definition_reads_sibling_mutable(a))
        {
            return true;
        }
    }
    // (4) Condition-gated statics (CR 604.1 / CR 613.1) via `iter_all()` (the
    // condition-filtered iterator would hide exactly the dormant defs this exists
    // to catch): condition + any live continuous modification (default-CONSERVATIVE).
    for obj in state.objects.values() {
        if obj.is_phased_out() {
            continue;
        }
        for def in obj.static_definitions.iter_all() {
            // CR 113.6 / CR 604.3: only a static that FUNCTIONS in its source's
            // current zone applies continuously during the loop. `iter_all()` is
            // deliberately condition-agnostic (to catch dormant defs), but it is NOT
            // zone-gated — a battlefield-default static (`active_zones` empty) on a
            // card in the library / hand / graveyard never applies and must not be
            // scanned as a loop observer (same false positive as block (1)). The
            // canonical `static_functions_in_zone` predicate keeps genuinely
            // off-battlefield-functional statics (`active_zones = [Graveyard]`, …)
            // and command-zone emblems while dropping the inert deck/hand cards.
            if !crate::game::functioning_abilities::static_functions_in_zone(obj, def) {
                continue;
            }
            if def
                .condition
                .as_ref()
                .is_some_and(scan::static_condition_reads_sibling_mutable)
            {
                return true;
            }
            // CR 613.1: a live continuous modification vetoes iff it READS a mutable
            // board aggregate (`sibling`) OR a projected player resource
            // (`projected`). BOTH axes (M9): the projected-resource firewall has no
            // modification scan, so this descent is the sole guard against a
            // projected-reading modification (a `SetDynamicPower{Ref(LifeTotal)}`
            // anthem) reaching the ω/drain cover.
            if def.modifications.iter().any(|m| {
                scan::continuous_modification_reads_sibling_mutable(m)
                    || scan::continuous_modification_reads_projected_resource(m)
            }) {
                return true;
            }
        }
    }
    // (5) Transient continuous effects (duration + gating condition, CR 604.1).
    for tce in &state.transient_continuous_effects {
        if scan::duration_reads_sibling_mutable(&tce.duration) {
            return true;
        }
        if tce
            .condition
            .as_ref()
            .is_some_and(scan::static_condition_reads_sibling_mutable)
        {
            return true;
        }
    }
    // (5b) Runtime-granted keyword synthesized triggers (CR 603.4).
    for obj in state.objects.values() {
        if obj.is_phased_out() {
            continue;
        }
        for def in crate::game::triggers::granted_keyword_triggers_in_zone(state, obj) {
            // CR 510.2 / CR 506.1: same phase-unreachability relief as block (1). The
            // guard is per-`def` and applies to any trigger definition, however it was
            // produced. Fail-closed on `phase_invariant: None`.
            if let Some(phase) = scope.phase_invariant {
                if crate::game::triggers::trigger_event_unreachable_in_phase(&def, phase) {
                    continue;
                }
            }
            if def
                .condition
                .as_ref()
                .is_some_and(scan::trigger_condition_reads_sibling_mutable)
            {
                return true;
            }
            if def
                .execute
                .as_ref()
                .is_some_and(|a| scan::ability_definition_reads_sibling_mutable(a))
            {
                return true;
            }
        }
    }
    // (6) S3 belt — pending/delayed ability-body stores. Both compared frames sit at
    // a clean priority window where these are normally empty; a non-empty store
    // carries a deferred ability body that could read |G|, so reject conservatively.
    if !state.delayed_triggers.is_empty()
        || !state.deferred_triggers.is_empty()
        || state.pending_trigger.is_some()
        || state.pending_trigger_order.is_some()
        || !state.epic_effects.is_empty()
    {
        return true;
    }
    false
}

/// §5.3a: does a stack entry's AST read the growing class (axis-2 `sibling`)?
/// Delegates to the axis-2 accessors over the embedded ability plus the
/// trigger-level intervening-if (CR 603.4). `KeywordAction` has no AST ⇒ fail
/// closed; a permanent `Spell { ability: None }` reads nothing (its resolution
/// changes the board and breaks `board_covers` anyway).
fn stack_entry_reads_growing_class(entry: &StackEntry) -> bool {
    use crate::game::ability_scan as scan;
    if let StackEntryKind::TriggeredAbility {
        condition: Some(condition),
        ..
    } = &entry.kind
    {
        if scan::trigger_condition_reads_sibling_mutable(condition) {
            return true;
        }
    }
    match entry.ability() {
        Some(ability) => scan::ability_reads_sibling_mutable(ability),
        None => matches!(entry.kind, StackEntryKind::KeywordAction { .. }),
    }
}

/// §5.4 (BLOCKER-S2 + FINDING-2 + §6 keystone): does ANY cost surface reference the
/// growing class? ONE predicate over EVERY cost surface on the FLUSHED current:
/// (1) the cost-KEYWORD family — a board/graveyard-referencing cost reducer or
/// tap/sacrifice aggregate (Affinity/Convoke/Crew/Delve/Emerge/…) on ANY object (a
/// recast loop's keyword rides an off-battlefield card), printed or granted;
/// (2) the STATIC cost surface (`StaticDefinition::mode`) via the EXHAUSTIVE
/// `StaticMode` scan (CR 601.2f) — the cost-modification statics carry a
/// `dynamic_count: Option<QuantityRef>` ("for each X you control", NOT a fixed
/// `ManaCost`), plus the `AbilityCost`-bearing and keyword-granting cost variants;
/// (3) the object-level `additional_cost`; (4) the full ability TREE's activation
/// costs — the top-level `cost` plus every nested `sub_ability`/`else_ability`/
/// `mode_abilities` cost — each via the EXHAUSTIVE `AbilityCost` scan (Finding-2, NO
/// `_`). CR 732.2a keystone: the cost-affordability that the `ResourceVector` cannot
/// model. Each surface is fail-closed on anything it cannot classify.
fn cost_surface_references_growing_class(state: &GameState) -> bool {
    use crate::game::ability_scan as scan;
    for obj in state.objects.values() {
        // CR 601.2f / CR 602.5a / CR 113.6: a cost surface is only a live loop
        // affordability factor where it can actually be PAID. A card in the LIBRARY
        // is never a cost source — a recast loop returns its spell to hand /
        // graveyard / exile (never the library), and no activated ability or cast
        // cost functions from the library. Scanning a bystander deck card's convoke /
        // affinity / delve keyword there is the same all-zones false-reject class as
        // the observer firewalls above (a Commander deck's library holds ~90 cards).
        // The off-battlefield HAND surface is deliberately kept — the loop's own
        // recast spell rides there (see `object_growth_r_e_cost_keyword_family`).
        if obj.zone == Zone::Library {
            continue;
        }
        // (1) printed cost-keyword family.
        if obj
            .keywords
            .iter()
            .any(scan::keyword_cost_reads_growing_class)
        {
            return true;
        }
        // (1b) granted cost-keyword family (AddKeyword / AddKeywordWithDerivedCost)
        // + (2) the STATIC cost surface (`StaticDefinition::mode`, CR 601.2f). A
        // static cost-mod only bites where the static FUNCTIONS (CR 113.6) — gate it
        // so a non-functioning static (e.g. on a hand card) is not read as a live
        // cost modifier.
        for def in obj.static_definitions.iter_all() {
            if !crate::game::functioning_abilities::static_functions_in_zone(obj, def) {
                continue;
            }
            if def
                .modifications
                .iter()
                .any(scan::modification_grants_growing_cost_keyword)
            {
                return true;
            }
            if static_mode_references_growing_class(&def.mode) {
                return true;
            }
        }
        // (3) object-level additional cost surface (EXHAUSTIVE AbilityCost).
        if let Some(additional) = &obj.additional_cost {
            if additional_cost_references_growing_class(additional) {
                return true;
            }
        }
        // (4) the full ability TREE's activation costs — top-level plus nested
        // sub/else/mode abilities (each `AbilityDefinition` carries its own `cost`).
        if obj
            .abilities
            .iter()
            .any(ability_tree_cost_references_growing_class)
        {
            return true;
        }
    }
    false
}

/// §5.4 + CR 601.2f: EXHAUSTIVE no-`_` scan of a `StaticDefinition::mode`'s cost
/// surface. Every cost-carrying variant routes its dynamic component fail-closed;
/// every non-cost variant (or fixed-cost variant) binds read-free. A new
/// `StaticMode` variant fails to compile here until it is classified.
fn static_mode_references_growing_class(mode: &crate::types::statics::StaticMode) -> bool {
    use crate::game::ability_scan::{
        ability_cost_references_sibling_mutable as cost_reads,
        keyword_cost_reads_growing_class as kw_reads,
        quantity_ref_references_sibling_mutable as qty_reads,
    };
    use crate::types::statics::StaticMode;
    match mode {
        // CR 601.2f: cast/ability cost adjustments carry a dynamic multiplier
        // `dynamic_count: Option<QuantityRef>` ("for each X you control"). An
        // `ObjectCount` of the grown class reads |G|, so route it fail-closed — for
        // BOTH directions: `Raise`+`ObjectCount` is the false-positive-∞ case, and
        // `Reduce` is the §6 keystone-REJECT case. `amount` (a fixed `ManaCost`) and
        // every other field are read-free.
        StaticMode::ModifyCost { dynamic_count, .. }
        | StaticMode::ReduceAbilityCost { dynamic_count, .. } => {
            dynamic_count.as_ref().is_some_and(qty_reads)
        }
        // CR 118.8 / CR 118.9 / CR 601.2f: variants carrying an `AbilityCost` payment
        // — the additional/alternative cast cost — route it through the exhaustive
        // `AbilityCost` scanner (a `PayLife`/`ManaDynamic`/… reading `ObjectCount`
        // reads |G|).
        StaticMode::ImposeAdditionalCost { cost, .. }
        | StaticMode::AlternativeKeywordCost { cost, .. }
        | StaticMode::CastWithAlternativeCost { cost, .. } => cost_reads(cost),
        // CR 118.9 + CR 601.2f: cast-permission riders carrying an optional
        // `AbilityCost` payment (Bolas's Citadel's `alt_cost`, the graveyard/exile
        // permissions' `extra_cost`). Same fail-closed AbilityCost routing so a
        // board-scaling rider cannot hide behind a permission grant.
        StaticMode::TopOfLibraryCastPermission { alt_cost, .. } => {
            alt_cost.as_ref().is_some_and(cost_reads)
        }
        StaticMode::GraveyardCastPermission { extra_cost, .. }
        | StaticMode::ExileCastPermission { extra_cost, .. } => {
            extra_cost.as_ref().is_some_and(|c| cost_reads(&c.cost))
        }
        // CR 702.51a etc.: grants a keyword to the controller's cast spells. If that
        // keyword is a board-reading cost keyword (convoke, …) the grant is itself a
        // |G| cost surface — route it through the keyword classifier (the StaticMode
        // analogue of `modification_grants_growing_cost_keyword`).
        StaticMode::CastWithKeyword { keyword } => kw_reads(keyword),

        // Non-cost (or fixed-cost) variants — read-free, listed exhaustively (NO `_`).
        // `ReduceActionCost`/`DefilerCostReduction` carry only a fixed generic
        // reduction; `CantPayCost` is a payment PROHIBITION, not a payable cost; the
        // cast-permission `frequency`/`play_mode`/`cost`(mode-only) fields are not
        // board reads.
        StaticMode::Continuous
        | StaticMode::DamageNotRemovedDuringCleanup
        | StaticMode::CantAttack
        | StaticMode::CantBlock
        | StaticMode::CantAttackOrBlock
        | StaticMode::CantBecomeSuspected
        | StaticMode::MaxAttackersEachCombat { .. }
        | StaticMode::MaxBlockersEachCombat { .. }
        | StaticMode::CantBeTargeted
        | StaticMode::CantBeCast { .. }
        | StaticMode::CantBeActivated { .. }
        | StaticMode::CantSearchLibrary { .. }
        | StaticMode::RestrictLibrarySearchToTop { .. }
        | StaticMode::ControlPlayersDuringOwnLibrarySearch { .. }
        | StaticMode::CantCauseSacrificeOrExile { .. }
        | StaticMode::CastWithFlash
        | StaticMode::GrantsExtraVote
        | StaticMode::GrantsExtraVillainousChoice
        | StaticMode::ReduceActionCost { .. }
        | StaticMode::ModifyActivationLimit { .. }
        | StaticMode::ActivateAsInstant { .. }
        | StaticMode::CantPayCost { .. }
        | StaticMode::CantGainLife
        | StaticMode::CantLoseLife
        | StaticMode::PlayerProtection(..)
        | StaticMode::MustAttack
        | StaticMode::MustAttackPlayer { .. }
        | StaticMode::MustBlock
        | StaticMode::MustBlockAttacker { .. }
        | StaticMode::CantDraw { .. }
        | StaticMode::DrawFromBottom { .. }
        | StaticMode::DoubleTriggers { .. }
        | StaticMode::IgnoreHexproof
        | StaticMode::ExtraBlockers { .. }
        | StaticMode::RevealTopOfLibrary { .. }
        | StaticMode::RevealHand { .. }
        | StaticMode::TopOfLibraryHasPlot
        | StaticMode::TopOfLibraryPlotPermission
        | StaticMode::CastFromHandFree { .. }
        | StaticMode::LinkedCollectionCounterPlayPermission
        | StaticMode::CountersPersistAcrossZones { .. }
        // CountersCantBeRemoved (Fear of Sleep Paralysis) is a counter-removal
        // prohibition — no payment cost; its `counter_type` field is a filter, not
        // a board read — so its cost surface is read-free.
        | StaticMode::CountersCantBeRemoved { .. }
        | StaticMode::CantBeCountered
        | StaticMode::CantBeCopied
        | StaticMode::CantEnterBattlefieldFrom
        | StaticMode::CantCastFrom { .. }
        | StaticMode::CantCastDuring { .. }
        | StaticMode::CantActivateDuring { .. }
        | StaticMode::PerTurnCastLimit { .. }
        | StaticMode::PerTurnDrawLimit { .. }
        | StaticMode::SuppressTriggers { .. }
        | StaticMode::CantBeBlocked
        | StaticMode::CantBeBlockedExceptBy { .. }
        | StaticMode::CantBeBlockedBy { .. }
        | StaticMode::CantBeBlockedByMoreThan { .. }
        | StaticMode::CantBeBlockedUnlessAllBlock
        | StaticMode::AttachmentRestriction { .. }
        | StaticMode::Protection
        | StaticMode::Indestructible
        | StaticMode::CantBeDestroyed
        | StaticMode::CantBeRegenerated
        | StaticMode::FlashBack
        | StaticMode::Shroud
        | StaticMode::Hexproof
        | StaticMode::Vigilance
        | StaticMode::Menace
        | StaticMode::Reach
        | StaticMode::Flying
        | StaticMode::Trample
        | StaticMode::Deathtouch
        | StaticMode::Lifelink
        | StaticMode::CantTap
        | StaticMode::CantUntap
        | StaticMode::MustBeBlocked { .. }
        | StaticMode::MustBeBlockedByAll { .. }
        | StaticMode::Goaded
        | StaticMode::MustAttackAwayFromSource
        | StaticMode::CombatAlone { .. }
        | StaticMode::CantCrew
        | StaticMode::CantPhaseIn
        | StaticMode::CrewContribution { .. }
        | StaticMode::MayLookAtTopOfLibrary
        | StaticMode::MayLookAtFaceDown
        | StaticMode::CantBeTurnedFaceUp
        | StaticMode::MayChooseNotToUntap
        | StaticMode::AdditionalLandDrop { .. }
        | StaticMode::EmblemStatic
        | StaticMode::BlockRestriction { .. }
        | StaticMode::NoMaximumHandSize
        | StaticMode::MaximumHandSize { .. }
        | StaticMode::MayPlayAdditionalLand
        | StaticMode::CantHaveKeyword { .. }
        | StaticMode::CantWinTheGame
        | StaticMode::CantLoseTheGame
        | StaticMode::LegendRuleDoesntApply
        | StaticMode::SpeedCanIncreaseBeyondFour
        | StaticMode::DefilerCostReduction { .. }
        | StaticMode::SkipStep { .. }
        | StaticMode::SpendManaAsAnyColor { .. }
        | StaticMode::PayLifeAsColoredMana { .. }
        | StaticMode::StepEndUnspentMana { .. }
        | StaticMode::UnspentManaLossCausesLifeLoss
        | StaticMode::CanAttackWithDefender
        | StaticMode::AttackOnlyNeighbor
        | StaticMode::IgnoreLandwalkForBlocking { .. }
        | StaticMode::CanActivateAbilitiesAsThoughHaste
        | StaticMode::CanBlockShadow
        | StaticMode::AssignNoCombatDamage
        | StaticMode::UntapsDuringEachOtherPlayersUntapStep
        | StaticMode::MaxUntapPerType { .. }
        | StaticMode::EntersWithAdditionalCounters { .. }
        | StaticMode::CountsAsNamed { .. }
        | StaticMode::Other(..) => false,
    }
}

/// §5.4 (review LOW): the object's full ability TREE cost surface — the top-level
/// `cost` plus every nested `sub_ability` / `else_ability` / `mode_abilities` cost
/// (each `AbilityDefinition` carries its own `cost`). `ability_definition_axes`
/// binds `cost` read-free (deferred here), so a board-scaling cost on a NESTED
/// sub-ability would otherwise be scanned by neither the §5.3a effect firewall nor a
/// top-level-only cost scan. Each cost routes through the EXHAUSTIVE `AbilityCost`
/// scanner (Finding-2, NO `_`).
fn ability_tree_cost_references_growing_class(
    def: &crate::types::ability::AbilityDefinition,
) -> bool {
    use crate::game::ability_scan::ability_cost_references_sibling_mutable as reads;
    if def.cost.as_ref().is_some_and(reads) {
        return true;
    }
    if def
        .sub_ability
        .as_deref()
        .is_some_and(ability_tree_cost_references_growing_class)
    {
        return true;
    }
    if def
        .else_ability
        .as_deref()
        .is_some_and(ability_tree_cost_references_growing_class)
    {
        return true;
    }
    def.mode_abilities
        .iter()
        .any(ability_tree_cost_references_growing_class)
}

/// §5.4 item (3): unwrap an `AdditionalCost` to its embedded `AbilityCost`(s) and
/// scan each through the EXHAUSTIVE cost scanner. Exhaustive no-`_` over
/// `AdditionalCost` so a new cost shape forces a decision.
fn additional_cost_references_growing_class(a: &crate::types::ability::AdditionalCost) -> bool {
    use crate::game::ability_scan::ability_cost_references_sibling_mutable as reads;
    use crate::types::ability::AdditionalCost;
    match a {
        AdditionalCost::Optional { cost, .. } | AdditionalCost::Required(cost) => reads(cost),
        AdditionalCost::Kicker { costs, .. } => costs.iter().any(reads),
        AdditionalCost::Choice(a, b) => reads(a) || reads(b),
    }
}

/// CR 704.5f / CR 704.5g / CR 704.5i: strict-compare the PRE-projection object
/// resource axes the SBA layer reads every beat — `damage_marked` (lethal marked
/// damage) and the FULL `counters` map (toughness-lowering `-1/-1`, loyalty). The
/// inherited `project_out_resources` zeroes these for the 2p equality path (which
/// NEEDS them projected — lifelink/ping loops mark damage monotonically), so the
/// coverability path re-asserts them here: a counter/damage rider that drifts
/// projection-invisibly would otherwise ride a covering pair to a false win, then
/// graveyard its own churner source mid-extrapolation. Sibling of
/// [`loyalty_activation_counts_match`] — same shared-object-id iteration, symmetric
/// because gate (1)'s `loop_states_equal` already requires identical object sets.
fn object_resource_axes_match(prior: &GameState, current: &GameState) -> bool {
    prior.objects.iter().all(|(id, oa)| {
        current
            .objects
            .get(id)
            .is_none_or(|ob| oa.damage_marked == ob.damage_marked && oa.counters == ob.counters)
    })
}

/// Normalize a stack into behavioral-identity clones for coverability counting:
/// zero the volatile top-level `id`/`source_id` and the per-kind inner `source_id`,
/// strip nested `source_id`s from the embedded ability, and retain the associated
/// trigger-firing class
/// ([`crate::game::triggers::normalize_ability_identity`]). KEEP `controller` (an
/// opponent's otherwise-identical trigger must never merge with the controller's)
/// and the entire `kind` payload (`condition`, `trigger_event`,
/// `subject_match_count`, `die_result`, `description`, `source_name`) — a residual
/// content difference only SUPPRESSES a match (fail-safe). Two same-controller
/// entries differing only in `source_id` (two Blight-Priest copies) resolve
/// identically after the item-4 guard, so identifying them is sound.
fn normalized_stack_entries(state: &GameState) -> Vec<(StackEntry, Option<TriggerFiring>)> {
    state
        .stack
        .iter()
        .map(|entry| {
            let firing = state
                .stack_trigger_firings
                .get(&entry.id)
                .copied()
                .map(|firing| match firing {
                    TriggerFiring::ReceiptEligible(_) => TriggerFiring::LegacyDelayed,
                    firing => firing,
                });
            let mut norm = entry.clone();
            norm.id = ObjectId(0);
            norm.source_id = ObjectId(0);
            match &mut norm.kind {
                StackEntryKind::TriggeredAbility {
                    source_id, ability, ..
                } => {
                    *source_id = ObjectId(0);
                    crate::game::triggers::normalize_ability_identity(ability);
                }
                StackEntryKind::ActivatedAbility { source_id, ability } => {
                    *source_id = ObjectId(0);
                    crate::game::triggers::normalize_ability_identity(ability);
                }
                StackEntryKind::Spell {
                    ability: Some(ability),
                    ..
                } => crate::game::triggers::normalize_ability_identity(ability),
                StackEntryKind::Spell { ability: None, .. }
                | StackEntryKind::KeywordAction { .. } => {}
            }
            (norm, firing)
        })
        .collect()
}

/// Stack coverability (§2.2 item 2): `prior` is an order-preserving bottom-up
/// SUBSEQUENCE of `current` (2a), at least one normalized kind strictly grew, and
/// EVERY kind that grew already occurs in `prior` with count ≥ 1 (2b — a
/// never-before-seen 0→1 entry is rejected outright, its resolution behavior never
/// having been observed inside the window).
///
// ponytail: greedy embedding + per-kind linear counts, n = stack depth (small);
// revisit only if a deep-stack combo profiles hot.
fn stack_covers(
    prior: &[(StackEntry, Option<TriggerFiring>)],
    current: &[(StackEntry, Option<TriggerFiring>)],
) -> bool {
    // (2a) greedy two-pointer subsequence embedding, bottom-up.
    let mut ci = 0usize;
    for pe in prior {
        loop {
            if ci >= current.len() {
                return false;
            }
            let matched = &current[ci] == pe;
            ci += 1;
            if matched {
                break;
            }
        }
    }
    // (2b) strict growth confined to already-occupied places.
    let mut any_growth = false;
    for (idx, ce) in current.iter().enumerate() {
        // process each distinct kind once (first occurrence).
        if current[..idx].iter().any(|e| e == ce) {
            continue;
        }
        let cn = current.iter().filter(|e| *e == ce).count();
        let pn = prior.iter().filter(|e| *e == ce).count();
        if cn > pn {
            if pn == 0 {
                return false;
            }
            any_growth = true;
        }
    }
    any_growth
}

/// CR 603.3c / CR 603.3d + CR 601.2d: does a stack entry take NO player ordering
/// input at resolution? Only a `TriggeredAbility` qualifies (`Spell`/
/// `ActivatedAbility` are player-driven; `KeywordAction` carries no `ResolvedAbility`)
/// with no targets, no variable-count targeting, no divide/distribute assignment,
/// and no cross-target constraints on the embedded ability. The mid-construction
/// modal firewall (`state.pending_trigger_entry != Some(entry.id)`) is unreachable
/// while both compared states sit at `WaitingFor::Priority`, but keeps the guard
/// closed under future sampling changes (a chosen mode is otherwise baked into the
/// entry's `ability`, so the normalized key already separates distinct modes).
///
/// Contract boundary: this gate owns only ANNOUNCEMENT-time ordering input
/// (targets, divide/distribute, cross-target constraints). Resolution-time
/// choices (CR 608.2d — proliferate/populate/sacrifice-choice/optional/…) are
/// owned by item 6 (`stack_entry_resolution_choice_freedom`), applied to every
/// current-stack entry, not just grown ones.
fn stack_entry_has_no_ordering_input(state: &GameState, entry: &StackEntry) -> bool {
    let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
        return false;
    };
    if state.pending_trigger_entry == Some(entry.id) {
        return false;
    }
    // Variable-count / divide-distribute / cross-target constraints are always
    // ordering input (the player picks how many / how to split / which combo).
    if ability.multi_target.is_some()
        || ability.distribution.is_some()
        || !ability.target_constraints.is_empty()
    {
        return false;
    }
    // A no-target trigger takes no announcement-time input.
    if ability.targets.is_empty() {
        return true;
    }
    // CR 603.3d + CR 608.2b + CR 732.2a: a non-empty target list is NOT player
    // ordering input when exactly one legal assignment exists — the choice is
    // FORCED, so the shortcut stays deterministic. Re-derived per call against the
    // board the CALLER passes, and that board is load-bearing rather than incidental:
    // there are THREE call sites and none of them is "the live state" by construction
    // — the announced loop passes the pair's own CARRYING FRAME (a retained ring
    // sample), the current-stack loops pass `current`. The verdict is a function of
    // the board's legal-target population, so handing a retained pair the live board
    // is fail-OPEN: a target legal on the frame but gone from `current` collapses the
    // assignment to "forced" and relieves a choice that is not forced.
    forced_unique_targeting(state, ability)
}

/// CR 603.3d / CR 608.2b / CR 732.2a: exactly one legal target assignment ⇒ the
/// target choice is FORCED, not player ordering input. Reuses the engine's own
/// auto-target oracle (`auto_select_targets_for_ability => Ok(Some(_))` iff a
/// single legal assignment exists, limit=2) — the same authority the trigger
/// dispatcher uses. Fail-closed on any build error, empty slots, or ≥2 legal
/// assignments (`Ok(None)` / `Err`).
///
/// "The same authority the trigger dispatcher uses" is MEASURED, not asserted:
/// `triggers::prepare_trigger_targets` calls this very function and routes
/// `Ok(Some(targets))` to `PreparedTriggerTargets::AutoAssigned` (targets assigned
/// at dispatch, no prompt) and `Ok(None)` to `NeedsPlayerChoice` (the
/// `WaitingFor::TriggerTargetSelection` prompt). So `true` here is exactly "the
/// dispatcher will announce this target itself and the player is never asked".
///
/// `pub(crate)` for ONE additional consumer, and it is the publish side of the same
/// question: [`crate::game::engine::entry_publishes_pin_slots`] must not publish a
/// CR 732.2a decision point for a choice no player makes. Exported rather than
/// re-derived there — two copies of this predicate could disagree about whether a
/// choice is forced, and the publisher and the relief disagreeing is precisely the
/// fail-open shape gate (3) exists to prevent.
///
/// ⚠ THE BOARD IS THE VERDICT. Every caller must pass the frame the rest of its own
/// derivation uses — the announced pair's CARRYING FRAME for a retained sample, the
/// live board only for a live entry. Handing a retained pair the live board is
/// fail-OPEN: a target legal on the frame but gone from the live board collapses the
/// assignment to "forced" and relieves (or unpublishes) a choice that is not forced.
pub(crate) fn forced_unique_targeting(
    state: &GameState,
    ability: &crate::types::ability::ResolvedAbility,
) -> bool {
    match crate::game::ability_utils::build_target_slots(state, ability) {
        Ok(slots) if !slots.is_empty() => matches!(
            crate::game::ability_utils::auto_select_targets_for_ability(
                state,
                ability,
                &slots,
                &ability.target_constraints,
            ),
            Ok(Some(_))
        ),
        _ => false,
    }
}

/// §2.2 item 4: does this stack entry's AST read ANY still-projected axis (the
/// narrowed set: player-level monotone resources/tallies + the journal/count block)?
/// Delegates to the C0 walker's third axis over the embedded ability (which itself
/// recurses `sub_ability`/`else_ability` and the ability-level `AbilityCondition`),
/// plus the trigger-level `TriggerCondition` (CR 603.4 intervening-if). Object-axis
/// readers classify as NON-reading — their drift breaks gate (1) instead. A
/// `KeywordAction` has no AST to classify ⇒ fail closed (`true`); a permanent
/// `Spell { ability: None }` reads nothing (its resolution changes the board and
/// breaks gate (1) anyway) ⇒ `false`.
fn stack_entry_reads_projected_resource(entry: &StackEntry) -> bool {
    // Trigger-level intervening-if (CR 603.4) — carried on the kind, not the ability.
    if let StackEntryKind::TriggeredAbility {
        condition: Some(condition),
        ..
    } = &entry.kind
    {
        if crate::game::ability_scan::trigger_condition_reads_projected_resource(condition) {
            return true;
        }
    }
    match entry.ability() {
        Some(ability) => {
            // The resolution-time branch selector (`AbilityCondition`) is scanned
            // explicitly for self-documenting item-4 coverage; the whole-ability scan
            // (which recurses `sub_ability`/`else_ability` and re-covers `.condition`)
            // catches every other read surface.
            ability
                .condition
                .as_ref()
                .is_some_and(crate::game::ability_scan::ability_condition_reads_projected_resource)
                || crate::game::ability_scan::ability_reads_projected_resource(ability)
        }
        // KeywordAction: no AST to classify ⇒ fail closed. Permanent `Spell { ability:
        // None }`: nothing to read (its resolution changes the board, breaking gate 1).
        None => matches!(entry.kind, StackEntryKind::KeywordAction { .. }),
    }
}

/// §2.2 item 6: can resolving this stack entry offer a resolution-time player
/// choice (a non-priority `WaitingFor` the C2/no-ordering-input gate cannot see)?
/// Delegates to the ability_scan choice classifier over the embedded ability.
/// Exhaustive over all four `StackEntryKind`s (no wildcard): only a
/// `TriggeredAbility` carries a `ResolvedAbility` to classify; `Spell`/
/// `ActivatedAbility`/`KeywordAction` are fail-closed `MayPrompt` — even a
/// bottom-frozen entry the extrapolation never resolves rejects the cover.
/// (Ceiling + upgrade path: model which stack suffix resolves per cycle only if
/// a real fixture needs it.) The trigger-level `condition` (intervening-if
/// re-check, CR 603.4) is pure evaluation and contributes no prompt.
fn stack_entry_resolution_choice_freedom(
    state: &GameState,
    entry: &StackEntry,
    budget: &mut ProbeBudget,
) -> crate::game::resolution_prompt::ResolutionChoiceFreedom {
    use crate::game::resolution_prompt::ResolutionChoiceFreedom;
    match &entry.kind {
        StackEntryKind::TriggeredAbility { ability, .. } => {
            // CR 603.4 + CR 608.2k: the classifier probes a RESOLUTION, so it
            // must be handed the board `resolve_top` would hand
            // `resolve_ability_chain` — this entry off the stack, with
            // resolution scope bound. Handing it the raw pre-resolution board
            // resolves every `EventContextAmount` / `Triggering*` reference
            // against an absent context and is FAIL-OPEN for the `> 0`-gated
            // virtual replacement arms.
            //
            // The entry is removed BY ID, never by `pop`: the callers walk the
            // stack at arbitrary depth, so "pop the top" is wrong for every
            // non-top entry, while "this entry is the one resolving" is the
            // counterfactual being asked. The clone lands only on the probe —
            // `resolve_top` calls the same shared binding on its own
            // `&mut GameState` and pays nothing.
            // CR 732.2a: THE CHEAP BINDING PRECONDITION RUNS BEFORE THE CLONE.
            //
            // The whole-`GameState` clone below is the dominant per-entry work, and it
            // used to be paid unconditionally — including for entries the classifier was
            // about to reject on a pure AST gate (`optional`, `unless_pay`, a modal
            // header, an `UpTo` count, a non-allow-listed effect). Those entries bought
            // a full board copy and a scope binding to reach a verdict that never looked
            // at the board.
            //
            // `chain_offers_choice` is that verdict's AST half, so asking it here costs
            // nothing and removes the clone entirely for every gated entry.
            //
            // DELIBERATELY NOT a `try_charge_one` in this position, which was the other
            // remedy on offer: the meter's charge sits BELOW the `optional` gate by
            // design, and `r16_the_f4_offering_beats_probe_demand_is_exactly_measured`
            // pins `spent == conjunct6_asks` for exactly that reason — its own message
            // predicts that hoisting the charge above the gate makes every optional ask
            // charge twice. Measured: adding a charge here does make that row fail. The
            // precondition form bounds the same dominant work without moving the meter.
            if crate::game::resolution_prompt::chain_offers_choice(ability) {
                return ResolutionChoiceFreedom::MayPrompt;
            }
            let mut board = state.clone();
            board.stack.retain(|e| e.id != entry.id);
            if !crate::game::stack::bind_resolution_scope(&mut board, entry, None) {
                // CR 603.4 false ⇒ the live resolution proposes nothing, and an
                // empty derivation is never "safe".
                return ResolutionChoiceFreedom::MayPrompt;
            }
            crate::game::resolution_prompt::ability_resolution_choice_freedom(
                &board, ability, budget,
            )
        }
        StackEntryKind::Spell { .. }
        | StackEntryKind::ActivatedAbility { .. }
        | StackEntryKind::KeywordAction { .. } => ResolutionChoiceFreedom::MayPrompt,
    }
}

/// §2.2 item 5 (the R4-G1 second scan surface): does ANY live off-stack fire-time
/// condition read a still-projected resource? A dormant intervening-if / replacement
/// / condition-gated static that reads a projected axis (CR 603.4 / CR 614.1 /
/// CR 604.1 / CR 613.1 / CR 101.2) produces NO stack entry on either compared frame,
/// so item 4 cannot see it — yet it arms mid-extrapolation and breaks the replay.
/// Run once on `current` (item-1 board equality makes the definition sets identical).
/// Fail-closed: any surface the scan cannot classify ⇒ reject (no shortcut).
///
/// Keyword-synthesized granted triggers (`KeywordTriggerInstaller::triggers_for`
/// / `synthesize_granted_keyword_triggers`) ARE scanned here — loop (iv), via
/// `crate::game::triggers::granted_keyword_triggers_in_zone` (the same synthesis
/// authority the live trigger-collection path uses). They are produced
/// on-the-fly during trigger collection and (for off-zone grants, and in any
/// state where layer 6 has not reinstalled them) never land on
/// `obj.trigger_definitions`, so `active_trigger_definitions` (loop (i)) cannot
/// be relied on to reach them. Most such triggers carry non-projected fire-time
/// conditions (Echo→`EchoDue`, Renown→`Not(IsRenowned)`, Suspend/Soulshift/
/// Vanishing/CumulativeUpkeep→counter/zone conditions, Soulbond→filter
/// conditions), but Dethrone does not — see below.
///
/// The item-5 classifier (`trigger_condition_reads_projected_resource`) flags
/// four granted-keyword conditions as projected-reading — Dethrone, Increment,
/// Soulbond, Training — but only Dethrone is a GENUINE projected read. Dethrone
/// (CR 702.105a) compares the defending player's `LifeTotal` to the max
/// `LifeTotal` among all players (CR 119 life = a PROJECTED axis this pass
/// zeroes); Increment/Soulbond/Training are fail-closed false positives
/// (`ManaSpentToCast` / control-filter / co-attacker-power reads the classifier's
/// `Axes::CONSERVATIVE` walk cannot descend, all cast/combat/object state gate (1)
/// strict-compares). Because loop (iv) now scans these synthesized defs, a
/// runtime-GRANTED Dethrone (`Effect::GrantKeywords` /
/// `ContinuousModification::AddKeyword`) whose dormant condition would arm
/// mid-extrapolation is caught (fail-safe reject) — closing the inc2b
/// dormant-arming hole (false WIN, N1(k) class). This makes item-5 structurally
/// complete for granted keywords rather than a hand-list. The guard test
/// `granted_keyword_trigger_conditions_projected_reads_are_exactly_known_gaps` in
/// `game::triggers` still pins the flagged set so a NEW projected-reading
/// granted-keyword condition surfaces as a review signal.
fn fire_time_conditions_read_projected_resource(state: &GameState) -> bool {
    fire_time_conditions_read_projected_resource_scoped(state, LoopWindowScope::unproven())
}

/// Scoped sibling of [`fire_time_conditions_read_projected_resource`] — see
/// [`LoopWindowScope`]. Reads `scope.cast_card_ids` (CR 601.2f, block (iii-static));
/// that guard sits inside an `is_some_and`, so [`LoopWindowScope::unproven`] never
/// reaches it and the 2-arg wrapper stays identity (`scoped_wrappers_are_identity`).
fn fire_time_conditions_read_projected_resource_scoped(
    state: &GameState,
    scope: LoopWindowScope<'_>,
) -> bool {
    // (i) Trigger fire-time intervening-if conditions (CR 603.4). `active_trigger_
    // definitions` is the liveness authority (CR 702.26b phased-out + CR 114.4
    // command-zone gate) that deliberately does NOT filter by `condition`.
    for obj in state.objects.values() {
        for active in crate::game::functioning_abilities::active_trigger_definitions(state, obj) {
            let def = active.definition;
            // CR 603.4 / CR 113.6: gate on zone-of-function — a permanent trigger on
            // a card in the library / hand / graveyard never fires during the loop
            // (mirror of the growing-class firewall's block (1) fix; the drain path
            // has the identical all-zones defect).
            if !crate::game::triggers::trigger_definition_functions_in_zone(def, obj.zone) {
                continue;
            }
            if def
                .condition
                .as_ref()
                .is_some_and(crate::game::ability_scan::trigger_condition_reads_projected_resource)
            {
                return true;
            }
        }
    }
    // (ii) Replacement definitions — condition AND body (CR 614.1). A replacement is
    // an in-loop transition that never lands on the stack, so item 4 never sees it.
    // The condition + runtime continuation have C0-walker predicates; body payloads
    // without one (an `execute` `AbilityDefinition`, a state-reading damage-amount
    // modification) are treated fail-closed — conservative, fail-safe (no shortcut).
    for (_, obj, def) in crate::game::functioning_abilities::active_replacements(state) {
        // CR 614.1 / CR 113.6: `active_replacements` is all-zones; restrict to the
        // zones a battlefield-event replacement functions in (mirror of the
        // growing-class firewall's block (3) fix).
        if !matches!(obj.zone, Zone::Battlefield | Zone::Command) {
            continue;
        }
        if def
            .condition
            .as_ref()
            .is_some_and(crate::game::ability_scan::replacement_condition_reads_projected_resource)
        {
            return true;
        }
        if def
            .runtime_execute
            .as_ref()
            .is_some_and(|a| crate::game::ability_scan::ability_reads_projected_resource(a))
        {
            return true;
        }
        if replacement_body_may_read_projected(def) {
            return true;
        }
    }
    // (iii) Condition-gated statics (CR 604.1 / CR 613.1) — ALL modes via `iter_all()`
    // (NOT the condition-filtered active iterator, whose gate hides exactly the
    // dormant defs this surface exists to catch), plus transient continuous effects'
    // `ForAsLongAs`/gating conditions (CR 604.1).
    for obj in state.objects.values() {
        if obj.is_phased_out() {
            continue;
        }
        for def in obj.static_definitions.iter_all() {
            // CR 113.6 / CR 604.3: gate on zone-of-function (mirror of the
            // growing-class firewall's block (4) fix; keeps graveyard/exile-
            // functional statics and command emblems, drops inert deck/hand cards).
            if !crate::game::functioning_abilities::static_functions_in_zone(obj, def) {
                continue;
            }
            // CR 601.2f vs CR 604.1 / CR 613.1: a self-cost modifier on a card the
            // window provably never casts cannot modify any cost paid inside the
            // window, so its condition's read of a projected resource is not an
            // observation of the loop. Fail-closed on `cast_card_ids: None` (no proof
            // ⇒ scan everything); `Some(&[])` can never arise (see
            // `window_cast_card_ids`).
            if matches!(
                def.mode,
                crate::types::statics::StaticMode::ModifyCost { .. }
            ) && matches!(
                def.affected,
                Some(crate::types::ability::TargetFilter::SelfRef)
            ) && scope
                .cast_card_ids
                .is_some_and(|ids| !ids.contains(&obj.card_id))
            {
                continue;
            }
            if def
                .condition
                .as_ref()
                .is_some_and(crate::game::ability_scan::static_condition_reads_projected_resource)
            {
                return true;
            }
        }
    }
    for tce in &state.transient_continuous_effects {
        if crate::game::ability_scan::duration_reads_projected_resource(&tce.duration) {
            return true;
        }
        if tce
            .condition
            .as_ref()
            .is_some_and(crate::game::ability_scan::static_condition_reads_projected_resource)
        {
            return true;
        }
    }
    // (iv) Runtime-GRANTED keyword synthesized trigger defs (CR 603.4). These are
    // produced on-the-fly during trigger collection by
    // `synthesize_granted_keyword_triggers` / `KeywordTriggerInstaller` and — for
    // off-zone grants, and in any state where layer 6 has not (re)installed them —
    // never land on `obj.trigger_definitions`, so loop (i) cannot reach them. A
    // granted Dethrone (CR 702.105a) carries a fire-time intervening-if reading the
    // defending player's `LifeTotal` (CR 119, a projected axis this pass zeroes); a
    // dormant such condition would arm mid-extrapolation and break the replay.
    // Reuse the collection path's synthesis authority (single authority, no
    // duplicated synthesis) via `granted_keyword_triggers_in_zone`, which applies
    // the same zone gate. Fail-closed: the classifier's `Axes::CONSERVATIVE` walk
    // rejects any condition subtree it cannot descend.
    for obj in state.objects.values() {
        if obj.is_phased_out() {
            continue;
        }
        for def in crate::game::triggers::granted_keyword_triggers_in_zone(state, obj) {
            if def
                .condition
                .as_ref()
                .is_some_and(crate::game::ability_scan::trigger_condition_reads_projected_resource)
            {
                return true;
            }
        }
    }
    false
}

/// CR 113.6 (CR 113.6k): every trigger definition that FUNCTIONS in its source's current zone.
/// The shared board walk for the axis firewalls — `board_has_event_observer` and
/// [`board_has_functioning_etb_trigger`] both ask "which event does it react to?" of the same
/// set, so the zone gate has one authority.
fn functioning_board_trigger_defs(
    state: &GameState,
) -> impl Iterator<Item = &crate::types::ability::TriggerDefinition> {
    state.objects.values().flat_map(move |obj| {
        crate::game::functioning_abilities::active_trigger_definitions(state, obj)
            .map(|active| active.definition)
            .filter(move |def| {
                crate::game::triggers::trigger_definition_functions_in_zone(def, obj.zone)
            })
    })
}

/// CR 603.6a: does ANY functioning board trigger fire on a battlefield entry?
///
/// The route firewall for a batched collapse that MINTS TOKENS: each minted token is a real
/// CR 603.6a entry, so every board ETB trigger fires for real on top of whatever the batched
/// arithmetic already applied. Measured on the Sprout Swarm 4p dump: the batched
/// `[Tokens, Life { per_cycle_delta: 1 }]` pair took P0 from 546 to 596 at the collapse, and
/// draining the 50 real token-ETB triggers paid the SAME life again, ending at 646. Routing to
/// the concrete replay makes the real ETB triggers the ONLY source, which is what the board does.
///
/// SHAPE-AGNOSTIC by construction. An earlier form asked whether the trigger's effect chain was
/// an `Effect::GainLife`, which is under-approximate: life reaches `apply_life_gain` from four
/// resolvers (`effects/life.rs`, `effects/double.rs`, `effects/exchange_life.rs`, and
/// `effects/deal_damage.rs`'s CR 702.15b lifelink leg), so a Terror-of-the-Peaks-shaped board —
/// an ETB damage trigger on a permanent with lifelink — grows a genuinely ETB-sourced life axis
/// that no `Effect`-shape test can see. Asking only "is there a functioning ETB trigger" cannot
/// miss a life source.
///
/// This predicate and the effect-shape test it replaced are INCOMPARABLE, not nested — dropping the
/// `Effect::GainLife` conjunct is strictly LOOSER on effect shape (any ETB trigger counts, not just
/// a life-gaining one). What narrows the CALLER is a different axis: it pairs this with
/// `token_profile.is_some()`, so only a collapse that MINTS the entries can route here and a
/// token-less loop never does. Looser on shape, narrower on axis; neither side contains the other.
///
/// Distinct from [`life_growth_is_observed`], which asks whether a LUMP gain would miscount an
/// observer. Here the batched arithmetic is right and the double-apply comes from the collapse
/// itself. Deliberately NOT folded into `life_growth_is_observed`: that predicate also gates the
/// offer firewall, where this shape is not an observation. Deliberately NOT a
/// registration-cancelling suppressor either — the axis can be MIXED-cause (an ETB rider plus a
/// drain), and the batched `Life` registration is per-player, so dropping it would under-apply
/// the non-ETB half and silence the wrong beneficiary.
///
/// A sound OVER-approximation in the same idiom as its siblings: a true result routes to the
/// discrete N-cycle driver, which is always correct (only slower).
pub(crate) fn board_has_functioning_etb_trigger(state: &GameState) -> bool {
    use crate::types::triggers::TriggerEventKey;
    functioning_board_trigger_defs(state).any(|def| {
        crate::game::trigger_index::keys_from_trigger_def(def)
            .0
            .iter()
            .any(|key| matches!(key, TriggerEventKey::EnterBattlefield(_)))
    })
}

/// CR 732.2a / CR 603.4 / CR 614.1: does any battlefield/command-FUNCTIONING trigger fire on
/// `trig_key`, or any active battlefield/command replacement replace `repl_event`? The shared
/// per-event observer scan for the axis-specific firewalls, classifying triggers via the same
/// `keys_from_trigger_def` registry the trigger index uses.
fn board_has_event_observer(
    state: &GameState,
    trig_key: crate::types::triggers::TriggerEventKey,
    repl_event: ReplacementEvent,
) -> bool {
    if functioning_board_trigger_defs(state).any(|def| {
        crate::game::trigger_index::keys_from_trigger_def(def)
            .0
            .contains(&trig_key)
    }) {
        return true;
    }
    for (_, obj, def) in crate::game::functioning_abilities::active_replacements(state) {
        // CR 614.1 / CR 113.6: `active_replacements` is all-zones; a life/counter-event
        // replacement functions on the battlefield or in the command zone.
        if matches!(obj.zone, Zone::Battlefield | Zone::Command) && def.event == repl_event {
            return true;
        }
    }
    false
}

/// CR 732.2a + CR 122.1 / CR 701.34a: is the growing COUNTER axis OBSERVED — does any live
/// trigger, replacement, or count-reader react to a counter placement each cycle? A sound
/// OVER-approximation: a true result ROUTES the loop to the discrete N-cycle driver (always safe),
/// never a wrong single-batch. Returns true iff ANY:
/// - [`fire_time_conditions_read_growing_class`] — counter count-readers (a charge-count static;
///   a counter-reading condition / body / cost). Retained from the fodder firewall.
/// - a battlefield-functioning `CounterAdded` trigger ("whenever a +1/+1 counter is put …").
/// - an active battlefield/command `AddCounter` replacement (Corpsejack's counter doubler).
///
/// The batched N×δ counter collapse is sound ONLY when this is false: `apply_counter_addition`
/// emits one lump `CounterAdded` bypassing the replacement doubler pipeline. AXIS-SPECIFIC: a
/// life observer does NOT make counter growth observed (they read different mutation events), so a
/// pure counter loop still batches on a board carrying only a life observer.
pub(crate) fn counter_growth_is_observed(state: &GameState) -> bool {
    use crate::types::triggers::TriggerEventKey;
    fire_time_conditions_read_growing_class(state, None)
        || board_has_event_observer(
            state,
            TriggerEventKey::CounterAdded,
            ReplacementEvent::AddCounter,
        )
}

/// CR 732.2a + CR 119.3: is the growing LIFE axis OBSERVED — does any live trigger, replacement,
/// or projected-life-total reader react to a life gain each cycle? A sound OVER-approximation
/// (true ⇒ drive, always safe). Returns true iff ANY:
/// - a player-level projected life-total read off-stack
///   ([`fire_time_conditions_read_projected_resource`]) or on-stack
///   ([`stack_entry_reads_projected_resource`]) — a life-total condition / static / replacement body.
/// - a battlefield-functioning `LifeChanged` trigger (Heliod "whenever you gain life …"; also
///   `LifeLost`/`LifeChanged` via the shared event key — an over-approximation, still safe).
/// - an active battlefield/command `GainLife` replacement (Rhox's life-gain doubler).
///
/// The batched N×δ life collapse is sound ONLY when this is false: `apply_life_gain` re-runs the
/// replacement pipeline, so a lump gain fires a life observer ONCE not N×. AXIS-SPECIFIC: a
/// counter observer does NOT make life growth observed.
pub(crate) fn life_growth_is_observed(state: &GameState) -> bool {
    use crate::types::triggers::TriggerEventKey;
    fire_time_conditions_read_projected_resource(state)
        || state.stack.iter().any(stack_entry_reads_projected_resource)
        || board_has_event_observer(
            state,
            TriggerEventKey::LifeChanged,
            ReplacementEvent::GainLife,
        )
}

/// CR 614.1a: a replacement's BODY (not its `condition`) can read a projected
/// player resource. `QuantityModification` variants are all fixed constants (no
/// read). `DamageModification::LifeFloor` caps against a player's live life total
/// (CR 119, projected); `Plus { value }` carries a `QuantityExpr` that MAY read one
/// — treated fail-closed. `execute` is an `AbilityDefinition` with no C0-walker
/// predicate ⇒ fail-closed when present. The un-flagged `DamageModification` /
/// `QuantityModification` variants are safe to omit because their outputs land in
/// STRICT-COMPARED state (token/counter counts, source power) — not a projected
/// axis — so a divergence there already breaks gate (1) directly rather than
/// arming mid-extrapolation. All other modification variants read only fixed
/// amounts or the source's own (strict-compared) power.
fn replacement_body_may_read_projected(def: &crate::types::ability::ReplacementDefinition) -> bool {
    if def.execute.is_some() {
        return true;
    }
    matches!(
        def.damage_modification,
        Some(DamageModification::LifeFloor { .. } | DamageModification::Plus { .. })
    )
}

/// CR 119 / CR 106.1 / CR 122.1: zero every PLAYER axis removed from strict loop
/// equality. The no-`..` destructure is compiler-total (mirror of
/// `_gamestate_partition_is_total`, game_state.rs): a new `Player` field BREAKS THE
/// BUILD until the author classifies it — zero it here (project out) or bind `_`
/// (keep in strict equality). Paired with [`projected_player_axes`] (the BLOCKER-2
/// sign-check reads the SAME projected field set, also no-`..`), so a newly-projected
/// consumable cannot be silently missed by the sign veto.
fn project_out_player_consumables(p: &mut Player) {
    let Player {
        life,
        mana_pool,
        poison_counters,
        energy,
        player_counters,
        life_gained_this_turn,
        life_lost_this_turn,
        cards_drawn_this_turn,
        cards_drawn_this_step,
        // Strict-equality fields (NOT projected) — bound `_`, NO `..`:
        id: _,
        library: _,
        hand: _,
        graveyard: _,
        attraction_deck: _,
        contraption_deck: _,
        contraption_crank_sprocket: _,
        sticker_sheets: _,
        has_drawn_this_turn: _,
        lands_played_this_turn: _,
        life_lost_last_turn: _,
        descended_this_turn: _,
        speed: _,
        speed_trigger_used_this_turn: _,
        crimes_committed_this_turn: _,
        drew_from_empty_library: _,
        turns_taken: _,
        is_eliminated: _,
        bending_types_this_turn: _,
        status: _,
        companion: _,
        chosen_attributes: _,
        can_look_at_top_of_library: _,
        commander_color_identity: _,
    } = p;
    // CR 119: life is monotone in a drain/lifegain loop.
    *life = 0;
    // CR 106.1: floating mana is consumed/produced within the loop.
    mana_pool.clear();
    // CR 122.1: consumable counters a loop pumps (poison/energy/…).
    *poison_counters = 0;
    *energy = 0;
    player_counters.clear();
    // Per-turn resource trackers the strict PartialEq compares — these grow with the
    // loop but do not change the board configuration.
    *life_gained_this_turn = 0;
    *life_lost_this_turn = 0;
    *cards_drawn_this_turn = 0;
    *cards_drawn_this_step = 0;
}

/// Clone a state through `normalize_for_loop` and additionally zero every
/// monotone resource the modulo comparison must ignore. The result is only ever
/// fed to `loop_states_equal`; it is never used as a live game state.
/// CR 120 / CR 122.1 / CR 613.4c: project the monotone per-object resources out of one
/// object (the single authority, shared by [`project_out_resources`] and the object-growth
/// hook's fodder-class representative so the class compares in the SAME normalized form as
/// the projected frame objects — otherwise a raw-P/T class member would fail
/// `fodder_content_eq` against the P/T-zeroed frame and be mis-partitioned as stable-engine).
pub(crate) fn project_object_for_loop(object: &mut crate::game::game_object::GameObject) {
    // CR 120: marked damage is a monotone resource (lifelink/ping loops).
    object.damage_marked = 0;
    // CR 122.1: project out only *monotone* counters (CR 122.1a/613.4c +1/+1, -1/-1,
    // P/T; CR 306.5b loyalty; CR 310.4c defense) — these are the pumped resource of a
    // +1/+1 or loyalty loop, so two cycles compare as the same board. PRESERVE
    // consumable/duration/state-gating counters (CR 122.1b/c/d stun/shield/keyword;
    // CR 702.62a/63a time; CR 702.32a fade; CR 702.24a age; CR 714.3 lore; generic):
    // consuming one of these is a real board change, not a monotone pump, so it must
    // remain visible to `objects_content_eq` (game_state.rs counter comparison).
    object
        .counters
        .retain(|ct, _| !ct.is_monotone_loop_resource());
    // CR 613.4c: the counter-derived fields are zeroed because they derive ONLY from the
    // monotone counters just projected out — power/toughness fold only
    // `power_toughness_delta()==Some` counters, loyalty derives only from
    // CounterType::Loyalty and defense only from CounterType::Defense. The preserved
    // counters never reach these four fields, so zeroing cannot mask a consumed
    // non-monotone counter.
    object.power = None;
    object.toughness = None;
    object.loyalty = None;
    object.defense = None;
}

fn project_out_resources(state: &GameState) -> GameState {
    let mut s = state.normalize_for_loop();

    for player in &mut s.players {
        // BLOCKER-2: single authority for the projected player-consumable set,
        // shared with the `projected_player_axes` sign-check (compiler-total, no-`..`).
        project_out_player_consumables(player);
    }

    for (_, object) in s.objects.iter_mut() {
        project_object_for_loop(object);
    }

    // Per-turn / per-game *bookkeeping* accumulators the dynamic Engine-A path
    // perturbs each cycle. This block runs ONLY in the offline `loop_states_equal_
    // modulo_resources` comparison and never touches a live game state, so it cannot
    // affect the strict CR 104.4b mandatory-draw path (which compares
    // `normalize_for_loop()` directly, not this projection). The accumulators
    // partition into two classes that are handled OPPOSITELY:
    //   * repetition-BLOCKING legality gates (per-turn/per-game activation tallies,
    //     once-per-turn/N-times trigger limits, per-object loyalty activation count)
    //     — PRESERVED (or compared analysis-locally) so a GATED loop compares UNEQUAL
    //     and is not falsely certified as infinite;
    //   * pure pumped HISTORY (journals, counts, branch/quantity sources) — CLEARED
    //     so a genuine unrestricted loop compares equal.
    //
    // Pure pumped HISTORY: journals, counts, and branch/quantity sources a genuine
    // loop pumps every cycle. None of these BLOCK loop repetition (they are read by
    // branch conditions or quantity refs, not by a once-per-turn/N-times legality
    // gate), so their downstream effect is caught by the board-equality or net-progress
    // gates — clearing them is required so a real loop compares equal. Only the
    // repetition-blocking activation/trigger/loyalty gates above are preserved.
    s.spells_cast_this_turn = 0;
    s.spells_cast_last_turn = None;
    s.priority_pass_count = 0;
    // CR 602.5b: per-turn / per-game activation gates. These tallies are bumped for
    // EVERY activation (restrictions.rs record_ability_activation, unconditional), so
    // they grow for unrestricted loops too — blanket-clearing them would erase the
    // gate that makes a once-per-turn ("Activate only once each turn") or once-per-game
    // ability NON-repeatable, falsely certifying it as infinite. Retain only the keys
    // whose ability actually carries the matching restriction so two cycles of a GATED
    // activation compare DIFFERENT (the gate progressed) while pure pumped history is
    // still projected out (unrestricted loops compare equal).
    let keep_turn: HashSet<(ObjectId, usize)> = s
        .activated_abilities_this_turn
        .keys()
        .filter(|key| ability_has_per_turn_activation_gate(&s, key))
        .copied()
        .collect();
    s.activated_abilities_this_turn
        .retain(|key, _| keep_turn.contains(key));
    let keep_game: HashSet<(ObjectId, usize)> = s
        .activated_abilities_this_game
        .keys()
        .filter(|key| ability_has_per_game_activation_gate(&s, key))
        .copied()
        .collect();
    s.activated_abilities_this_game
        .retain(|key, _| keep_game.contains(key));
    // CR 603.4: NthResolutionThisTurn{n} is a one-shot branch SELECTOR (an effect
    // branch fires when the per-ability resolution count == n), NOT a repetition-
    // blocking legality gate. Clearing it is sound: a board-divergent Nth branch is
    // caught by objects_content_eq, and a resource-only Nth branch is a one-time bonus
    // the warmup-skipping steady-cycle measurement never re-counts. Projected out as
    // pure pumped history.
    s.ability_resolutions_this_turn.clear();
    s.loyalty_abilities_activated_this_turn.clear();
    s.extra_loyalty_activations_this_turn.clear();
    // CR 603.2h: trigger once-per-turn / N-times-per-turn limits. These maps have
    // EXACTLY ONE writer each — the constraint-keyed `record_trigger_fired`
    // (triggers.rs), which returns early for an unconstrained trigger:
    // `triggers_fired_this_turn` is written ONLY for `TriggerConstraint::OncePerTurn`,
    // `trigger_fire_counts_this_turn` ONLY for `MaxTimesPerTurn`. An UNRESTRICTED
    // (repeatable) trigger inserts into NEITHER, so a legitimate unrestricted-trigger
    // loop never touches them and PRESERVING them cannot break legit-loop equality.
    // For a GATED trigger the key/count is present/grows, so two cycles compare
    // DIFFERENT — exactly the soundness the gate enforces (a once-per-turn trigger
    // cannot drive an infinite loop). `triggers_fired_this_turn_per_opponent`
    // (OncePerOpponentPerTurn) and `triggers_fired_this_game` (OncePerGame) are
    // likewise NOT cleared here — consistent with the preserved `crew_activated_this_turn`.
    // CR 120: who has dealt damage + the per-turn damage event log.
    s.objects_that_dealt_damage.clear();
    s.damage_dealt_this_turn.clear();
    // CR 601: per-turn / per-game cast journals.
    s.spells_cast_this_turn_by_player.clear();
    s.spells_cast_this_game.clear();
    s.spells_cast_this_game_by_player.clear();
    // CR 400 (zones) / CR 603.6a (ETB) / CR 701.21 (sacrifice) / CR 111 (tokens):
    // append-only event journals a loop pumps.
    s.zone_changes_this_turn.clear();
    s.battlefield_entries_this_turn.clear();
    s.created_tokens_this_turn.clear();
    s.players_who_created_token_this_turn.clear();
    s.sacrificed_permanents_this_turn.clear();
    s.players_who_sacrificed_artifact_this_turn.clear();
    s.counter_added_this_turn.clear();
    s.player_actions_this_turn.clear();
    // CR 506 / CR 500.8: combat/phase tallies an extra-combat loop pumps.
    s.combat_phases_started_this_turn = 0;
    s.end_steps_started_this_turn = 0;

    // CR 104.4b / CR 732.2a — MODULO LAYER ONLY. The strict `loop_states_equal` /
    // `normalize_for_loop` are deliberately NOT changed; they never call this fn
    // (`project_out_resources` is reached only via `loop_states_equal_modulo_resources`).
    //
    // A triggered/activated ability placed on the stack takes a FRESH
    // `entry_id = ObjectId(next_object_id++)` every time it goes on the stack, and
    // `StackEntry`/`GameState` `PartialEq` compare that id. A MANDATORY trigger
    // cascade (e.g. Marauding Blight-Priest + Bloodthirsty Conqueror) holds one
    // in-loop trigger on the stack at every priority window (the stack never empties
    // between resolutions), so two same-phase cycle points differ ONLY in this
    // volatile id and never compare modulo-equal — the loop is invisible to the
    // modulo scan. Canonicalize the id to its stack POSITION (the modulo analogue of
    // `normalize_for_loop` zeroing `next_object_id`) while PRESERVING
    // source_id/controller/kind, so different triggers/spells from different sources
    // at the same depth still compare UNEQUAL.
    //
    // What is STILL compared element-wise inside `kind` (and is therefore the real
    // discriminator, left intentionally untouched): for a `TriggeredAbility` the
    // `trigger_event` (`GameEvent::LifeChanged { player_id, amount }` for the drain
    // class — no volatile id, constant amount per cycle), `subject_match_count`, and
    // `die_result`, plus the boxed `ability` and `condition`. These are CONTENT, not
    // bookkeeping: a residual difference in any of them only makes the two states
    // compare UNEQUAL, which SUPPRESSES a match — fail-safe (never a false win). The
    // `stack_trigger_firings` is the one sidecar indexed by the fresh stack-entry
    // id, so canonicalize it with the stack. The firing kind remains significant:
    // CR 603.7 keeps delayed and ordinary trigger firings distinct. A delayed
    // provenance receipt is monotonic installation history, however, so it is
    // reduced to the same legacy-delayed marker as `normalize_for_loop`. The same
    // fail-safe direction holds for any other state field that still references a
    // raw stack id (`stack_paid_facts`, `pending_trigger_entry`, a `WaitingFor`
    // carrying a stack-entry id): left AS-IS, a residual mismatch can only suppress
    // a match.
    // Canonicalizing the position id can therefore never MANUFACTURE a false positive
    // (a wrongful win); it can only make a genuine repeat visible.
    let mut trigger_firings = std::mem::take(&mut s.stack_trigger_firings);
    for (pos, entry) in s.stack.iter_mut().enumerate() {
        let original_id = entry.id;
        let canonical_id = ObjectId(pos as u64);
        entry.id = canonical_id;
        if let Some(firing) = trigger_firings.remove(&original_id) {
            let firing = match firing {
                TriggerFiring::ReceiptEligible(_) => TriggerFiring::LegacyDelayed,
                firing => firing,
            };
            s.stack_trigger_firings.insert(canonical_id, firing);
        }
    }
    s
}

/// The controller-side raw values of the PROJECTED scalar player consumables, in a
/// fixed order matching [`project_out_player_consumables`]' zeroing. The no-`..`
/// destructure means the sign-check cannot silently miss a newly-projected scalar.
/// `life`/`mana_pool` are bound `_` (their sign is the sole authority of
/// `ResourceVector::net_progress_for` — not re-vetoed here, to avoid dual authority);
/// `player_counters` is a map-typed consumable, so it is bound `_` here and returned by the
/// SEPARATE no-`..` [`projected_player_maps`] (its own structural totality guard), then
/// compared per-kind by [`driving_resources_non_decreasing`]. The two no-`..` destructures
/// PARTITION the projected consumables (scalars here, maps there) with no field double-bound
/// or dropped.
#[cfg_attr(not(test), allow(dead_code))] // 4d-ii wires the live/offline caller; 4d-i exercises via unit tests.
fn projected_player_axes(p: &Player) -> Vec<i64> {
    let Player {
        poison_counters,
        energy,
        life_gained_this_turn,
        life_lost_this_turn,
        cards_drawn_this_turn,
        cards_drawn_this_step,
        life: _,
        mana_pool: _,
        player_counters: _,
        // Strict-equality fields, no-`..`:
        id: _,
        library: _,
        hand: _,
        graveyard: _,
        attraction_deck: _,
        contraption_deck: _,
        contraption_crank_sprocket: _,
        sticker_sheets: _,
        has_drawn_this_turn: _,
        lands_played_this_turn: _,
        life_lost_last_turn: _,
        descended_this_turn: _,
        speed: _,
        speed_trigger_used_this_turn: _,
        crimes_committed_this_turn: _,
        drew_from_empty_library: _,
        turns_taken: _,
        is_eliminated: _,
        bending_types_this_turn: _,
        status: _,
        companion: _,
        chosen_attributes: _,
        can_look_at_top_of_library: _,
        commander_color_identity: _,
    } = p;
    vec![
        *poison_counters as i64,
        *energy as i64,
        *life_gained_this_turn as i64,
        *life_lost_this_turn as i64,
        *cards_drawn_this_turn as i64,
        *cards_drawn_this_step as i64,
    ]
}

/// CR 122.1: the controller-side MAP-typed PROJECTED player consumables (today only
/// `player_counters`), in a fixed order. The no-`..` destructure (the map-typed mirror of
/// [`projected_player_axes`]) is the structural tie that BUILD-BREAKS the moment a second
/// map-typed projected consumable is added — forcing the author to thread it into
/// [`driving_resources_non_decreasing`]'s per-kind veto too, so a new map consumable can
/// never be zeroed by [`project_out_player_consumables`] yet silently escape the sign-check
/// (closes BLOCKER-2's "one field over" latent gap). Returns references so the caller unions
/// keys without cloning.
#[cfg_attr(not(test), allow(dead_code))] // 4d-ii wires the live/offline caller; 4d-i exercises via unit tests.
fn projected_player_maps(
    p: &Player,
) -> Vec<&HashMap<crate::types::player::PlayerCounterKind, u32>> {
    let Player {
        player_counters,
        // Scalar-projected + strict-equality fields (handled elsewhere), no-`..`:
        life: _,
        mana_pool: _,
        poison_counters: _,
        energy: _,
        life_gained_this_turn: _,
        life_lost_this_turn: _,
        cards_drawn_this_turn: _,
        cards_drawn_this_step: _,
        id: _,
        library: _,
        hand: _,
        graveyard: _,
        attraction_deck: _,
        contraption_deck: _,
        contraption_crank_sprocket: _,
        sticker_sheets: _,
        has_drawn_this_turn: _,
        lands_played_this_turn: _,
        life_lost_last_turn: _,
        descended_this_turn: _,
        speed: _,
        speed_trigger_used_this_turn: _,
        crimes_committed_this_turn: _,
        drew_from_empty_library: _,
        turns_taken: _,
        is_eliminated: _,
        bending_types_this_turn: _,
        status: _,
        companion: _,
        chosen_attributes: _,
        can_look_at_top_of_library: _,
        commander_color_identity: _,
    } = p;
    vec![player_counters]
}

/// CR 122.1 / CR 119 / CR 106.1: BLOCKER-2 structural sign-check — every projected
/// controller consumable is non-decreasing across the driven pair. This closes the
/// hole where `project_out_resources` erases `energy` / `player_counters` (and
/// monotone OBJECT counters) from strict loop equality with no summed-vector gate
/// recovering their sign. Blanket fail-closed veto over the compiler-total projected
/// set (§6.2): any enumerated axis with `current < prior` ⇒ `false`. Same-turn
/// `MonotoneHistory` axes (life_gained/…) never decrease, so the blanket veto never
/// false-rejects the fodder class; true consumables (energy / poison / per-kind
/// player_counters / monotone object counters) reject on any decrease.
///
/// MUST read RAW (un-projected) frames — `project_out_resources` zeroed these, so the
/// caller passes the raw settle frames (4d-ii) / raw synthetic states (4d-i tests).
pub(crate) fn driving_resources_non_decreasing(
    prior: &GameState,
    current: &GameState,
    controller: PlayerId,
) -> bool {
    // CR 119: no `GameState::player` accessor exists — find by id (per §6.3 fallback).
    let (Some(pp), Some(cp)) = (
        prior.players.iter().find(|p| p.id == controller),
        current.players.iter().find(|p| p.id == controller),
    ) else {
        return false;
    };
    // (a) scalar projected axes — positional zip (fixed order).
    if projected_player_axes(cp)
        .into_iter()
        .zip(projected_player_axes(pp))
        .any(|(cur, pri)| cur < pri)
    {
        return false;
    }
    // (b) CR 122.1 per-kind MAP-typed consumables: union keys, veto any decrease. Driven
    //     from `projected_player_maps` (no-`..`) rather than hardcoding `player_counters`, so
    //     a future 2nd map consumable BUILD-BREAKS `projected_player_maps` until it is threaded
    //     here too (the structural tie closing BLOCKER-2's "one field over" gap). The two Vecs
    //     zip index-for-index (same destructure order on both frames).
    for (cur_map, pri_map) in projected_player_maps(cp)
        .into_iter()
        .zip(projected_player_maps(pp))
    {
        for kind in pri_map.keys().chain(cur_map.keys()) {
            if cur_map.get(kind).copied().unwrap_or(0) < pri_map.get(kind).copied().unwrap_or(0) {
                return false;
            }
        }
    }
    // (c) monotone OBJECT-counter per-kind totals on the CONTROLLER's permanents
    //     (project_out_resources erases these — the object-side analogue of the
    //     player-consumable hole). CR 122.1a / CR 613.4c +1/+1, CR 306.5c loyalty,
    //     CR 310.4c defense. Per-KIND totals (not one summed total) so kind-A↓ /
    //     kind-B↑ cannot mask a real per-kind depletion. `damage_marked` is NOT vetoed
    //     (a decrease is a beneficial heal).
    let totals = |s: &GameState| -> HashMap<CounterType, u64> {
        let mut m: HashMap<CounterType, u64> = HashMap::default();
        for id in &s.battlefield {
            if let Some(o) = s.objects.get(id) {
                if o.controller != controller {
                    continue;
                }
                for (ct, n) in &o.counters {
                    if ct.is_monotone_loop_resource() {
                        *m.entry(ct.clone()).or_insert(0) += *n as u64;
                    }
                }
            }
        }
        m
    };
    let (pt, ct) = (totals(prior), totals(current));
    for kind in pt.keys().chain(ct.keys()) {
        if ct.get(kind).copied().unwrap_or(0) < pt.get(kind).copied().unwrap_or(0) {
            return false;
        }
    }
    // (d) CR 704.5g: veto a controller-side `damage_marked` INCREASE (carry b). OPPOSITE
    //     polarity to the consumables above — a creature whose total marked damage reaches
    //     its toughness is destroyed, so a board-growing loop that ALSO accrues damage on the
    //     controller's own engine each cycle is self-terminating, not a sustainable CR 732.2a
    //     shortcut. `project_out_resources` zeroes `damage_marked` (invisible to strict
    //     loop-equality); this recovers the sign. Summed across the controller's battlefield
    //     (damage is one scalar per object, no per-kind split). A DECREASE (heal) is allowed —
    //     orthogonal to 4d-i's `sign_check_damage_marked_heal_not_vetoed`.
    let damage_total = |s: &GameState| -> u64 {
        s.battlefield
            .iter()
            .filter_map(|id| s.objects.get(id))
            .filter(|o| o.controller == controller)
            .map(|o| o.damage_marked as u64)
            .sum()
    };
    if damage_total(current) > damage_total(prior) {
        return false;
    }
    true
}

/// CR 602.5b: does the ability at `key=(source,index)` carry a PER-TURN activation
/// gate? Single authority for "is this activated-tally key a per-turn gate?".
/// Exhaustive-by-listing `matches!` (no wildcard) so a future per-turn restriction
/// variant forces an explicit keep/drop decision. A key whose source object is
/// absent (un-activatable, gate moot) is treated as not-gated and projected out.
fn ability_has_per_turn_activation_gate(state: &GameState, key: &(ObjectId, usize)) -> bool {
    state
        .objects
        .get(&key.0)
        .and_then(|o| o.abilities.get(key.1))
        .is_some_and(|def| {
            def.activation_restrictions.iter().any(|r| {
                matches!(
                    r,
                    ActivationRestriction::OnlyOnceEachTurn
                        | ActivationRestriction::MaxTimesEachTurn { .. }
                )
            })
        })
}

/// CR 602.5b: per-GAME activation gate. Single authority.
fn ability_has_per_game_activation_gate(state: &GameState, key: &(ObjectId, usize)) -> bool {
    state
        .objects
        .get(&key.0)
        .and_then(|o| o.abilities.get(key.1))
        .is_some_and(|def| {
            def.activation_restrictions
                .iter()
                .any(|r| matches!(r, ActivationRestriction::OnlyOnce))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::ability::TriggerDefinitionRef;
    use crate::types::identifiers::{
        CardId, DelayedTriggerInstanceId, DelayedTriggerOrigin, DelayedTriggerToken,
    };
    use crate::types::zones::Zone;

    fn pid(n: u8) -> PlayerId {
        PlayerId(n)
    }

    fn battlefield_creature(state: &mut GameState, id: u64, controller: u8) -> ObjectId {
        let oid = ObjectId(id);
        let mut object = GameObject::new(
            oid,
            CardId(1),
            PlayerId(controller),
            "Walking Ballista".to_string(),
            Zone::Battlefield,
        );
        object.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
        state.objects.insert(oid, object);
        state.battlefield.push_back(oid);
        oid
    }

    /// Inflate one of the real 4p dumps through the PRODUCTION decoder — the same
    /// chokepoint the server's `from_persisted` and WASM's `decode_restored_game_state`
    /// funnel through, never a bare `GameState` decode.
    fn dump_state(gz: &[u8]) -> GameState {
        use std::io::Read;
        let mut json = String::new();
        flate2::read::GzDecoder::new(gz)
            .read_to_string(&mut json)
            .expect("fixture .json.gz must inflate to UTF-8 JSON");
        let envelope: serde_json::Value =
            serde_json::from_str(&json).expect("dump envelope parses as JSON");
        serde_json::from_value::<crate::types::game_state::PersistedGameState>(
            envelope["gameState"].clone(),
        )
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
    }

    /// One beat of the shared dump drive policy (`tests/integration/loop_shortcut.rs`'s
    /// `dump_drive_one_beat`): at `Priority` always pass — the mandatory triggers resolve
    /// and re-trigger, which IS the loop when there is one — otherwise take the first
    /// legal non-terminal action. Every beat crosses `apply()`.
    fn dump_drive_one_beat(state: &mut GameState) -> Result<(), String> {
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;

        let actor = state
            .waiting_for
            .acting_player()
            .into_iter()
            .chain(state.players.iter().map(|p| p.id))
            .find_map(|p| {
                let (actions, _costs, _grouped) =
                    crate::ai_support::legal_actions_for_viewer(state, p);
                (!actions.is_empty()).then_some((p, actions))
            });
        let Some((who, actions)) = actor else {
            return Err(format!("no legal actor at {:?}", state.waiting_for));
        };
        let forbidden =
            |a: &GameAction| matches!(a, GameAction::Concede { .. } | GameAction::Debug(_));
        let chosen = if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            actions
                .iter()
                .find(|a| matches!(a, GameAction::PassPriority))
        } else {
            actions
                .iter()
                .find(|a| !matches!(a, GameAction::PassPriority) && !forbidden(a))
                .or_else(|| actions.iter().find(|a| !forbidden(a)))
        };
        let Some(action) = chosen.cloned() else {
            return Err(format!("empty action list at {:?}", state.waiting_for));
        };
        crate::game::engine::apply(state, who, action.clone())
            .map(|_| ())
            .map_err(|e| format!("apply err ({action:?}): {e:?}"))
    }

    /// A frozen-set LOWER BOUND computed INDEPENDENTLY of the function under test: the
    /// longest common prefix, by object id, of every window frame's stack and `current`'s.
    ///
    /// Sound because it is a strictly weaker computation than the predicate it bounds —
    /// `certified_period_touch` freezes an id iff it sits at the SAME INDEX in every window
    /// frame, and every position inside a common prefix satisfies that by construction. It
    /// is a prefix scan that stops at the first disagreement, not a per-index filter over
    /// the whole stack, so it cannot degenerate into `f(x) == f(x)` against the callee.
    fn frozen_lower_bound(window: &[&GameState], current: &GameState) -> usize {
        (0..current.stack.len())
            .take_while(|&i| {
                let id = current.stack[i].id;
                window
                    .iter()
                    .all(|f| f.stack.get(i).map(|e| e.id) == Some(id))
            })
            .count()
    }

    /// The two 4p dumps every row in this module drives with the GENERIC policy
    /// ([`dump_drive_one_beat`]: pass at `Priority`, else the first legal non-terminal action),
    /// as `(name, gzip)` pairs.
    ///
    /// `fantastic_four_bounded_loop_4p.json.gz` is now tracked too (5d U5), and is deliberately
    /// NOT listed here: MEASURED, the generic policy never reaches its loop at all — that
    /// helper's victim preference matches `GameAction::SelectTargets` while the F4 dump raises
    /// `GameAction::ChooseTarget`, and its fallback answers Invisible Woman's CR 603.5 "may"
    /// with whichever `DecideOptionalEffect` is enumerated first, which breaks the chain to
    /// Mister Fantastic. Adding it here would add a dump on which these rows measure nothing.
    /// Its rows live in `tests/integration/fantastic_four_bounded_loop.rs`, with the drive
    /// policy that dump requires.
    const TRACKED_DUMPS: [(&str, &[u8]); 2] = [
        (
            "dina",
            include_bytes!("../../tests/fixtures/dina_conqueror_4p.json.gz"),
        ),
        (
            "dellian",
            include_bytes!("../../tests/fixtures/dellian_emblem_conqueror_4p.json.gz"),
        ),
    ];

    /// Drive one dump through `apply()` until `pred` accepts the board, returning the beat
    /// index and that board.
    ///
    /// The beat is SEARCHED by its construction requirements, never hardcoded — a hardcoded
    /// index is a fixture that drifts silently when the drive policy moves. `None` ⇒ no beat
    /// within `max_beats` satisfied them, which every caller turns into a loud failure rather
    /// than a vacuous pass.
    fn drive_dump_until(
        gz: &[u8],
        max_beats: usize,
        pred: impl Fn(&GameState) -> bool,
    ) -> Option<(usize, GameState)> {
        let mut state = dump_state(gz);
        for beat in 0..max_beats {
            if pred(&state) {
                return Some((beat, state));
            }
            if dump_drive_one_beat(&mut state).is_err() {
                return None;
            }
        }
        None
    }

    /// PRODUCTION'S OWN CANDIDATE WALK, CONSUMED rather than imitated: this asks
    /// [`crate::game::engine::candidate_windows`] — the very iterator
    /// `certified_bounded_cycle_offer` walks — so the ORDER (newest-first), the span
    /// arithmetic and the degenerate-pair filter are production's by construction. Returns
    /// the first `idx` whose window drives item (4) AT ALL, with that window's certified
    /// touch and its measured span.
    ///
    /// CR 732.2a: `frames_per_period` is a MEASURED span, so a row that hard-codes
    /// `&live[live.len() - 2..]` is asserting `span == 1` without saying so — and reads a
    /// HALF PERIOD the moment the sampling rate moves. It moved: the answer-beat sampler
    /// retains a frame at forced-window ANSWER beats as well, which halves the newest
    /// adjacent pair on the dellian dump.
    ///
    /// **PRECONDITION, NOT CONCLUSION.** `conjunct4_scans() > 0` says items (1)/(2)/(3)
    /// passed on SOME production-shaped window so item (4) RAN; it admits `scans ∈ {1, 2}`,
    /// which FAILS the caller's `scans > non_exempt` assertion. The search can therefore land
    /// on a beat whose assertion fails — that is what keeps the caller non-vacuous.
    ///
    /// One FRESH container per candidate: the counter is cumulative, so a shared container
    /// (which is what production carries) would make `> 0` unattributable after the first
    /// candidate that scans.
    fn newest_item4_window<'a>(
        live: &[&'a GameState],
        current: &'a GameState,
        proposer: PlayerId,
    ) -> Option<(usize, PeriodTouch<'a>, u32)> {
        for (idx, span, window) in crate::game::engine::candidate_windows(live) {
            let touch = certified_period_touch(window, current, PeriodCertification::BoardCovered);
            let mut verdicts = PeriodVerdicts::for_period(live, current, proposer);
            let _ = loop_states_cover_modulo_growth_pinned(
                window[0],
                current,
                proposer,
                &[],
                &touch,
                &mut verdicts,
            );
            if verdicts.conjunct4_scans() > 0 {
                return Some((idx, touch, span));
            }
        }
        None
    }

    /// The construction requirement shared by every row that needs a REAL certified window
    /// carrying a non-empty observed-frozen prefix: a usable ring AND a newest candidate pair
    /// (`span == 1`, the shape §3 D2's walk reaches first) whose common prefix is
    /// index-stable.
    ///
    /// ⚠ THIS PREDICATE HARD-CODES `span == 1`, and the residual that leaves is SMALLER IN
    /// COUNT AND LARGER IN KIND than "four authored-ring rows" claimed. MEASURED: exactly TWO
    /// call sites remain — `r21_b_the_exemption_narrows_conjunct_six_by_exactly_the_frozen_set`
    /// and `r27_a2_every_announced_pair_carries_an_unnormalized_evaluation_board` — and
    /// NEITHER is an authored ring. Both drive the REAL `TRACKED_DUMPS` through
    /// `drive_dump_until`, so both are exposed to the answer-beat sampler that can halve the
    /// newest adjacent pair (the hazard [`newest_item4_window`] exists for). They stay because
    /// at the beat this predicate SELECTS the hardcode is EXACT — not because a half period
    /// would fail loudly, which it would not.
    ///
    /// MEASURED at the beat `drive_dump_until(gz, 80, has_frozen_window)` returns: `dina` beat
    /// 6, `ring = 2`; `dellian` beat 5, `ring = 2`; and `candidate_windows` yields exactly ONE
    /// candidate on each — `idx = 0`, `span = 1`, window length 2. With a two-frame ring
    /// `&live[live.len() - 2..]` IS the whole ring, so there is no half period to read here.
    ///
    /// ⚠ THE REASON THAT STOOD HERE — *"both fail LOUD rather than silently on a half period …
    /// each then asserts its own domain non-empty"* — IS UNPROVEN AND WRONG, and is replaced
    /// rather than softened. The loud floors are real but do not cover this hazard: a half
    /// period is a NON-degenerate window with non-empty domains, so neither
    /// `drive_dump_until`'s reach-guard nor either row's domain-non-empty assertion
    /// (`frozen_ids`/`announced` in the first, `announced` in the second) would fire on one,
    /// and both rows' claims (a set-narrowing identity; a universal over announced pairs) are
    /// span-INDEPENDENT, so on a half period they would PASS. The residual is therefore "exact
    /// today, SILENT the day the sampling rate grows this ring past two frames" — a smaller
    /// hazard than the old text claimed to have closed, and an honest one. The real-dump
    /// item-(4) rows, which have no such measured guarantee, use [`newest_item4_window`]
    /// instead.
    fn has_frozen_window(state: &GameState) -> bool {
        if state.loop_detect_ring.len() < 2 {
            return false;
        }
        let live: Vec<&GameState> = state.loop_detect_ring.iter().map(|f| &f.live).collect();
        frozen_lower_bound(&live[live.len() - 2..], state) > 0
    }

    fn test_trigger_ref(state: &GameState, object_id: ObjectId) -> TriggerDefinitionRef {
        let object = &state.objects[&object_id];
        TriggerDefinitionRef {
            source: crate::types::identifiers::ObjectIncarnationRef::from_object(object),
            occurrence: crate::types::ability::TriggerDefinitionOccurrenceRef::Printed {
                base_set: object.trigger_base_set_instance,
                printed_index: 0,
            },
        }
    }

    /// Insert a battlefield permanent with a chosen `tapped` state (B4 `board_delta`
    /// fixtures). Distinct `card_id` per `id` so no fixture accidentally shares identity.
    fn bf_obj(state: &mut GameState, id: u64, controller: u8, tapped: bool) {
        let oid = ObjectId(id);
        let mut object = GameObject::new(
            oid,
            CardId(id),
            PlayerId(controller),
            "Token".into(),
            Zone::Battlefield,
        );
        object.tapped = tapped;
        state.objects.insert(oid, object);
    }

    /// Insert a named battlefield permanent with a chosen `tapped` state AND push it
    /// onto `state.battlefield` (fodder-pile fixtures iterate the battlefield vector).
    fn named_bf(
        state: &mut GameState,
        id: u64,
        controller: u8,
        name: &str,
        tapped: bool,
    ) -> ObjectId {
        let oid = ObjectId(id);
        let mut object = GameObject::new(
            oid,
            CardId(id),
            PlayerId(controller),
            name.to_string(),
            Zone::Battlefield,
        );
        object.tapped = tapped;
        state.objects.insert(oid, object);
        state.battlefield.push_back(oid);
        oid
    }

    /// DESIGN STEP 4 (∞-pile): `tapped_fodder_members` returns exactly the winning
    /// controller's *tapped* fodder-class members — not untapped fodder, not
    /// non-fodder permanents, not an opponent's tapped fodder.
    ///
    /// REVERT-PROBE: drop the `o.tapped` conjunct in `tapped_fodder_members` → the
    /// untapped P0 Saproling (id 3) leaks into the set → `assert_eq` below fails.
    #[test]
    fn tapped_fodder_members_returns_only_controllers_tapped_fodder() {
        let mut state = GameState::new_two_player(7);
        let t1 = named_bf(&mut state, 1, 0, "Saproling", true); // P0 tapped fodder
        let t2 = named_bf(&mut state, 2, 0, "Saproling", true); // P0 tapped fodder
        let _untapped = named_bf(&mut state, 3, 0, "Saproling", false); // P0 UNtapped fodder
        let _land = named_bf(&mut state, 4, 0, "Forest", true); // P0 tapped NON-fodder
        let _opp = named_bf(&mut state, 5, 1, "Saproling", true); // opponent tapped fodder

        // Fodder class: content-equal (modulo tapped) to the P0 Saprolings.
        let class = GameObject::new(
            ObjectId(999),
            CardId(999),
            PlayerId(0),
            "Saproling".to_string(),
            Zone::Battlefield,
        );

        let pile = tapped_fodder_members(&state, pid(0), &class);
        assert_eq!(
            pile,
            BTreeSet::from([t1, t2]),
            "only P0's tapped Saprolings; untapped/non-fodder/opponent excluded"
        );
    }

    /// T10 (B4 core): `board_delta` isolates the one untapped seed a net-object-progress
    /// loop adds, and nets out recycled tapped tokens present in BOTH frames.
    #[test]
    fn board_delta_isolates_untapped_seed() {
        let mut before = GameState::new_two_player(7);
        bf_obj(&mut before, 700, 0, true); // recycled tapped body...
        bf_obj(&mut before, 701, 0, true); // ...present in both frames

        let mut after = before.clone();
        bf_obj(&mut after, 702, 0, false); // the extra untapped seed

        let delta = board_delta(&before, &after);
        assert_eq!(
            delta.added.len(),
            1,
            "only the new seed is added; recycled tokens (in both) net out"
        );
        assert!(
            !delta.added[0].tapped,
            "the isolated seed is untapped — a pre-BoardDelta path drops this object entirely"
        );
        assert!(delta.removed.is_empty(), "nothing left the battlefield");
    }

    /// T11 (B4): `board_delta` reports the correct tap-state split — a tap-state-blind
    /// diff would report the right count with wrong flags.
    #[test]
    fn board_delta_reports_tapped_split() {
        let mut before = GameState::new_two_player(7);
        bf_obj(&mut before, 700, 0, true); // recycled body in both

        let mut after = before.clone();
        bf_obj(&mut after, 800, 0, false); // 1 untapped seed
        bf_obj(&mut after, 801, 0, true); // 2 tapped tokens
        bf_obj(&mut after, 802, 0, true);

        let delta = board_delta(&before, &after);
        assert_eq!(delta.added.len(), 3);
        assert_eq!(
            delta.added.iter().filter(|r| !r.tapped).count(),
            1,
            "exactly one untapped seed"
        );
        assert_eq!(
            delta.added.iter().filter(|r| r.tapped).count(),
            2,
            "exactly two tapped tokens"
        );
    }

    /// Battlefield creature carrying exactly one activated ability whose
    /// `activation_restrictions` is `restrictions` — production shape the gate
    /// predicates run against (`o.abilities.get(idx).activation_restrictions`).
    fn battlefield_creature_with_restrictions(
        state: &mut GameState,
        id: u64,
        controller: u8,
        restrictions: Vec<ActivationRestriction>,
    ) -> ObjectId {
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};
        use std::sync::Arc;

        let oid = battlefield_creature(state, id, controller);
        let mut def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::unimplemented("gate-test", "activated"),
        );
        def.activation_restrictions = restrictions;
        state.objects.get_mut(&oid).unwrap().abilities = Arc::new(vec![def]);
        oid
    }

    /// CR 104.4b vs CR 732.2a: two byte-identical states must compare equal under
    /// BOTH the strict equality and the resource-modulo equality.
    #[test]
    fn identical_states_equal_under_both_comparisons() {
        let mut state = GameState::new_two_player(7);
        battlefield_creature(&mut state, 500, 0);
        let copy = state.clone();

        assert!(
            loop_states_equal(&state.normalize_for_loop(), &copy.normalize_for_loop()),
            "identical states must be strictly equal"
        );
        assert!(
            loop_states_equal_modulo_resources(&state, &copy),
            "identical states must be modulo-resources equal"
        );
    }

    /// THE KEY DISCRIMINATOR (CR 732.2a vs CR 104.4b): same board but different
    /// life, mana, and counters must be **modulo-resources equal** (a beneficial
    /// loop point) yet **strictly unequal** (not a mandatory-draw loop). This is
    /// the entire reason the modulo comparison exists; reverting the resource
    /// projection makes the modulo assertion fail.
    #[test]
    fn same_board_different_resources_is_modulo_equal_but_strictly_unequal() {
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 500, 0);

        let mut b = a.clone();
        // Drain a life point, float a red mana, add a +1/+1 counter, mark damage.
        b.players[1].life -= 1;
        b.players[0].life += 1;
        b.players[0]
            .mana_pool
            .add(crate::types::mana::ManaUnit::new(
                ManaType::Red,
                oid,
                false,
                Vec::new(),
            ));
        if let Some(o) = b.objects.get_mut(&oid) {
            o.counters.insert(CounterType::Plus1Plus1, 3);
            o.damage_marked = 2;
        }

        assert!(
            !loop_states_equal(&a.normalize_for_loop(), &b.normalize_for_loop()),
            "differing life/mana/counters must NOT be strictly equal (else a wrongful CR 104.4b draw)"
        );
        assert!(
            loop_states_equal_modulo_resources(&a, &b),
            "same board with only monotone resources differing must be modulo-resources equal (CR 732.2a net-progress loop point)"
        );
    }

    /// BLOCKER 1 (CR 122.1c): a CONSUMED non-monotone counter (shield, 2 -> 1)
    /// plus a projected-out resource gain must keep two boards modulo-UNEQUAL —
    /// the finite counter makes the cycle non-repeatable. PAIRED positive control:
    /// a board differing only by a MONOTONE +1/+1 (CR 122.1a) plus the same
    /// resource gain stays modulo-EQUAL, proving the partition projects monotone
    /// counters out without erasing consumable ones.
    #[test]
    fn consumed_shield_counter_breaks_modulo_equality_but_monotone_does_not() {
        // --- Negative: consumed shield counter keeps boards unequal ---
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 500, 0);
        a.objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Shield, 2);
        let mut b = a.clone();
        b.objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Shield, 1); // consumed one shield
        b.players[1].life -= 1; // projected-out resource gain
        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "a consumed shield counter (CR 122.1c) makes the cycle non-repeatable; \
             boards must NOT be modulo-equal even though only a resource also changed"
        );

        // --- Positive control: only a monotone +1/+1 differs => still equal ---
        let mut c = GameState::new_two_player(7);
        let oid2 = battlefield_creature(&mut c, 600, 0);
        let mut d = c.clone();
        d.objects
            .get_mut(&oid2)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 3);
        d.players[1].life -= 1;
        assert!(
            loop_states_equal_modulo_resources(&c, &d),
            "only a monotone +1/+1 pump (CR 122.1a) plus a resource delta must stay modulo-equal"
        );
    }

    /// PR-7 #1: a board differing ONLY by a strictly-grown preserved `Generic`
    /// charge counter (CR 122.1) is COVERED by the counter-growth predicate — and is
    /// NOT caught by the plain equality path (Generic is PRESERVED, so the growing
    /// charge makes `loop_states_equal_modulo_resources` return false). The pairing
    /// proves the cover does real work rather than shadowing the equality path.
    #[test]
    fn counter_growth_covers_strict_generic_charge_growth() {
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 500, 0);
        a.objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 3);
        let mut b = a.clone();
        b.objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 4); // +1 charge

        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "a growing preserved Generic charge counter must NOT be plain-equal (else no cover is needed)"
        );
        assert!(
            loop_states_cover_modulo_counter_growth(&a, &b),
            "strict Generic charge growth (CR 122.1) must be covered (CR 732.2a)"
        );
    }

    /// PR-7 #2: a CONSUMED `Generic` charge counter (2 -> 1) is REJECTED — an
    /// ∞-consume trap, not an unbounded pump (fail-closed).
    ///
    /// NON-VACUITY (A1, direction-blind revert): the discriminating revert is making
    /// `classify_generic_counter_growth` treat ANY nonzero Generic delta as growth
    /// (dropping the `a < b => Consumed` SIGN discrimination as a whole). Under that
    /// revert the consume classifies `StrictGrowth`, `equalize_generic_counters`
    /// restores prior's charge, and the cover returns TRUE — flipping this assertion.
    /// Deleting ONLY the early-return would classify `Stable`, which STILL rejects, so
    /// this test discriminates the SIGN, not merely the branch's presence.
    #[test]
    fn counter_growth_rejects_consumed_generic_charge() {
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 500, 0);
        a.objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 2);
        let mut b = a.clone();
        b.objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 1); // consumed one charge

        assert!(
            !loop_states_cover_modulo_counter_growth(&a, &b),
            "a consumed Generic charge counter is an ∞-consume trap, not a pump — must reject (fail-closed)"
        );
    }

    /// PR-7 #3: a STABLE board (charge unchanged) is REJECTED by the counter-growth
    /// cover — a constant-depth loop is the equality path's job, not this one. Paired:
    /// the same two states ARE plain-equal, proving the reject is the strict-growth-
    /// only gate (no Generic motion), not a board difference.
    #[test]
    fn counter_growth_rejects_stable_charge() {
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 500, 0);
        a.objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 3);
        let b = a.clone(); // charge unchanged

        assert!(
            loop_states_equal_modulo_resources(&a, &b),
            "an unchanged charge board is plain-equal (the equality path's domain)"
        );
        assert!(
            !loop_states_cover_modulo_counter_growth(&a, &b),
            "no Generic growth => strict-growth-only gate rejects (Stable is the equality path's job)"
        );
    }

    /// PR-7 #4: a grown non-`Generic` PRESERVED counter (`Stun`, CR 122.1d) is
    /// REJECTED — only `Generic` is a growable pump axis; a stun counter gates the
    /// untap SBA, so its growth is a real board change, not an unbounded resource.
    ///
    /// NON-VACUITY: a POSITIVE control with the SAME shape but a `Generic` counter
    /// growing by the same amount IS covered — proving the per-`CounterType` table
    /// discriminates `Generic` from the preserved-non-`Generic` class, not merely
    /// that "some counter changed".
    #[test]
    fn counter_growth_rejects_non_generic_preserved_counter_growth() {
        // Negative: stun growth is not a pump axis.
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 500, 0);
        a.objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);
        let mut b = a.clone();
        b.objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 2); // stun grew

        assert!(
            !loop_states_cover_modulo_counter_growth(&a, &b),
            "a grown Stun counter (CR 122.1d) is a real board change, not a Generic pump — must reject"
        );

        // Positive control: same shape, a Generic counter grows => covered.
        let mut c = GameState::new_two_player(7);
        let oid2 = battlefield_creature(&mut c, 600, 0);
        c.objects
            .get_mut(&oid2)
            .unwrap()
            .counters
            .insert(CounterType::Generic("oil".to_string()), 1);
        let mut d = c.clone();
        d.objects
            .get_mut(&oid2)
            .unwrap()
            .counters
            .insert(CounterType::Generic("oil".to_string()), 2);
        assert!(
            loop_states_cover_modulo_counter_growth(&c, &d),
            "same shape with a Generic oil counter growing IS covered (per-type table discriminates)"
        );
    }

    /// BLOCKER 2 (CR 121.4 / CR 704.5b): a pure mill delta (only a negative
    /// library_delta) is net progress. Controls: an empty delta is not progress,
    /// and the consumed-axis guard still rejects a loop that net-loses life.
    #[test]
    fn pure_mill_delta_is_net_progress() {
        let mut mill = ResourceVector::default();
        mill.library_delta.insert(pid(1), -4);
        assert!(
            mill.is_net_progress(),
            "a pure mill loop (only negative library_delta) is net progress (CR 121.4)"
        );

        assert!(
            !ResourceVector::default().is_net_progress(),
            "an empty delta is not net progress"
        );

        // Consumed-axis guard intact: a mill that net-loses life is rejected.
        let mut mill_bleed = ResourceVector::default();
        mill_bleed.library_delta.insert(pid(1), -4);
        mill_bleed.life.insert(pid(0), -1);
        assert!(
            !mill_bleed.is_net_progress(),
            "a loop that net-spends a consumed axis (life) is not sustainable"
        );
    }

    /// A real board difference (an extra permanent) must make even the
    /// resource-modulo comparison return false — the projection must not blur
    /// genuine board changes.
    #[test]
    fn extra_permanent_is_not_modulo_equal() {
        let mut a = GameState::new_two_player(7);
        battlefield_creature(&mut a, 500, 0);
        let mut b = a.clone();
        battlefield_creature(&mut b, 501, 0);

        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "an extra permanent is a genuine board change, not a resource difference"
        );
    }

    /// A different tap state is a genuine board difference (tap/untap loop phase)
    /// — modulo-resources must NOT blur it.
    #[test]
    fn different_tap_state_is_not_modulo_equal() {
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 500, 0);
        let mut b = a.clone();
        if let Some(o) = b.objects.get_mut(&oid) {
            o.tapped = true;
        }

        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "a tapped-vs-untapped object is a board difference, not a resource difference"
        );
    }

    /// `snapshot` reads life, mana, library size, and counters directly out of a
    /// `GameState`; `delta` then measures a known monotone change exactly.
    #[test]
    fn snapshot_and_delta_measure_known_changes() {
        let mut before_state = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut before_state, 500, 0);
        let before = ResourceVector::snapshot(&before_state);

        let mut after_state = before_state.clone();
        after_state.players[1].life -= 5; // opponent took 5 (drain)
        after_state.players[0]
            .mana_pool
            .add(crate::types::mana::ManaUnit::new(
                ManaType::Green,
                oid,
                false,
                Vec::new(),
            ));
        if let Some(o) = after_state.objects.get_mut(&oid) {
            o.counters.insert(CounterType::Plus1Plus1, 2);
        }
        let after = ResourceVector::snapshot(&after_state);

        let delta = ResourceVector::delta(&before, &after);

        // Green mana index is 4 in WUBRG+C order.
        assert_eq!(delta.mana[4], 1, "one green mana floated");
        assert_eq!(
            delta.life.get(&pid(1)).copied(),
            Some(-5),
            "opponent lost 5 life"
        );
        assert_eq!(
            delta
                .counters
                .get(&(CounterClass::Plus1Plus1, ObjectClass::Creature))
                .copied(),
            Some(2),
            "two +1/+1 counters added to a creature"
        );
        // Library unchanged ⇒ no key for either player.
        assert!(delta.library_delta.is_empty(), "no library change");
    }

    /// `is_net_progress` is true for a +damage / consume-nothing delta and false
    /// for a no-op and for a delta that net-consumes a consumed axis (life).
    #[test]
    fn net_progress_classification() {
        // +damage, nothing consumed ⇒ net progress.
        let mut win = ResourceVector::default();
        win.damage_dealt.insert(pid(1), 1);
        assert!(
            win.is_net_progress(),
            "+1 damage with no cost is net progress"
        );

        // No-op ⇒ not net progress.
        let noop = ResourceVector::default();
        assert!(
            !noop.is_net_progress(),
            "an empty delta is not net progress"
        );

        // Net-negative consumed axis (life) ⇒ not net progress even with a gain.
        let mut bleed = ResourceVector {
            tokens_created: 1,
            ..Default::default()
        };
        bleed.life.insert(pid(0), -1);
        assert!(
            !bleed.is_net_progress(),
            "a loop that net-loses life is not sustainable, so not infinite net progress"
        );
    }

    /// REVERT-PROBE for the modulo-vs-strict discriminator: a fabricated
    /// "strict-only" comparison (the *uncomplemented* equality, i.e. forgetting
    /// to project out resources) must reject the same-board/different-resources
    /// pair that the real modulo comparison accepts. This pins that the resource
    /// projection is load-bearing: remove it (fall back to `loop_states_equal`)
    /// and the discriminator collapses.
    #[test]
    fn revert_probe_projection_is_load_bearing() {
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 500, 0);
        let mut b = a.clone();
        b.players[1].life -= 1;
        if let Some(o) = b.objects.get_mut(&oid) {
            o.counters.insert(CounterType::Plus1Plus1, 1);
        }

        // The real (complemented) comparison accepts it.
        assert!(loop_states_equal_modulo_resources(&a, &b));
        // The un-complemented comparison (what a revert would leave) rejects it.
        assert!(
            !loop_states_equal(&a.normalize_for_loop(), &b.normalize_for_loop()),
            "without the resource projection the comparison would (wrongly) reject this beneficial-loop point"
        );
    }

    /// R1 — REVERT PROBE for the state-readable combat-phase axis (EDIT 3):
    /// `snapshot` reads extra combat phases from `combat_phases_started_this_turn`
    /// (entered, minus the one natural combat) plus the `BeginCombat` entries
    /// queued in `state.extra_phases`. A queued `Upkeep` extra phase must not
    /// change it. Reverting EDIT 3 leaves `combat_phases` at its `Default` 0 and
    /// flips the positive assertions.
    #[test]
    fn snapshot_reads_extra_combat_phases() {
        use crate::types::game_state::ExtraPhase;

        let mut state = GameState::new_two_player(7);
        // CR 506.1: one natural combat + two extra combats already ENTERED.
        state.combat_phases_started_this_turn = 3;
        // CR 500.8: one extra combat still QUEUED, plus a non-combat extra phase
        // that must be filtered out.
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::BeginCombat,
            attacker_restriction: None,
            attacker_restriction_source: None,
        });
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::Upkeep,
            phase: Phase::Upkeep,
            attacker_restriction: None,
            attacker_restriction_source: None,
        });

        let v = ResourceVector::snapshot(&state);
        // entered extra = (3 - 1) = 2; queued BeginCombat = 1; Upkeep ignored.
        assert_eq!(
            v.combat_phases, 3,
            "snapshot = entered-extra (started-1=2) + queued BeginCombat (1); Upkeep filtered"
        );

        // Removing the queued BeginCombat drops the axis to the entered term only.
        let mut consumed = GameState::new_two_player(7);
        consumed.combat_phases_started_this_turn = 3;
        let v2 = ResourceVector::snapshot(&consumed);
        assert_eq!(
            v2.combat_phases, 2,
            "with no queued extras, only the entered term (started - 1) remains"
        );
    }

    /// `unbounded_components` names the axis that grew — the input the PR-2
    /// `WinKind` classifier reads. A mill loop surfaces as a negative library.
    #[test]
    fn unbounded_components_names_growing_axes() {
        let mut drain = ResourceVector::default();
        drain.damage_dealt.insert(pid(1), 3);
        let axes = drain.unbounded_components();
        assert_eq!(axes, vec![(ResourceAxis::DamageDealt(pid(1)), 3)]);

        let mut mill = ResourceVector::default();
        mill.library_delta.insert(pid(1), -4);
        let axes = mill.unbounded_components();
        assert_eq!(
            axes,
            vec![(ResourceAxis::LibraryDelta(pid(1)), -4)],
            "a mill loop is unbounded downward on library size"
        );
    }

    /// EDIT A1 (CR 602.5b): a per-turn ("Activate only once each turn") activation
    /// gate must be PRESERVED across `project_out_resources`, so a loop that
    /// re-activates the gated ability (tally 1 -> 2) plus a projected resource
    /// (life) compares modulo-UNEQUAL — the gate is what makes it non-repeatable.
    /// PAIRED POSITIVE: an UNRESTRICTED ability's tally is projected out, so the
    /// same shape stays modulo-EQUAL. The contrast is the discrimination: reverting
    /// to a blanket `.clear()` flips the negative to equal.
    #[test]
    fn activated_once_per_turn_gate_breaks_modulo_equality() {
        // --- Negative: gated ability, tally differs => UNEQUAL ---
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature_with_restrictions(
            &mut a,
            700,
            0,
            vec![ActivationRestriction::OnlyOnceEachTurn],
        );
        let mut b = a.clone();
        b.activated_abilities_this_turn.insert((oid, 0), 1); // gate progressed
        b.players[1].life -= 1; // projected-out resource gain
        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "a preserved once-per-turn activation gate (CR 602.5b) must keep two cycles UNEQUAL"
        );

        // --- Positive control: unrestricted ability, tally projected out => EQUAL ---
        let mut c = GameState::new_two_player(7);
        let oid2 = battlefield_creature_with_restrictions(&mut c, 701, 0, Vec::new());
        let mut d = c.clone();
        d.activated_abilities_this_turn.insert((oid2, 0), 1);
        d.players[1].life -= 1;
        assert!(
            loop_states_equal_modulo_resources(&c, &d),
            "an unrestricted ability's tally is pure history and must be projected out (EQUAL)"
        );
    }

    /// EDIT A1 (CR 602.5b): per-GAME ("Activate only once") gate preserved; sibling
    /// unrestricted ability projected out.
    #[test]
    fn activated_once_per_game_gate_breaks_modulo_equality() {
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature_with_restrictions(
            &mut a,
            710,
            0,
            vec![ActivationRestriction::OnlyOnce],
        );
        let mut b = a.clone();
        b.activated_abilities_this_game.insert((oid, 0), 1);
        b.players[1].life -= 1;
        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "a preserved once-per-game activation gate (CR 602.5b) must keep two cycles UNEQUAL"
        );

        let mut c = GameState::new_two_player(7);
        let oid2 = battlefield_creature_with_restrictions(&mut c, 711, 0, Vec::new());
        let mut d = c.clone();
        d.activated_abilities_this_game.insert((oid2, 0), 1);
        d.players[1].life -= 1;
        assert!(
            loop_states_equal_modulo_resources(&c, &d),
            "an unrestricted ability's per-game tally is pure history and must be projected out (EQUAL)"
        );
    }

    /// EDIT A3 (CR 603.2h): a once-per-turn TRIGGER limit (`triggers_fired_this_turn`)
    /// is no longer cleared, so a loop that re-fires the gated trigger plus a
    /// resource delta compares UNEQUAL. CONTROL: an unrestricted trigger writes
    /// NEITHER map, so a loop modeled with empty trigger maps both sides is EQUAL.
    #[test]
    fn trigger_once_per_turn_gate_breaks_modulo_equality() {
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 720, 0);
        let mut b = a.clone();
        b.triggers_fired_this_turn.insert(test_trigger_ref(&b, oid)); // OncePerTurn gate fired
        b.players[1].life -= 1;
        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "a preserved once-per-turn trigger limit (CR 603.2h) must keep two cycles UNEQUAL"
        );

        // CONTROL: unrestricted trigger touches neither map => both empty => EQUAL.
        let mut c = GameState::new_two_player(7);
        battlefield_creature(&mut c, 721, 0);
        let mut d = c.clone();
        d.players[1].life -= 1; // only a projected resource differs
        assert!(
            loop_states_equal_modulo_resources(&c, &d),
            "an unrestricted trigger writes neither limit map, so the cycle stays EQUAL"
        );
    }

    /// EDIT A3 (CR 603.2h): an N-times-per-turn TRIGGER limit
    /// (`trigger_fire_counts_this_turn`) 1 vs 2 plus a resource delta compares
    /// UNEQUAL. CONTROL: empty count maps both sides => EQUAL.
    #[test]
    fn trigger_max_times_per_turn_gate_breaks_modulo_equality() {
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 730, 0);
        a.trigger_fire_counts_this_turn
            .insert(test_trigger_ref(&a, oid), 1);
        let mut b = a.clone();
        b.trigger_fire_counts_this_turn
            .insert(test_trigger_ref(&b, oid), 2); // limit progressed
        b.players[1].life -= 1;
        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "a preserved N-times-per-turn trigger limit (CR 603.2h) must keep two cycles UNEQUAL"
        );

        let mut c = GameState::new_two_player(7);
        battlefield_creature(&mut c, 731, 0);
        let mut d = c.clone();
        d.players[1].life -= 1;
        assert!(
            loop_states_equal_modulo_resources(&c, &d),
            "with empty count maps both sides, only a projected resource differs => EQUAL"
        );
    }

    /// EDIT B (CR 606.3): the per-object loyalty-activation count is compared
    /// analysis-locally, so a loop re-activating a loyalty ability (0 -> 1) plus a
    /// projected resource (loyalty counters, which `project_out_resources` zeroes)
    /// compares UNEQUAL. `objects_content_eq` ignores this field, so this helper is
    /// the ONLY thing catching the loyalty loop. CONTROL: equal counts (a damage
    /// loop on the same board) stay EQUAL.
    #[test]
    fn loyalty_activation_breaks_modulo_equality() {
        let mut a = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut a, 740, 0);
        a.objects.get_mut(&oid).unwrap().card_types.core_types = vec![CoreType::Planeswalker];
        let mut b = a.clone();
        // The loyalty ability was activated again, and loyalty grew (projected out).
        if let Some(o) = b.objects.get_mut(&oid) {
            o.loyalty_activations_this_turn = 1;
            o.counters.insert(CounterType::Loyalty, 5);
        }
        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "CR 606.3: a re-activated loyalty ability (count 0 -> 1) must compare UNEQUAL even \
             though loyalty counters are projected out and objects_content_eq ignores the count"
        );

        // CONTROL: equal loyalty-activation counts (a non-loyalty damage loop) => EQUAL.
        let mut c = GameState::new_two_player(7);
        battlefield_creature(&mut c, 741, 0);
        let mut d = c.clone();
        d.players[1].life -= 1; // a drain loop, no loyalty re-activation
        assert!(
            loop_states_equal_modulo_resources(&c, &d),
            "equal loyalty-activation counts must stay modulo-EQUAL (transparent for non-loyalty loops)"
        );
    }

    /// EDIT A5 (CR 602.5b): the gate-predicate partition. `AsSorcery` is a real
    /// non-gate restriction variant (it constrains timing, not repetition), so it
    /// must read as NOT a per-turn gate — proving the predicates classify by the
    /// repetition axis, not by "has any restriction".
    #[test]
    fn activation_gate_predicates_partition_restrictions() {
        let mut state = GameState::new_two_player(7);

        let per_turn = battlefield_creature_with_restrictions(
            &mut state,
            750,
            0,
            vec![ActivationRestriction::OnlyOnceEachTurn],
        );
        let max_turn = battlefield_creature_with_restrictions(
            &mut state,
            751,
            0,
            vec![ActivationRestriction::MaxTimesEachTurn { count: 2 }],
        );
        let per_game = battlefield_creature_with_restrictions(
            &mut state,
            752,
            0,
            vec![ActivationRestriction::OnlyOnce],
        );
        let non_gate = battlefield_creature_with_restrictions(
            &mut state,
            753,
            0,
            vec![ActivationRestriction::AsSorcery],
        );

        // Per-turn predicate: true for the two per-turn limits, false otherwise.
        assert!(ability_has_per_turn_activation_gate(&state, &(per_turn, 0)));
        assert!(ability_has_per_turn_activation_gate(&state, &(max_turn, 0)));
        assert!(!ability_has_per_turn_activation_gate(
            &state,
            &(per_game, 0)
        ));
        assert!(!ability_has_per_turn_activation_gate(
            &state,
            &(non_gate, 0)
        ));

        // Per-game predicate: true ONLY for OnlyOnce.
        assert!(ability_has_per_game_activation_gate(&state, &(per_game, 0)));
        assert!(!ability_has_per_game_activation_gate(
            &state,
            &(per_turn, 0)
        ));
        assert!(!ability_has_per_game_activation_gate(
            &state,
            &(max_turn, 0)
        ));
        assert!(!ability_has_per_game_activation_gate(
            &state,
            &(non_gate, 0)
        ));

        // A missing source object is not-gated (gate moot).
        assert!(!ability_has_per_turn_activation_gate(
            &state,
            &(ObjectId(9999), 0)
        ));
        assert!(!ability_has_per_game_activation_gate(
            &state,
            &(ObjectId(9999), 0)
        ));
    }

    /// Build a `TriggeredAbility` stack entry from `source`/`controller` with the
    /// given volatile `entry_id` (fresh each cycle in the live reducer).
    fn trigger_entry(
        entry_id: u64,
        source: u64,
        controller: u8,
    ) -> crate::types::game_state::StackEntry {
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
        use crate::types::game_state::{StackEntry, StackEntryKind};
        let src = ObjectId(source);
        StackEntry {
            id: ObjectId(entry_id),
            source_id: src,
            controller: PlayerId(controller),
            kind: StackEntryKind::TriggeredAbility {
                source_id: src,
                ability: Box::new(ResolvedAbility::new(
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                    vec![],
                    src,
                    PlayerId(controller),
                )),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        }
    }

    /// U-stack ([BLOCKER 0]): the modulo comparator must treat two cascade cycle
    /// points whose stacks hold the SAME triggered ability from the SAME source but
    /// a DIFFERENT (fresh) entry id as equal — otherwise a mandatory trigger cascade
    /// is invisible to the modulo scan and PR-3 is dead code. The control pair (a
    /// DIFFERENT source) must still compare UNEQUAL (the canon zeroes only the
    /// bookkeeping id, never the content).
    ///
    /// Revert proof: removing the `entry.id = ObjectId(pos)` loop in
    /// `project_out_resources` flips the first assertion to `false`.
    #[test]
    fn modulo_equal_ignores_volatile_stack_entry_id() {
        let mut a = GameState::new_two_player(7);
        a.stack.push_back(trigger_entry(10, 500, 0));
        a.stack_trigger_firings.insert(
            ObjectId(10),
            TriggerFiring::ReceiptEligible(DelayedTriggerOrigin {
                token: DelayedTriggerToken(1),
                instance: DelayedTriggerInstanceId(1),
                source_id: ObjectId(500),
            }),
        );
        let mut b = a.clone();
        b.stack.clear();
        b.stack.push_back(trigger_entry(11, 500, 0)); // same source, fresh id
        b.stack_trigger_firings.remove(&ObjectId(10));
        b.stack_trigger_firings.insert(
            ObjectId(11),
            TriggerFiring::ReceiptEligible(DelayedTriggerOrigin {
                token: DelayedTriggerToken(2),
                instance: DelayedTriggerInstanceId(2),
                source_id: ObjectId(500),
            }),
        );
        assert!(
            loop_states_equal_modulo_resources(&a, &b),
            "same delayed firing must compare equal modulo fresh stack and provenance identities"
        );

        let mut different_firing = b.clone();
        different_firing
            .stack_trigger_firings
            .insert(ObjectId(11), TriggerFiring::Ordinary);
        assert!(
            !loop_states_equal_modulo_resources(&a, &different_firing),
            "ordinary and delayed trigger firings must remain distinct"
        );

        // CONTROL: a different source_id is a genuinely different stack point.
        let mut c = a.clone();
        c.stack.clear();
        c.stack.push_back(trigger_entry(10, 501, 0));
        assert!(
            !loop_states_equal_modulo_resources(&a, &c),
            "a trigger from a DIFFERENT source must NOT be equated (content is preserved)"
        );
    }

    // ===================================================================
    // N1 — growing-cascade coverability (`loop_states_cover_modulo_growth`)
    // Positives P1/P2 + hostile revert-fail negatives (a)–(n). Each hostile
    // returns FALSE; the plan's §5 names the one-line revert that flips it TRUE.
    // ===================================================================

    use crate::types::ability::{
        AbilityCondition, Comparator, ControllerRef, CountScope, Effect, FilterProp, PlayerScope,
        PtStat, PtValueScope, QuantityExpr, QuantityRef, ReplacementCondition,
        ReplacementDefinition, ResolvedAbility, StaticCondition, StaticDefinition, TargetFilter,
        TargetRef, TriggerCondition, TriggerDefinition, TypedFilter,
    };
    use crate::types::counter::CounterMatch;
    use crate::types::player::PlayerCounterKind;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::triggers::TriggerMode;

    const CHURN_SRC: u64 = 500;

    /// A mandatory, no-ordering-input `TriggeredAbility` stack entry wrapping
    /// `ability`, with an optional trigger-level intervening-if `condition`.
    /// `controller` is kept in the normalized key; `entry_id`/`source_id` are
    /// zeroed by normalization, so kind identity is (controller, ability, condition).
    fn churn_entry(
        entry_id: u64,
        controller: u8,
        ability: ResolvedAbility,
        condition: Option<TriggerCondition>,
    ) -> StackEntry {
        let src = ObjectId(CHURN_SRC);
        StackEntry {
            id: ObjectId(entry_id),
            source_id: src,
            controller: PlayerId(controller),
            kind: StackEntryKind::TriggeredAbility {
                source_id: src,
                ability: Box::new(ability),
                condition,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                // CR 603.2c: the batched-subject count these entries' `event_amount()`
                // drains resolve "that many" against. `bind_resolution_scope` lifts it
                // into resolution scope; with it absent the drain's amount resolves to
                // ZERO, the resolver proposes nothing, and gate (6)'s probe is
                // fail-closed on the EMPTY derivation — a different fact from the ones
                // these rows test.
                subject_match_count: Some(1),
                die_result: None,
                provenance: None,
            },
        }
    }

    /// Fixed-amount `GainLife` ability — reads NO projected resource; distinct
    /// normalized kinds are produced by varying `amount`.
    fn gain_ability(amount: i32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(CHURN_SRC),
            PlayerId(0),
        )
    }

    /// The opponent `Typed` player-target filter Vito/Sanguine Bond announce
    /// ("target opponent") — verbatim the card-data parse
    /// (`Typed{type_filters:[], controller:Opponent, properties:[]}`) plus optional
    /// extra `properties` for the projected-axis discriminators.
    fn opp_typed(properties: Vec<FilterProp>) -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![],
            controller: Some(ControllerRef::Opponent),
            properties,
        })
    }

    /// A `LoseLife` ability whose `amount` is supplied and whose player target is
    /// `target` — the Vito/Sanguine drain shape. With `amount` non-projected
    /// (EventContextAmount / Fixed), the projected axis comes ENTIRELY from the
    /// target (item-4's subject).
    fn lose_life_targeting(amount: QuantityExpr, target: TargetFilter) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::LoseLife {
                amount,
                target: Some(target),
            },
            vec![],
            ObjectId(CHURN_SRC),
            PlayerId(0),
        )
    }

    fn event_amount() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        }
    }

    fn your_life_total() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal {
                player: PlayerScope::Controller,
            },
        }
    }

    // ===================================================================
    // COMMIT 1 (item-4) — `TargetFilter::Typed` projected-axis discriminators.
    // Non-vacuous at the classifier level independent of item-3.
    // ===================================================================

    /// Vito's `target opponent` (pure-controller `Typed`, empty properties) reads
    /// NO projected resource. Revert-probe: restoring the arm to
    /// `TargetFilter::Typed(..) => Axes::CONSERVATIVE` flips this to `true`.
    #[test]
    fn typed_filter_pure_controller_not_projected() {
        let ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
        assert!(
            !crate::game::ability_scan::ability_reads_projected_resource(&ability),
            "pure-controller opponent Typed reads no projected resource"
        );
    }

    /// A `Cmc` threshold reading your life total is still projected (CR 119).
    /// Revert-probe: narrowing the `Cmc` value to `Fixed(1)` flips this `false`.
    #[test]
    fn typed_filter_cmc_lifetotal_still_reads() {
        let ability = lose_life_targeting(
            event_amount(),
            opp_typed(vec![FilterProp::Cmc {
                comparator: Comparator::GE,
                value: your_life_total(),
            }]),
        );
        assert!(
            crate::game::ability_scan::ability_reads_projected_resource(&ability),
            "Cmc reading your life total is projected"
        );
    }

    /// Finding A (the NON-`Cmc` path): `PtComparison` reading your life total
    /// ("power ≤ your life total", CR 208 + CR 119) is projected. Revert-probe:
    /// classifying `PtComparison` as a NONE leaf (forgetting to recurse it) flips
    /// this `false` — the UNSOUND cover this test guards.
    #[test]
    fn typed_filter_ptcomparison_lifetotal_still_reads() {
        let ability = lose_life_targeting(
            event_amount(),
            opp_typed(vec![FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::LE,
                value: your_life_total(),
            }]),
        );
        assert!(
            crate::game::ability_scan::ability_reads_projected_resource(&ability),
            "PtComparison reading your life total is projected (recurse guard)"
        );
    }

    /// `CountersPutOnThisTurn` reads `counter_added_this_turn` (cleared by
    /// `project_out_resources`, CR 122.1) ⇒ projected (fail-closed leaf, no revert).
    #[test]
    fn typed_filter_counters_put_this_turn_conservative() {
        let ability = lose_life_targeting(
            event_amount(),
            opp_typed(vec![FilterProp::CountersPutOnThisTurn {
                actor: CountScope::Controller,
                counters: CounterMatch::Any,
                comparator: Comparator::GE,
                count: 1,
            }]),
        );
        assert!(
            crate::game::ability_scan::ability_reads_projected_resource(&ability),
            "CountersPutOnThisTurn is a proven-projected fail-closed leaf"
        );
    }

    /// Over-edit guard: the `Typed` arm keeps `event`/`sibling` CONSERVATIVE for
    /// both a pure-controller and a projected-property filter. A `Fixed` amount
    /// contributes NO axis, so both axes come SOLELY from the Typed arm here.
    /// Revert-probe: setting the arm's `event`/`sibling` to `false` flips these.
    #[test]
    fn event_and_sibling_axes_unchanged_for_typed() {
        for properties in [
            vec![],
            vec![FilterProp::Cmc {
                comparator: Comparator::GE,
                value: your_life_total(),
            }],
        ] {
            let ability =
                lose_life_targeting(QuantityExpr::Fixed { value: 1 }, opp_typed(properties));
            assert!(
                crate::game::ability_scan::ability_uses_event_context(&ability),
                "the Typed arm keeps event:true"
            );
            assert!(
                crate::game::ability_scan::ability_reads_sibling_mutable(&ability),
                "the Typed arm keeps sibling:true"
            );
        }
    }

    /// A plain fixed-drain churn entry (the target-class shape): controller 0,
    /// GainLife 1, no condition. `id` keeps entries distinct pre-normalization.
    fn g(id: u64) -> StackEntry {
        churn_entry(id, 0, gain_ability(1), None)
    }

    /// prior `[G,G]`, current `[G,G,G]` — the canonical homogeneous covering pair
    /// (board equal modulo resources, stack grew on an occupied mandatory place).
    fn cover_base() -> (GameState, GameState) {
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(g(10));
        prior.stack.push_back(g(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(g(20));
        current.stack.push_back(g(21));
        current.stack.push_back(g(22));
        (prior, current)
    }

    fn bf_object(state: &mut GameState, id: u64) -> ObjectId {
        bf_object_owned_by(state, id, PlayerId(1))
    }

    /// CR 614.1: a replacement definition's applicability is scoped to ITS
    /// controller's events, so a fixture that installs a def to be DRAWN as a
    /// candidate must put it on a permanent controlled by the player whose
    /// event it is meant to replace. The event-derived discharge asks the
    /// pipeline's own `find_applicable_replacements`, which honours that scope;
    /// the def-scan it replaced deliberately ignored it (over-count ⇒
    /// over-reject), so a P1-controlled def used to reject a P0 life gain.
    fn bf_object_owned_by(state: &mut GameState, id: u64, owner: PlayerId) -> ObjectId {
        let oid = ObjectId(id);
        let object = crate::game::game_object::GameObject::new(
            oid,
            CardId(7),
            owner,
            "Test Board Permanent".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(oid, object);
        state.battlefield.push_back(oid);
        oid
    }

    /// P1: homogeneous `[G,G]` → `[G,G,G]` covers.
    #[test]
    fn n1_p1_homogeneous_cover_true() {
        let (prior, current) = cover_base();
        assert!(loop_states_cover_modulo_growth(&prior, &current));
    }

    /// Stack growth compares trigger-firing semantics, not the fresh IDs that
    /// index their sidecar rows. This keeps the board-only precheck independent
    /// of stack depth while preserving CR 603.7's ordinary/delayed distinction.
    #[test]
    fn n1_trigger_firings_follow_normalized_stack_entries() {
        let (mut prior, mut current) = cover_base();
        for id in [10, 11] {
            prior
                .stack_trigger_firings
                .insert(ObjectId(id), TriggerFiring::Ordinary);
        }
        for id in [20, 21, 22] {
            current
                .stack_trigger_firings
                .insert(ObjectId(id), TriggerFiring::Ordinary);
        }
        assert!(
            loop_states_cover_modulo_growth(&prior, &current),
            "fresh stack-entry IDs must not block a same-kind trigger cover"
        );

        current
            .stack_trigger_firings
            .insert(ObjectId(21), TriggerFiring::LegacyDelayed);
        assert!(
            !loop_states_cover_modulo_growth(&prior, &current),
            "ordinary and delayed firing classes must not cover each other"
        );
    }

    /// P2: interleaved `[B,A]` → `[B,B,A]` covers (subsequence, non-prefix) —
    /// pins that embedding is NOT over-tightened to a strict bottom-prefix.
    #[test]
    fn n1_p2_interleaved_subsequence_cover_true() {
        // A = controller-0 kind, B = controller-1 kind (distinct via kept controller).
        let a = |id| churn_entry(id, 0, gain_ability(1), None);
        let b = |id| churn_entry(id, 1, gain_ability(1), None);
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(b(10)); // [B, A]
        prior.stack.push_back(a(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(b(20)); // [B, B, A]
        current.stack.push_back(b(21));
        current.stack.push_back(a(22));
        assert!(loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (a) an extra permanent in `current` ⇒ false (board differs, not just stack).
    /// Revert-fail: dropping the stack-cleared board compare flips this true.
    #[test]
    fn n1_a_extra_permanent_false() {
        let (prior, mut current) = cover_base();
        bf_object(&mut current, 900);
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (b) the grown entry carries a TARGET ⇒ false (has-ordering-input guard).
    /// The kind is occupied in prior so occupancy passes — isolates item 3.
    #[test]
    fn n1_b_grown_entry_targeted_false() {
        let targeted = |id| {
            let mut ability = gain_ability(1);
            ability.targets = vec![TargetRef::Player(PlayerId(1))];
            churn_entry(id, 0, ability, None)
        };
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(targeted(10));
        prior.stack.push_back(targeted(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(targeted(20));
        current.stack.push_back(targeted(21));
        current.stack.push_back(targeted(22));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    // ===================================================================
    // COMMIT 2 (item-3) — forced-unique targeted-cover discriminators.
    // Grown entries pass item-4 (pure-controller Typed) so item-3 is the sole
    // decider (the R1-vacuity remedy). Verbatim Vito/Sanguine drain shape.
    // ===================================================================

    /// A P0-controlled drain stack entry:
    /// `LoseLife{amount:EventContextAmount, target:Typed{controller:Opponent}}`
    /// with optional extra target `properties`. Verbatim the card-data parse.
    fn drain_entry(id: u64, properties: Vec<FilterProp>) -> StackEntry {
        let mut ability = lose_life_targeting(event_amount(), opp_typed(properties));
        // A real on-stack targeted trigger has its (chosen) target announced. A
        // non-empty `targets` is what routes item-3 through `forced_unique_targeting`
        // instead of the no-target trivial pass — the R1-vacuity remedy. The value is
        // a placeholder; `forced_unique_targeting` rebuilds slots from the effect.
        ability.targets = vec![TargetRef::Player(PlayerId(1))];
        churn_entry(id, 0, ability, None)
    }

    /// An `n`-player state carrying a P0 source creature (`CHURN_SRC`) so the
    /// drain's opponent target slot resolves against a real source context.
    fn drain_state(players: u8) -> GameState {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), players, 7);
        let src = ObjectId(CHURN_SRC);
        let mut obj = GameObject::new(
            src,
            CardId(9),
            PlayerId(0),
            "Test Vito".to_string(),
            Zone::Battlefield,
        );
        obj.card_types.core_types.push(CoreType::Creature);
        state.objects.insert(src, obj);
        state.battlefield.push_back(src);
        state
    }

    /// POSITIVE: 2p growing targeted drain `[D,D]→[D,D,D]`. Both fixes ⇒ cover TRUE
    /// (item-4: pure-controller Typed not projected; item-3: the single opponent is
    /// forced-unique). Revert-probes (measured in the impl report): undo item-3
    /// (`targets.is_empty()`) → FALSE; undo item-4 (`Typed=>CONSERVATIVE`) → FALSE.
    #[test]
    fn n1_forced_unique_targeted_cover_true() {
        let mut prior = drain_state(2);
        prior.stack.push_back(drain_entry(10, vec![]));
        prior.stack.push_back(drain_entry(11, vec![]));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(drain_entry(20, vec![]));
        current.stack.push_back(drain_entry(21, vec![]));
        current.stack.push_back(drain_entry(22, vec![]));
        assert!(
            loop_states_cover_modulo_growth(&prior, &current),
            "2p forced-unique targeted drain must cover (both item-3 and item-4 pass)"
        );
    }

    /// NEGATIVE (over-relax guard): 3p (2 opponents) targeted growth ⇒ cover FALSE.
    /// The drain still passes item-4, so the rejection is item-3: two legal opponent
    /// targets ⇒ `auto_select => Ok(None)` ⇒ NOT forced-unique. Revert-probe:
    /// mis-relaxing item-3 to accept any non-empty target flips this TRUE.
    #[test]
    fn n1_open_target_growing_still_rejected() {
        let mut prior = drain_state(3);
        prior.stack.push_back(drain_entry(10, vec![]));
        prior.stack.push_back(drain_entry(11, vec![]));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(drain_entry(20, vec![]));
        current.stack.push_back(drain_entry(21, vec![]));
        current.stack.push_back(drain_entry(22, vec![]));

        // Reach-guard (mandate 4 anti-vacuity): item-4 PASSES so the FALSE below is
        // attributable to item-3's ≥2-legal rejection, not an upstream projected read.
        let ability = current.stack[2].ability().unwrap();
        assert!(
            !crate::game::ability_scan::ability_reads_projected_resource(ability),
            "item-4 passes (pure-controller Typed) — the rejector is item-3"
        );
        assert!(
            !forced_unique_targeting(&current, ability),
            "two opponents ⇒ auto_select Ok(None) ⇒ not forced-unique"
        );

        assert!(
            !loop_states_cover_modulo_growth(&prior, &current),
            "open (≥2-legal) targeted growth must be rejected"
        );
    }

    /// CR 601.2c (reached for a triggered ability via CR 603.3d): the mint must ask the
    /// ANNOUNCEMENT authority how many choices announcing an entry requires — never a proxy
    /// for it. `Effect::target_filter()` answers a DIFFERENT question ("is there a
    /// player-target filter on the head effect?"); `ability_utils::build_target_slots`
    /// answers this one, and is the same function the relief's own `forced_unique_targeting`
    /// rebuilds slots with. Three rows, each a shape where the two answers DIVERGE, plus a
    /// positive control so the conjunct cannot be constant-false.
    ///
    /// MEASURED REVERT-PROBE (delete the `build_target_slots` conjunct in
    /// `entry_publishes_pin_slots`), on row (a)'s board:
    /// `mint_publishes` false→TRUE, `bounded_cycle_pin_slots(..).len()` 0→1 (ONE point with
    /// `min/max_targets: 1` for a TWO-choice announcement), and the pinned cover false→TRUE.
    /// That last flip is the fail-open: gate (3)'s bare `continue` discharges a slot no pin
    /// specifies. Rows (b) and (c) flip `mint_publishes` false→TRUE on the same probe.
    #[test]
    fn bounded_cycle_pin_slots_requires_a_single_mandatory_announcement_slot() {
        use crate::game::ability_utils::build_target_slots;
        use crate::game::engine::{bounded_cycle_pin_slots, entry_publishes_pin_slots};

        // ── (a) TWO announcement choices: a chained sub-ability that also drains
        // `target opponent`. CR 601.2c: "if the spell uses the word `target` in multiple
        // places, the same object or player can be chosen once for each instance."
        let (prior, current) = grown_window(3, |id| {
            let mut ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
            ability.targets = vec![TargetRef::Player(PlayerId(1))];
            ability.sub_ability = Some(Box::new(lose_life_targeting(
                event_amount(),
                opp_typed(vec![]),
            )));
            churn_entry(id, 0, ability, None)
        });
        let ability = current.stack[2].ability().unwrap();
        assert_eq!(
            build_target_slots(&current, ability).map(|s| s.len()).ok(),
            Some(2),
            "reach-guard: `collect_target_slots_inner` recurses into `sub_ability`, so the \
             runtime announcement carries two independent CR 601.2c choices"
        );
        assert!(
            ability.effect.target_filter().is_some(),
            "reach-guard: the PROXY still reports a single head-effect filter — that is the \
             whole divergence"
        );
        // Reach-guards that gate (3) is the SOLE rejector here (gates (4)/(6) clean), so the
        // cover assertions below are attributable to the pin relief and nothing else.
        assert!(
            !forced_unique_targeting(&current, ability),
            "reach-guard: three opponents ⇒ not forced-unique ⇒ gate (3) rejects"
        );
        assert!(
            !crate::game::ability_scan::ability_reads_projected_resource(ability),
            "reach-guard: gate (4) passes"
        );
        assert!(
            entry_publishes_pin_slots(&current, &current.stack[2], PlayerId(0)).is_none(),
            "a published point says `min/max_targets: 1`; this announcement has TWO choices"
        );
        assert!(
            bounded_cycle_pin_slots(&current, PlayerId(0)).is_empty(),
            "and the mint therefore publishes nothing rather than under-describing it"
        );
        let slot = churn_src_slot(&current, 0);
        for pinned in [&[][..], std::slice::from_ref(&slot)] {
            assert!(
                !loop_states_cover_modulo_growth_scoped(
                    &prior,
                    &current,
                    pinned_scope(pinned),
                    &mut PeriodVerdicts::for_period(&[], &current, PlayerId(0))
                ),
                "gate (3)'s relief is a bare `continue`: an unpublished second target choice \
                 must keep rejecting ({} pinned slot(s))",
                pinned.len()
            );
        }

        // ── (b) ZERO announcement choices, two ways. `Effect::target_filter()` returns
        // `Some` for both, but the SLOT BUILDER surfaces no stack slot: CR 701.21a
        // `Sacrifice` is carved out of `triggers::extract_target_filter_from_effect` (the
        // accessor the builder actually uses), and a CR 601.2c choice made at resolution
        // (`TargetChoiceTiming::Resolution`) is not announced at all.
        let zero_slot_cases: [(&str, ResolvedAbility); 2] = [
            ("CR 701.21a Sacrifice — not a target", {
                ResolvedAbility::new(
                    Effect::Sacrifice {
                        target: opp_typed(vec![]),
                        count: QuantityExpr::Fixed { value: 1 },
                        min_count: 0,
                    },
                    vec![],
                    ObjectId(CHURN_SRC),
                    PlayerId(0),
                )
            }),
            ("CR 601.2c — chosen at resolution, not announcement", {
                let mut a = lose_life_targeting(event_amount(), opp_typed(vec![]));
                a.targets = vec![TargetRef::Player(PlayerId(1))];
                a.target_choice_timing = crate::types::ability::TargetChoiceTiming::Resolution;
                a
            }),
        ];
        for (label, ability) in zero_slot_cases {
            let (_p, c) = grown_window(3, |id| churn_entry(id, 0, ability.clone(), None));
            let live = c.stack[2].ability().unwrap();
            assert!(
                live.effect.target_filter().is_some(),
                "reach-guard: the PROXY says `Some` ({label})"
            );
            assert_eq!(
                build_target_slots(&c, live).map(|s| s.len()).ok(),
                Some(0),
                "reach-guard: the ANNOUNCEMENT authority says zero ({label})"
            );
            assert!(
                entry_publishes_pin_slots(&c, &c.stack[2], PlayerId(0)).is_none(),
                "no announcement choice ⇒ no published point ({label})"
            );
        }

        // ── (c) an "up to one target" announcement: CR 601.2c makes the real minimum ZERO,
        // and the slot may legally carry an EMPTY legal set, so `min_targets: 1` overstates
        // it. One slot — the count half of the conjunct does NOT catch this; `!optional` does.
        let (_p_opt, c_opt) = grown_window(3, |id| {
            let mut ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
            ability.targets = vec![TargetRef::Player(PlayerId(1))];
            ability.optional_targeting = true;
            churn_entry(id, 0, ability, None)
        });
        assert_eq!(
            build_target_slots(&c_opt, c_opt.stack[2].ability().unwrap())
                .map(|s| s.iter().map(|slot| slot.optional).collect::<Vec<_>>())
                .ok(),
            Some(vec![true]),
            "reach-guard: exactly ONE slot, and it is OPTIONAL — so only the `!optional` \
             half of the conjunct can reject this row"
        );
        assert!(
            entry_publishes_pin_slots(&c_opt, &c_opt.stack[2], PlayerId(0)).is_none(),
            "CR 601.2c: an `up to one target` announcement's minimum is 0, not the \
             published 1"
        );

        // ── positive control: the shipped drain still publishes, so none of the above is a
        // constant-false conjunct. Same board as arm 1 of
        // `a_pinned_slot_skips_gate_three_and_six`.
        let (_p_ok, c_ok) = grown_window(3, |id| drain_entry(id, vec![]));
        assert_eq!(
            build_target_slots(&c_ok, c_ok.stack[2].ability().unwrap())
                .map(|s| s.len())
                .ok(),
            Some(1),
            "control: one mandatory announcement choice"
        );
        assert_eq!(
            bounded_cycle_pin_slots(&c_ok, PlayerId(0)).len(),
            1,
            "control: the instrument still returns a NON-zero point set"
        );
    }

    /// CR 115.2 + CR 601.2c (via CR 603.3d): the mint must take *which* choice it publishes
    /// from the ANNOUNCEMENT authority too — not just *how many*.
    ///
    /// The sibling row above closed the cardinality axis. This is the same divergence on the
    /// legal-SET axis, and it is the shape the cardinality conjunct cannot see: the head
    /// effect declares the CR 115.2 player filter but announces NOTHING
    /// (`TargetChoiceTiming::Resolution` ⇒ 0 slots), while a chained sub-ability announces
    /// the one mandatory slot — over CREATURES. `Effect::target_filter()` answers
    /// `Typed{[], Opponent, []}`; `build_target_slots` answers "one mandatory slot,
    /// `[Object(500), Object(901), Object(902)]`" (measured, both).
    ///
    /// MEASURED REVERT-PROBE (drop the all-`Player` conjunct in
    /// `entry_publishes_pin_slots`): `entry_publishes_pin_slots(..).is_some()` false→TRUE and
    /// `bounded_cycle_pin_slots(..)` 0→1 point, whose `legal_targets` before the fix was the
    /// re-derived `[Player(1), Player(2)]` — a published point claiming a PLAYER set for a
    /// three-OBJECT announcement, which no `TargetPin::Player` can specify and which gate
    /// (3)'s bare `continue` would discharge anyway.
    ///
    /// The positive control asserts the published set EQUALS the builder's slot rather than
    /// a literal, so it fails if the mint ever re-derives the set from the accessor again.
    #[test]
    fn bounded_cycle_pin_slots_legal_set_comes_from_the_announcement_authority() {
        use crate::game::ability_utils::build_target_slots;
        use crate::game::engine::{bounded_cycle_pin_slots, entry_publishes_pin_slots};
        use crate::types::ability::{TargetChoiceTiming, TypeFilter};

        // A chained sub-ability targeting CREATURES: the one slot the announcement really
        // surfaces. `controller: None` so the set spans all three seats' creatures.
        let creature_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![],
        });
        let (mut prior, mut current) = grown_window(3, |id| {
            let mut ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
            ability.targets = vec![TargetRef::Player(PlayerId(1))];
            // CR 601.2c: chosen at RESOLUTION ⇒ the head contributes no announcement slot.
            ability.target_choice_timing = TargetChoiceTiming::Resolution;
            ability.sub_ability = Some(Box::new(lose_life_targeting(
                event_amount(),
                creature_filter.clone(),
            )));
            churn_entry(id, 0, ability, None)
        });
        // Added to BOTH windows: the object axes are STRICT-compared, so a creature present
        // only in `current` would make the cover false for an unrelated reason.
        for state in [&mut prior, &mut current] {
            for (id, controller) in [(901u64, 1u8), (902, 2)] {
                let oid = ObjectId(id);
                let mut obj = GameObject::new(
                    oid,
                    CardId(9),
                    PlayerId(controller),
                    format!("Bystander {id}"),
                    Zone::Battlefield,
                );
                obj.card_types.core_types.push(CoreType::Creature);
                state.objects.insert(oid, obj);
                state.battlefield.push_back(oid);
            }
        }

        let ability = current.stack[2].ability().unwrap();
        assert!(
            ability.effect.target_filter().is_some(),
            "reach-guard: the PROXY still reports the head effect's player filter — that is \
             the whole divergence"
        );
        let announced = build_target_slots(&current, ability).expect("one announcement slot");
        assert_eq!(
            announced
                .iter()
                .map(|slot| (slot.optional, slot.legal_targets.clone()))
                .collect::<Vec<_>>(),
            vec![(
                false,
                vec![
                    TargetRef::Object(ObjectId(CHURN_SRC)),
                    TargetRef::Object(ObjectId(901)),
                    TargetRef::Object(ObjectId(902)),
                ]
            )],
            "reach-guard: the CARDINALITY conjunct passes here (exactly one MANDATORY slot), \
             so the all-`Player` conjunct is the sole rejector — and the announced set is \
             three OBJECTS, not the head filter's players"
        );
        assert!(
            !forced_unique_targeting(&current, ability),
            "reach-guard: three legal creatures ⇒ not forced-unique ⇒ gate (3) rejects"
        );
        assert!(
            !crate::game::ability_scan::ability_reads_projected_resource(ability),
            "reach-guard: gate (4) passes, so the cover rows below are gate (3)'s"
        );

        assert!(
            entry_publishes_pin_slots(&current, &current.stack[2], PlayerId(0)).is_none(),
            "the announced choice is among OBJECTS; a `TargetPin::Player` cannot specify it, \
             so nothing may be published"
        );
        assert!(
            bounded_cycle_pin_slots(&current, PlayerId(0)).is_empty(),
            "and the mint publishes no point rather than one describing a different choice"
        );
        let slot = churn_src_slot(&current, 0);
        for pinned in [&[][..], std::slice::from_ref(&slot)] {
            assert!(
                !loop_states_cover_modulo_growth_scoped(
                    &prior,
                    &current,
                    pinned_scope(pinned),
                    &mut PeriodVerdicts::for_period(&[], &current, PlayerId(0))
                ),
                "gate (3)'s relief is a bare `continue`: an object-valued announcement choice \
                 must keep rejecting ({} pinned slot(s))",
                pinned.len()
            );
        }

        // ── the RESIDUAL divergence the all-`Player` conjunct alone does NOT close: a
        // chained sub-ability that announces a choice among ALL players (CR 115.2 "target
        // player") under the same "target opponent" head. Every conjunct passes — one
        // mandatory slot, every candidate a player, head shape accepted — so the mint
        // publishes, and the ONLY thing that keeps the point honest is that the set is
        // carried through from the builder instead of re-derived from the head filter.
        let (_p_any, any_player) = grown_window(3, |id| {
            let mut ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
            ability.targets = vec![TargetRef::Player(PlayerId(1))];
            ability.target_choice_timing = TargetChoiceTiming::Resolution;
            ability.sub_ability = Some(Box::new(lose_life_targeting(
                event_amount(),
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![],
                    controller: None,
                    properties: vec![],
                }),
            )));
            churn_entry(id, 0, ability, None)
        });
        let all_seats = vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
            TargetRef::Player(PlayerId(2)),
        ];
        assert_eq!(
            build_target_slots(&any_player, any_player.stack[2].ability().unwrap())
                .map(|slots| slots
                    .iter()
                    .map(|s| (s.optional, s.legal_targets.clone()))
                    .collect::<Vec<_>>())
                .ok(),
            Some(vec![(false, all_seats.clone())]),
            "reach-guard: ONE mandatory slot whose candidates are all PLAYERS — so every \
             acceptance conjunct passes and only the publication path can still be wrong"
        );
        let any_points = bounded_cycle_pin_slots(&any_player, PlayerId(0));
        assert_eq!(
            any_points.len(),
            1,
            "reach-guard: the mint accepts this entry"
        );
        assert_eq!(
            any_points[0].kind,
            crate::analysis::decision_template::DecisionPointKind::Targets {
                legal_targets: all_seats,
                min_targets: 1,
                max_targets: 1,
                ordered: false,
            },
            "the point describes the choice the ANNOUNCEMENT offers (three seats); \
             re-deriving from the head filter would publish the two opponents"
        );

        // ── positive control + EQUIVALENCE: the shipped drain publishes, and the published
        // legal set is the BUILDER's, asserted against it rather than against a literal.
        let (_p_ok, c_ok) = grown_window(3, |id| drain_entry(id, vec![]));
        let mut control_slots =
            build_target_slots(&c_ok, c_ok.stack[2].ability().unwrap()).expect("control announces");
        assert_eq!(control_slots.len(), 1, "control: one announcement slot");
        let builder_set = control_slots.swap_remove(0).legal_targets;
        assert_eq!(
            builder_set,
            vec![
                TargetRef::Player(PlayerId(1)),
                TargetRef::Player(PlayerId(2))
            ],
            "control: the builder enumerates the two opponents"
        );
        let published = bounded_cycle_pin_slots(&c_ok, PlayerId(0));
        assert_eq!(
            published.len(),
            1,
            "control: the instrument returns non-zero"
        );
        assert_eq!(
            published[0].kind,
            crate::analysis::decision_template::DecisionPointKind::Targets {
                legal_targets: builder_set,
                min_targets: 1,
                max_targets: 1,
                ordered: false,
            },
            "the published legal set IS the announcement authority's slot"
        );
    }

    /// **Row F1.** CR 732.2a: a FORCED target is not a game choice, so the mint must NOT
    /// publish a decision point for it.
    ///
    /// CR 732.2a describes a shortcut as "a sequence of game choices, for all players"; a
    /// published `DecisionPoint` stands for one such choice. When exactly one legal assignment
    /// exists, the announcing player makes none — `triggers::prepare_trigger_targets` routes
    /// this predicate's `Ok(Some(..))` straight to `AutoAssigned`, so no
    /// `WaitingFor::TriggerTargetSelection` is raised, so `record_trigger_target_answer` (whose
    /// only two call sites are that prompt's reducer arms) never runs. A point published here
    /// would therefore demand a `predictability_gate` answer that CANNOT ARRIVE, and since the
    /// gate's `required` set is EVERY published point, one such point makes the whole offer
    /// undeclarable — the precise failure the bounded-offer journal exists to remove.
    ///
    /// # Discrimination, and the confound it breaks
    ///
    /// (a) vs (b) differ in the number of living opponents, which is confounded with the number
    /// of legal assignments — so (a′) repeats the forced verdict at (b)'s SEAT COUNT with one
    /// opponent eliminated (CR 800.4 + CR 102.1: a departed seat is not choosable). An
    /// implementation keyed on "is this a 2-player game" passes (a) and (b) and FAILS (a′).
    ///
    /// REVERT-PROBE, MEASURED (deleting the `forced_unique_targeting` withhold from
    /// `entry_publishes_pin_slots`, with the mutation `cmp`-proved to have applied): the row
    /// FLIPS TO FAILING at arm (a) — "CR 732.2a: no choice is made here, so nothing is
    /// published". (a′) and (c) assert the SAME withhold on two further boards and are not
    /// separately measured, because (a) panics first; each carries its own reach-guard instead.
    /// (b)'s positive is pinned INDEPENDENTLY OF THIS ROW and on BOTH sides of the change, by
    /// `bounded_cycle_pin_slots_requires_a_single_mandatory_announcement_slot`'s control on the
    /// same 3p `drain_entry` fixture — so "the mint publishes nothing" cannot be what makes the
    /// forced arms pass.
    ///
    /// # Reach-guards
    ///
    /// Each forced arm asserts the FULL announcement shape first (one mandatory slot, all-player
    /// legal set), so the withhold is attributable to forced-ness rather than to one of the
    /// upstream cardinality / optionality / all-`Player` conjuncts. Arm (d) asserts the RELIEF
    /// survives: gate (3) was already passing on a forced board through
    /// `stack_entry_has_no_ordering_input`, so unpublishing the point costs no cover.
    #[test]
    fn a_forced_target_is_not_a_published_decision_point() {
        use crate::analysis::decision_template::DecisionPointKind;
        use crate::game::ability_utils::build_target_slots;
        use crate::game::engine::{bounded_cycle_pin_slots, entry_publishes_pin_slots};

        let announcement = |state: &GameState| {
            build_target_slots(state, state.stack[2].ability().unwrap())
                .map(|slots| {
                    slots
                        .iter()
                        .map(|s| (s.optional, s.legal_targets.clone()))
                        .collect::<Vec<_>>()
                })
                .ok()
        };

        // ── (a) FORCED: 2p, the single opponent is the only legal assignment ──
        let (_p2, c2) = grown_window(2, |id| drain_entry(id, vec![]));
        assert_eq!(
            announcement(&c2),
            Some(vec![(false, vec![TargetRef::Player(PlayerId(1))])]),
            "reach-guard: ONE mandatory slot over PLAYERS — every conjunct upstream of the \
             forced-ness check accepts this entry, so the withhold below is attributable"
        );
        assert!(
            forced_unique_targeting(&c2, c2.stack[2].ability().unwrap()),
            "reach-guard: one legal assignment ⇒ the dispatcher announces it without asking"
        );
        assert!(
            entry_publishes_pin_slots(&c2, &c2.stack[2], PlayerId(0)).is_none(),
            "CR 732.2a: no choice is made here, so nothing is published — and a mandatory \
             drain has no CR 603.5 gate to publish either"
        );
        assert!(
            bounded_cycle_pin_slots(&c2, PlayerId(0)).is_empty(),
            "and the point mint carries the withhold through"
        );

        // ── (a′) SAME SEAT COUNT as (b), one opponent eliminated ⇒ still forced ──
        let (_pe, mut ce) = grown_window(3, |id| drain_entry(id, vec![]));
        ce.players
            .iter_mut()
            .find(|p| p.id == PlayerId(2))
            .expect("fixture: the 3p board seats P2")
            .is_eliminated = true;
        assert_eq!(
            ce.players.len(),
            3,
            "reach-guard: the SEAT COUNT still matches (b) — only legality differs"
        );
        assert_eq!(
            announcement(&ce),
            Some(vec![(false, vec![TargetRef::Player(PlayerId(1))])]),
            "reach-guard: CR 800.4 + CR 102.1 — a departed seat is not one of the people in \
             the game, so the announcement authority enumerates ONE opponent"
        );
        assert!(
            entry_publishes_pin_slots(&ce, &ce.stack[2], PlayerId(0)).is_none(),
            "the verdict follows the LEGAL SET, not the seat count"
        );

        // ── (b) MATCHED POSITIVE: 3p, two legal assignments ⇒ a real choice ⇒ published ──
        let (_p3, c3) = grown_window(3, |id| drain_entry(id, vec![]));
        assert!(
            !forced_unique_targeting(&c3, c3.stack[2].ability().unwrap()),
            "reach-guard: two opponents ⇒ `auto_select => Ok(None)` ⇒ the player IS asked"
        );
        let published = bounded_cycle_pin_slots(&c3, PlayerId(0));
        assert_eq!(
            published.len(),
            1,
            "control: an unforced target choice is still published — without this every \
             assertion above is satisfied by a mint that publishes nothing"
        );
        assert!(
            matches!(published[0].kind, DecisionPointKind::Targets { .. }),
            "control: and it is the CR 601.2c Targets point, not some other kind"
        );

        // ── (c) the CR 603.5 gate SURVIVES the withhold ──
        // Withholding the forced target must not suppress the entry: a "may" on the same
        // source is a real per-iteration choice with its own sub-index.
        let (_pm, cm) = grown_window(2, optional_drain);
        let pins = entry_publishes_pin_slots(&cm, &cm.stack[2], PlayerId(0))
            .expect("an optional entry still publishes its CR 603.5 gate");
        assert!(
            pins.target.is_none(),
            "the forced CR 601.2c target is withheld"
        );
        assert!(pins.may.is_some(), "the CR 603.5 take/decline is not");
        assert!(
            pins.legal_targets.is_empty(),
            "no target slot carries no legal set"
        );
        let may_points = bounded_cycle_pin_slots(&cm, PlayerId(0));
        assert_eq!(
            may_points.len(),
            1,
            "exactly the may point reaches the schema: {may_points:?}"
        );
        assert!(
            matches!(may_points[0].kind, DecisionPointKind::MayChoice),
            "and it is the MayChoice point"
        );

        // ── (d) NO RELIEF IS LOST: gate (3) passes on a forced board without any pin ──
        for (label, state) in [("(a) 2p", &c2), ("(a′) eliminated", &ce), ("(c) may", &cm)] {
            assert!(
                stack_entry_has_no_ordering_input(state, &state.stack[2]),
                "{label}: the target axis of gate (3) is discharged by forced-ness itself, so \
                 unpublishing the point cannot cost the cover a relief it used to get"
            );
        }
    }

    /// CR 704.5a — **WITHHOLDING A FORCED ANNOUNCEMENT FROM THE SCHEMA DOES NOT UNCHARGE ITS
    /// VICTIM: the bound is the SAME whether or not the point is published.**
    ///
    /// The sibling row above asserts the CR 732.2a WITHHOLD (a forced announcement is not a
    /// game choice, so no decision point is published). That withhold is right, and its blast
    /// radius was not: `declarable_victims` and `PeriodicDelta::victim_slot` were BOTH derived
    /// from the published point set, so withholding the point dropped the forced victim into
    /// `elimination_bounds`' bare-`observed_life_loss` arm and the bound GREW — an offer
    /// declaring more repetitions legal than CR 732.2a permits, on the very operator that
    /// proves the proposal "may be legally taken based on the current game state".
    ///
    /// This row therefore asserts THE BOUND, not the publication. A row that only re-asserted
    /// "the point is withheld" is exactly the row that already existed and that missed this.
    ///
    /// # The matched pair, and why the two arms are comparable
    ///
    /// Both arms run step (7)'s own two derivations verbatim, then the production
    /// `elimination_bounds`. They differ in ONE axis — how many opponents the announcement
    /// authority enumerates, which is what makes the announcement `Forced` (2p, one legal
    /// assignment, point WITHHELD) or `Chosen` (3p, two legal assignments, point PUBLISHED).
    /// The extra seat is parked at 40 life so its own headroom never binds, and the delta is
    /// byte-identical across the arms, so the ONLY thing that can move the bound is whether
    /// the withheld announcement is charged. Asserting EQUALITY pins both directions at once:
    /// the pre-fix fail-OPEN (looser when withheld) and an over-correction (tighter when
    /// withheld, which would silently shrink offers on boards that work today).
    ///
    /// * **(A) the ordinary forced drain** — P1 loses 1 per period. Charged: P1's magnitude is
    ///   `observed 1 + S 1 = 2` over headroom `7 - 1`, so **3**. Uncharged it is `1`, giving
    ///   **6**.
    /// * **(B) the victim who NETS A LIFE GAIN** — P1 *gains* 1 per period while the proposer
    ///   loses 2. This is the shape where the defect is worst rather than merely loose:
    ///   uncharged, P1's magnitude is `-1`, `elimination_bounds`' `narrow` guard
    ///   (`magnitude > 0`) never fires and P1's life axis is DISARMED outright, leaving only
    ///   the proposer's `20 / 2 = 10`. Charged, the `.max(0)` clamp floors the gain at zero
    ///   and P1 is charged `0 + S 2 = 2` over headroom `7 - 1`, so **3**. Case (o) of
    ///   `elimination_bounds_conventions` guards that clamp in isolation; this row is what
    ///   proves a real production derivation still REACHES it on a forced board.
    ///
    /// # What a wrong implementation would still pass, and the guard for each
    ///
    /// * *charge every living seat* (ignore the legal set): both arms move together and stay
    ///   equal ⇒ the VICTIM-SET assertions below, not the equality, are what reject it.
    /// * *republish the forced point* (revert the sibling row's withhold): the two derivations
    ///   coincide again and equality holds ⇒ the withhold reach-guard rejects it.
    /// * *charge the victim but not the magnitude* (or vice versa): arm (A) yields 6, not 3 ⇒
    ///   the exact-value assertions reject it.
    ///
    /// REVERT-PROBE, MEASURED (the mutation `cmp`-proved to have applied, and the file
    /// restored byte-identically by SHA256 afterwards): RE-CONFLATE the two questions inside
    /// the charging mint — add `.filter(|t| t.announcement == TargetAnnouncement::Chosen)` to
    /// `game::engine::bounded_cycle_charged_targets_for_window`, which is precisely "charge
    /// only what CR 732.2a publishes". The forced arm then charges NOTHING and the row FLIPS
    /// TO FAILING at arm (A)'s first victim-set assertion: `[]` where `[PlayerId(1)]` is
    /// required. Arm (B) is not separately measured because (A) panics first; it carries its
    /// own exact-value assertion instead.
    ///
    /// ⚠ THE OTHER OBVIOUS REVERT DOES NOT REACH THIS ROW, and that is worth stating rather
    /// than leaving to be re-derived: restoring step (7)'s published-point derivation inside
    /// `try_offer_bounded_cycle_shortcut` leaves this row GREEN (measured), because this row
    /// calls the charging mint directly. That revert is discriminated by the sibling
    /// production-offer row `the_bounded_offer_charges_a_forced_victim_it_publishes_no_point_for`,
    /// which flips on it. The two rows cover the two halves of the seam on purpose.
    #[test]
    fn a_withheld_forced_announcement_is_charged_like_a_published_one() {
        use crate::analysis::decision_template::DecisionPointKind;
        use crate::game::engine::{
            bounded_cycle_charged_targets_for_window, bounded_cycle_pin_slots,
        };
        use std::collections::BTreeMap;

        /// Step (7)'s own two derivations, verbatim — the union of the CHARGED
        /// announcements' legal player sets, and the per-slot magnitude keyed by
        /// `worst_seat_life_loss`. One function, so neither arm can compute them a
        /// different way.
        fn step_seven(
            state: &GameState,
            delta: &ResourceVector,
        ) -> (Vec<PlayerId>, BTreeMap<DecisionSlot, i64>) {
            let touch =
                certified_period_touch(&[], state, PeriodCertification::ResourceSignatureOnly);
            let charged = bounded_cycle_charged_targets_for_window(&touch, PlayerId(0));
            let mut victims: Vec<PlayerId> = charged
                .iter()
                .flat_map(|(_, seats)| seats.iter().copied())
                .collect();
            victims.sort_unstable();
            victims.dedup();
            let magnitude = charged
                .iter()
                .map(|(slot, _)| (slot.clone(), delta.worst_seat_life_loss()))
                .collect();
            (victims, magnitude)
        }

        // One board per arm: `players` seats, P0 (the proposer) at 21, P1 (the victim) at
        // 7, and every further seat parked at 40 so only P1's headroom can bind.
        let board = |players: u8| {
            let (_prior, mut current) = grown_window(players, |id| drain_entry(id, vec![]));
            for p in current.players.iter_mut() {
                p.life = match p.id {
                    PlayerId(0) => 21,
                    PlayerId(1) => 7,
                    _ => 40,
                };
            }
            current
        };
        let life_delta = |seats: &[(PlayerId, i64)]| {
            let mut v = ResourceVector::default();
            for (seat, n) in seats {
                v.life.insert(*seat, *n);
            }
            v
        };

        let forced = board(2);
        let chosen = board(3);

        // ── REACH-GUARDS: the two arms really are the withheld/published pair ────────────
        assert!(
            bounded_cycle_pin_slots(&forced, PlayerId(0)).is_empty(),
            "REACH-GUARD: the 2p arm's announcement must still be WITHHELD (CR 732.2a — one \
             legal assignment is no game choice). Without this the row would be satisfied by \
             re-publishing the forced point, which is the change the sibling row forbids"
        );
        assert!(
            bounded_cycle_pin_slots(&chosen, PlayerId(0))
                .iter()
                .any(|p| matches!(p.kind, DecisionPointKind::Targets { .. })),
            "REACH-GUARD: the 3p arm must PUBLISH its `Targets` point, else 'published' and \
             'withheld' name the same board and the equality below is vacuous"
        );

        for (label, delta, expected, uncharged) in [
            (
                "(A) ordinary forced drain",
                life_delta(&[(PlayerId(1), -1)]),
                3,
                6,
            ),
            (
                "(B) victim nets a life GAIN",
                life_delta(&[(PlayerId(1), 1), (PlayerId(0), -2)]),
                3,
                10,
            ),
        ] {
            let (forced_victims, forced_magnitude) = step_seven(&forced, &delta);
            let (chosen_victims, chosen_magnitude) = step_seven(&chosen, &delta);

            // The victim SETS, asserted by content: an implementation that charged every
            // living seat would keep the two bounds equal and pass the equality alone.
            assert_eq!(
                forced_victims,
                vec![PlayerId(1)],
                "{label}: CR 704.5a — the WITHHELD announcement still charges the one seat \
                 its legal set names, and only that seat"
            );
            assert_eq!(
                chosen_victims,
                vec![PlayerId(1), PlayerId(2)],
                "{label}: and the published one charges both of its legal targets"
            );
            assert_eq!(
                (forced_magnitude.len(), chosen_magnitude.len()),
                (1, 1),
                "{label}: one SOURCE announces on both boards, so exactly one slot is \
                 charged on each (PER SOURCE, NOT PER ENTRY)"
            );

            let forced_bound =
                delta.elimination_bounds(&forced, &forced_victims, &forced_magnitude);
            let chosen_bound =
                delta.elimination_bounds(&chosen, &chosen_victims, &chosen_magnitude);
            assert_eq!(
                forced_bound, chosen_bound,
                "{label}: CR 704.5a — the bound must not move because CR 732.2a declined to \
                 publish the announcement as a game choice. The victim loses the life either \
                 way; who chose the target is not an input to how much is lost"
            );
            assert_eq!(
                forced_bound, expected,
                "{label}: and the shared value is the CHARGED one ({expected}), re-derived \
                 by hand above — not the UNCHARGED {uncharged} the published-point \
                 derivation produced"
            );
            assert_ne!(
                expected, uncharged,
                "{label}: fixture guard — the two derivations must actually disagree on this \
                 board, else the row cannot discriminate"
            );
        }
    }

    /// CR 704.5a + CR 732.2a — **a slot ANNOUNCED TWICE in one window is charged the UNION of
    /// its legal sets, not the first frame's.**
    ///
    /// The two mints agree on WHICH entries the cycle accepts (both read `entry_announces`)
    /// and they dedup the same slot the same way — but they keep DIFFERENT FRAMES of a repeat,
    /// because publication skips a `NotProposerChoice` frame and charging does not. First-wins
    /// charging therefore let a NARROW earlier frame's legal set stand for a slot the schema
    /// publishes from a WIDER later one: the schema states the client may pin P2,
    /// `declarable_victims` reads `[P1]`, `elimination_bounds` never charges P2, and
    /// `max_iterations` GROWS. That is the fail-OPEN direction, on the operator whose whole job
    /// is proving the proposed sequence "may be legally taken based on the current game state".
    ///
    /// # The board, and why it is a legal transition rather than a contrived one
    ///
    /// Three frames on ONE source (`CHURN_SRC`, P0's). The middle frame carries a P2-controlled
    /// permanent whose `StaticMode::Hexproof` affects its controller — CR 702.11c, "you can't
    /// be the target of spells or abilities your opponents control" — so the announcement
    /// authority enumerates ONE opponent there and the announcement is forced. On the live
    /// board that permanent has LEFT, so both opponents are legal and the announcement is the
    /// proposer's choice. A permanent leaving the battlefield between two retained ring frames
    /// is an ordinary event; nothing here rewinds an irreversible fact (contrast
    /// `is_eliminated`, which is why this row does not use the elimination lever the sibling
    /// rows use).
    ///
    /// **REACHABILITY: NARROW, AND NOT CLOSED — stated in both directions.** No production
    /// trajectory that reaches this shape has been built, by the reviewer, the orchestrator or
    /// this row. Elimination — the realistic mechanism, and the one every tracked dump shows —
    /// narrows the legal set MONOTONICALLY, which puts the widest frame first and lands
    /// first-wins fail-CLOSED. The fail-open direction needs a seat's untargetability to END
    /// mid-window; a corpus census measured 14 cards granting a player untargetability
    /// mid-loop, all self-protective and predominantly "until end of turn", which does not
    /// expire mid-turn, so the path additionally needs the grantor to leave or a shorter
    /// duration. This row builds the grantor-leaves half at the mint's own boundary. It is
    /// NOT evidence that a full drive reaches it, and the shape is NOT "unreachable".
    ///
    /// # What a wrong implementation would still pass this row, and the guard for each
    ///
    /// * *charge every living seat* — passes the union assertion and FAILS the narrow-frame
    ///   reach-guard, which pins the first frame's legal set at exactly `[P1]`.
    /// * *keep the LAST frame instead of unioning* — indistinguishable HERE (the later frame
    ///   is the wider one) and equally sound on this board, but it is not monotone in general;
    ///   the `charged.len() == 1` + slot-identity guards are what keep the row about the DEDUP
    ///   rather than about frame order, and the doc on
    ///   `game::engine::bounded_cycle_charged_targets_for_window` carries the monotonicity
    ///   argument the union rests on.
    /// * *publish nothing at all* — the publication reach-guard requires the WIDE point to
    ///   reach the schema at the same `DecisionSlot`, so a mint that published nothing fails
    ///   before the claim.
    /// * *drop the dedup entirely* — `charged.len() == 1` fails; two charged copies of one
    ///   slot would double `declared_life_magnitude` and silently halve the bound.
    ///
    /// REVERT-PROBE, and it is the shipped code's own previous form: replace the union arm
    /// with `if charged.iter().any(|(slot, _)| *slot == target.slot) { continue; }`. The
    /// charged victim list reads `[PlayerId(1)]` and the row FLIPS TO FAILING at the union
    /// assertion; the bound assertion below then reads 6 where 3 is required.
    #[test]
    fn a_repeated_slots_victim_lists_are_unioned_not_first_wins() {
        use crate::analysis::decision_template::DecisionPointKind;
        use crate::game::ability_utils::build_target_slots;
        use crate::game::engine::{
            bounded_cycle_charged_targets_for_window, bounded_cycle_pin_slots_for_window,
        };
        use std::collections::BTreeMap;

        const GRANTOR: ObjectId = ObjectId(600);

        // P1 is parked out of reach so only P2's headroom can bind the life axis, and P2 is
        // seeded at 7 so neither the charged nor the uncharged bound lands on the `1` floor.
        let mut base = drain_state(3);
        for p in base.players.iter_mut() {
            p.life = match p.id {
                PlayerId(0) => 21,
                PlayerId(1) => 40,
                _ => 7,
            };
        }

        // Window head: nothing on the stack, so frame 1's entry counts as ANNOUNCED there.
        let head = base.clone();

        // Frame 1 — CR 702.11c: P2 controls a permanent granting its controller hexproof, so
        // an opponent-controlled source cannot target them and the announcement is forced.
        let mut narrow = base.clone();
        let mut grantor = GameObject::new(
            GRANTOR,
            CardId(77),
            PlayerId(2),
            "You Have Hexproof".to_string(),
            Zone::Battlefield,
        );
        grantor.static_definitions =
            vec![
                StaticDefinition::new(StaticMode::Hexproof).affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            ]
            .into();
        narrow.objects.insert(GRANTOR, grantor);
        narrow.battlefield.push_back(GRANTOR);
        crate::game::layers::flush_layers(&mut narrow);
        narrow.stack.push_back(drain_entry(10, vec![]));

        // The live board — the grantor has left, so both opponents are legal again.
        let mut current = base.clone();
        current.stack.push_back(drain_entry(20, vec![]));

        let legal = |state: &GameState, entry: usize| {
            build_target_slots(state, state.stack[entry].ability().unwrap())
                .map(|slots| {
                    slots
                        .iter()
                        .map(|s| (s.optional, s.legal_targets.clone()))
                        .collect::<Vec<_>>()
                })
                .ok()
        };

        // ── REACH-GUARDS: the window really is the narrow-then-wide repeat ──────────────
        assert_eq!(
            legal(&narrow, 0),
            Some(vec![(false, vec![TargetRef::Player(PlayerId(1))])]),
            "REACH-GUARD: CR 702.11c — the hexproof grantor must actually remove P2 from the \
             announcement authority's legal set on the FIRST frame, else this row is two \
             identical frames and the dedup is unobservable"
        );
        assert_eq!(
            legal(&current, 0),
            Some(vec![(
                false,
                vec![
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(2))
                ]
            )]),
            "REACH-GUARD: and the live board must admit BOTH opponents, so the later frame is \
             the WIDER one"
        );

        let touch = certified_period_touch(
            &[&head, &narrow],
            &current,
            PeriodCertification::ResourceSignatureOnly,
        );
        assert_eq!(
            touch
                .announced
                .iter()
                .map(|(_, e)| e.id)
                .collect::<Vec<_>>(),
            vec![ObjectId(10), ObjectId(20)],
            "REACH-GUARD: exactly two announcements, NARROW FIRST — first-wins keeps the \
             narrow one, which is the whole shape under test"
        );

        // ── The publication half: ONE point, carrying the WIDE legal set ────────────────
        let points = bounded_cycle_pin_slots_for_window(&touch, PlayerId(0));
        assert_eq!(
            points.len(),
            1,
            "REACH-GUARD: the narrow frame's announcement is forced and withheld, the wide \
             one is the proposer's own choice and published — exactly one point: {points:?}"
        );
        assert_eq!(
            points[0].kind,
            DecisionPointKind::Targets {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(2))
                ],
                min_targets: 1,
                max_targets: 1,
                ordered: false,
            },
            "REACH-GUARD: the SCHEMA states the client may pin P2. Everything below is about \
             the bound owing a charge for that stated pin"
        );

        // ── THE CLAIM: one slot, and its charge is the UNION ────────────────────────────
        let charged = bounded_cycle_charged_targets_for_window(&touch, PlayerId(0));
        assert_eq!(
            charged.len(),
            1,
            "PER SOURCE, NOT PER ENTRY: both entries carry one source, so one slot is \
             charged. Two copies would double `declared_life_magnitude` and halve the bound \
             instead of widening the victim set: {charged:?}"
        );
        assert_eq!(
            charged[0].0, points[0].slot,
            "the charged slot IS the published slot — without this the union below could be \
             about a different decision point than the one the schema offers"
        );
        assert_eq!(
            charged[0].1,
            vec![PlayerId(1), PlayerId(2)],
            "CR 704.5a: a repeated slot charges the UNION of its announcements' legal player \
             sets. First-wins reads [P1] here, so the schema would offer a P2 pin that \
             `elimination_bounds` never charges and `max_iterations` would GROW"
        );

        // ── And the bound really moves, so the union is not a cosmetic set difference ───
        let mut delta = ResourceVector::default();
        delta.life.insert(PlayerId(2), -1);
        let magnitude: BTreeMap<DecisionSlot, i64> = charged
            .iter()
            .map(|(slot, _)| (slot.clone(), delta.worst_seat_life_loss()))
            .collect();
        assert_eq!(
            delta.elimination_bounds(&current, &charged[0].1, &magnitude),
            3,
            "P2 is a declarable victim, so its life magnitude is `observed 1 + S 1 = 2` over \
             CR 704.5a headroom `7 - 1`; first-wins leaves P2 out of the victim set, charges \
             the bare observed 1 and returns 6"
        );
        assert_eq!(
            delta.elimination_bounds(&current, &[PlayerId(1)], &magnitude),
            6,
            "fixture guard — the two victim sets must actually disagree on this board, else \
             the assertion above cannot discriminate"
        );
    }

    /// CR 601.2c + CR 115.1 + CR 732.2a — **an announcement the PROPOSER does not make is
    /// withheld from the schema and charged all the same, on all THREE of CR 601.2c's axes.**
    ///
    /// `TargetAnnouncement` answers "is announcing this a game choice the proposer makes".
    /// `forced_unique_targeting` answers only the assignment-COUNT half of that question, and
    /// two `triggers::prepare_trigger_targets` routes raise no prompt for the proposer while
    /// the count half reports "not forced":
    ///
    /// * **(a) CR 601.2c `target_chooser`** — "of an opponent's choice" (Volcanic Offering's
    ///   shape). `ability_utils::auto_select_targets_for_ability` early-returns `Ok(None)`
    ///   whenever ANY slot carries a chooser, so the count half is false even with exactly ONE
    ///   legal assignment — arm (a2) is that exact board. The prompt is raised for the
    ///   CHOOSER, `record_trigger_target_answer` journals under the seat that answered, and
    ///   every consumer reads `loop_answer(slot, proposer)` ⇒ an unanswerable published point,
    ///   which is the undeclarable-offer condition the bounded offer exists to remove.
    /// * **(b) `TargetSelectionMode::Random`** — routed to `random_select_targets_for_ability`
    ///   and then `AutoAssigned`, so no prompt is ever raised, and the pin RELIEVES gate (3):
    ///   the offer would be minted because of a designation the RNG contradicts at drive time.
    ///
    /// **SCOPING HONESTY: the publication behaviour PREDATES the commit this row ships in.**
    /// What is new is a named authority claiming to answer the whole question while reading one
    /// of its three members. This row is not evidence of a defect this commit introduced.
    ///
    /// # What a wrong implementation would still pass, and the guard for each
    ///
    /// * *withhold everything* — arm (c) publishes on the SAME 3p board with neither axis set,
    ///   so a mint that published nothing fails there.
    /// * *withhold by legal-set size* — arm (a1)/(b) have TWO legal opponents and are still
    ///   withheld; arm (c) has the same two and publishes. Size cannot separate them.
    /// * *withhold, and also stop charging* — every arm asserts the CR 704.5a charge survives
    ///   with the full legal player set, which is the half `elimination_bounds` reads.
    /// * *key the chooser on presence rather than on the SEAT* — not discriminated here and
    ///   deliberately so: `collect_target_slots` already drops a chooser equal to the
    ///   ability's controller, so on these fixtures `is_some()` and `is_some_and(!= proposer)`
    ///   coincide. The inequality guards `entry.controller != ability.controller` skew, which
    ///   no fixture in this crate builds.
    ///
    /// REVERT-PROBE (each measured separately, since the first failing arm panics): delete the
    /// `slot.chooser` disjunct from `game::engine::entry_announces` ⇒ arms (a1)/(a2) FLIP TO
    /// FAILING on "must be WITHHELD"; delete the `target_selection_mode` disjunct ⇒ arm (b)
    /// flips instead. Neither deletion touches arm (c), which is what makes the two axes
    /// separately attributable rather than jointly.
    #[test]
    fn an_announcement_the_proposer_does_not_make_is_withheld_but_still_charged() {
        use crate::analysis::decision_template::DecisionPointKind;
        use crate::game::ability_utils::build_target_slots;
        use crate::game::engine::{
            bounded_cycle_charged_targets_for_window, bounded_cycle_pin_slots,
            entry_publishes_pin_slots,
        };
        use crate::types::ability::TargetSelectionMode;

        // The drain, with one of CR 601.2c's non-count announcement axes set.
        let axis_drain = |id: u64, chooser: bool, random: bool| {
            let mut ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
            ability.targets = vec![TargetRef::Player(PlayerId(1))];
            if chooser {
                // CR 601.2c: "of an opponent's choice" — `resolve_effect_player_ref` reads the
                // already-announced opponent target, so the announcing seat is P1.
                ability.target_chooser = Some(TargetFilter::Opponent);
            }
            if random {
                ability.target_selection_mode = TargetSelectionMode::Random;
            }
            churn_entry(id, 0, ability, None)
        };

        let charged_victims = |state: &GameState| {
            let touch =
                certified_period_touch(&[], state, PeriodCertification::ResourceSignatureOnly);
            bounded_cycle_charged_targets_for_window(&touch, PlayerId(0))
        };
        let announcement_slot = |state: &GameState| {
            build_target_slots(state, state.stack[2].ability().unwrap())
                .map(|slots| {
                    slots
                        .iter()
                        .map(|s| (s.optional, s.chooser, s.legal_targets.clone()))
                        .collect::<Vec<_>>()
                })
                .ok()
        };

        // ── (c) CONTROL first: neither axis, two legal opponents ⇒ PUBLISHED ────────────
        let (_pc, control) = grown_window(3, |id| axis_drain(id, false, false));
        let control_points = bounded_cycle_pin_slots(&control, PlayerId(0));
        assert_eq!(control_points.len(), 1, "control: {control_points:?}");
        assert!(
            matches!(control_points[0].kind, DecisionPointKind::Targets { .. }),
            "control: an announcement the proposer DOES make is still published — without \
             this every withhold below is satisfied by a mint that publishes nothing"
        );

        // ── (a1) CR 601.2c chooser, 3p: two legal assignments, still not the proposer's ──
        let (_p1, chooser_3p) = grown_window(3, |id| axis_drain(id, true, false));
        assert_eq!(
            announcement_slot(&chooser_3p),
            Some(vec![(
                false,
                Some(PlayerId(1)),
                vec![
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(2))
                ]
            )]),
            "REACH-GUARD (a1): ONE mandatory slot over PLAYERS whose ANNOUNCER is P1, not the \
             proposer — every conjunct upstream of the announcement check accepts this entry, \
             and the legal set is the SAME SIZE as the control's, so size cannot be the \
             discriminator"
        );
        assert!(
            entry_publishes_pin_slots(&chooser_3p, &chooser_3p.stack[2], PlayerId(0)).is_none(),
            "CR 601.2c: P1 announces this target, so no point the PROPOSER could answer is \
             published — a published one is unanswerable at `loop_answer(slot, proposer)` and \
             one unanswerable point makes the WHOLE offer undeclarable"
        );
        assert_eq!(
            charged_victims(&chooser_3p)
                .into_iter()
                .map(|(_, seats)| seats)
                .collect::<Vec<_>>(),
            vec![vec![PlayerId(1), PlayerId(2)]],
            "CR 704.5a: withheld is not uncharged — whoever announces it, the named seat \
             loses the life"
        );

        // ── (a2) CR 601.2c chooser, 2p: EXACTLY ONE legal assignment, and the count half is
        //        blind to it — the precise claim, isolated ────────────────────────────────
        let (_p2, chooser_2p) = grown_window(2, |id| axis_drain(id, true, false));
        assert_eq!(
            announcement_slot(&chooser_2p),
            Some(vec![(
                false,
                Some(PlayerId(1)),
                vec![TargetRef::Player(PlayerId(1))]
            )]),
            "REACH-GUARD (a2): exactly ONE legal assignment"
        );
        assert!(
            !forced_unique_targeting(&chooser_2p, chooser_2p.stack[2].ability().unwrap()),
            "REACH-GUARD (a2): and the assignment-COUNT authority still reports NOT forced — \
             `auto_select_targets_for_ability` early-returns `Ok(None)` on any chooser. This \
             is why the count half alone minted `Chosen` for a one-assignment announcement"
        );
        assert!(
            entry_publishes_pin_slots(&chooser_2p, &chooser_2p.stack[2], PlayerId(0)).is_none(),
            "so the withhold must come from the CHOOSER axis, not from forced-ness"
        );
        assert_eq!(
            charged_victims(&chooser_2p)
                .into_iter()
                .map(|(_, seats)| seats)
                .collect::<Vec<_>>(),
            vec![vec![PlayerId(1)]],
            "CR 704.5a: still charged, and only the one seat its legal set names"
        );

        // ── (b) CR 115.1 overridden: the GAME selects, so nobody is prompted ─────────────
        let (_p3, random_3p) = grown_window(3, |id| axis_drain(id, false, true));
        assert_eq!(
            announcement_slot(&random_3p),
            Some(vec![(
                false,
                None,
                vec![
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(2))
                ]
            )]),
            "REACH-GUARD (b): no chooser is involved — this arm is the SELECTION-MODE axis \
             alone, on the control's own legal set"
        );
        assert!(
            !forced_unique_targeting(&random_3p, random_3p.stack[2].ability().unwrap()),
            "REACH-GUARD (b): two legal assignments, so the count authority reports NOT \
             forced and would have published"
        );
        assert!(
            entry_publishes_pin_slots(&random_3p, &random_3p.stack[2], PlayerId(0)).is_none(),
            "CR 115.1 is overridden — `prepare_trigger_targets` routes this to \
             `random_select_targets_for_ability` and `AutoAssigned`, raising no prompt at \
             all, so a pin would be a designation the RNG contradicts at drive time"
        );
        assert_eq!(
            charged_victims(&random_3p)
                .into_iter()
                .map(|(_, seats)| seats)
                .collect::<Vec<_>>(),
            vec![vec![PlayerId(1), PlayerId(2)]],
            "CR 704.5a: the RNG names one of these seats and it loses the life"
        );
    }

    /// CR 704.5a + CR 732.2a — **the same fix at the PRODUCTION OFFER, on a board that
    /// publishes NO decision point at all.**
    ///
    /// [`a_withheld_forced_announcement_is_charged_like_a_published_one`] drives step (7)'s
    /// derivations directly; this one drives
    /// `try_offer_bounded_cycle_shortcut_metered` — the producer the interactive bridge
    /// calls — and asserts the value that actually ships to the client.
    ///
    /// THE COMBINATION IS UNREACHABLE BEFORE THE FIX: a published schema with ZERO decision
    /// points, and a certificate whose `victim_slot` is NON-EMPTY. Both derivations used to
    /// read the same list, so "no points" implied "nothing charged" by construction.
    ///
    /// # The board
    ///
    /// `ring_announcing_on_its_newest_sample` is a 2-seat ring whose newest retained sample
    /// carries the drain entry, so the announcement's legal set is the single opponent and
    /// the announcement is FORCED. The harness steps P1's life one point per retained frame,
    /// so the certified period's measured delta is P1 `-1`.
    ///
    /// # The arithmetic, re-derived independently of `elimination_bounds`
    ///
    /// The certifying pair is the ring frame one period back against the live board, and the
    /// harness steps P1 one life point per retained frame, so the MEASURED per-period delta is
    /// P1 `-2` (asserted below rather than assumed). `worst_seat_life_loss` is therefore 2 and
    /// one slot is charged, so `S = 2`; P1 is a declarable victim and is charged
    /// `observed 2 + S 2 = 4` against CR 704.5a headroom `21 - 1 = 20`, giving **5**.
    /// Uncharged — the published-point derivation, which sees no points here at all — P1's
    /// magnitude is the bare observed `2` and the bound is **10**.
    ///
    /// P1 is seeded at 21 rather than the harness default so neither value lands on the `1`
    /// floor of the legal range, where an over-charging bug would be indistinguishable from
    /// the right answer.
    ///
    /// REVERT-PROBE: restore step (7)'s published-point derivation ⇒ `victim_slot` is empty
    /// and `max_iterations` is 10 ⇒ both the non-empty assertion and the value assertion FLIP.
    #[test]
    fn the_bounded_offer_charges_a_forced_victim_it_publishes_no_point_for() {
        use crate::game::engine::{
            try_offer_bounded_cycle_shortcut_metered, BoundedOfferRefusal, ProbeCap,
        };
        use crate::types::game_state::WaitingFor;

        let state = ring_announcing_on_its_newest_sample(
            |s| {
                announcing_ring_source(s, CHURN_SRC);
                // Seeded BEFORE any frame is snapshotted, so every retained sample and the
                // live board share this headroom and only the harness's own per-frame step
                // separates them.
                s.players
                    .iter_mut()
                    .find(|p| p.id == PlayerId(1))
                    .expect("the harness seats P1")
                    .life = 21;
            },
            |frame| {
                frame.stack.push_back(drain_entry(950, vec![]));
            },
        );
        // REACH-GUARD: the fixture must really be the FORCED shape, or this row is about the
        // ordinary published path the F4 dumps already cover.
        let announced = announced_from_retained_sample(&state, 950);
        assert!(
            forced_unique_targeting(announced, announced.stack[0].ability().unwrap()),
            "REACH-GUARD: one living opponent ⇒ one legal assignment ⇒ the dispatcher \
             announces the target itself and no player is ever asked"
        );

        let (outcome, meter) =
            try_offer_bounded_cycle_shortcut_metered(&state, false, ProbeCap::Shipped);
        let waiting = outcome.unwrap_or_else(|refusal: BoundedOfferRefusal| {
            panic!(
                "REACH-GUARD: the bounded offer must FIRE on this board, else every assertion \
                 below is made about a refusal; got {refusal:?}, meter {meter:?}"
            )
        });
        let WaitingFor::LoopShortcut {
            certificate,
            schema,
            ..
        } = &waiting
        else {
            panic!("the bounded producer returns a `LoopShortcut` offer; got {waiting:?}")
        };
        let per_cycle = certificate
            .per_cycle
            .as_ref()
            .expect("a bounded offer publishes the per-period signature its bound was divided by");

        assert!(
            schema.points.is_empty(),
            "CR 732.2a: the ONE announcement on this board is FORCED, so the schema publishes \
             no decision point at all; got {:?}",
            schema.points
        );
        assert_eq!(
            per_cycle.delta.life.get(&PlayerId(1)).copied(),
            Some(-2),
            "REACH-GUARD: the certified period must really drain the victim, else the bound \
             below is not about a CR 704.5a threshold; delta {:?}",
            per_cycle.delta
        );
        assert_eq!(
            per_cycle
                .victim_slot
                .iter()
                .map(|(_, m)| *m)
                .collect::<Vec<_>>(),
            vec![2],
            "CR 704.5a: the forced announcement is CHARGED even though CR 732.2a published no \
             point for it — the combination that was unreachable before, because both \
             derivations read the published list; got {:?}",
            per_cycle.victim_slot
        );
        assert_eq!(
            schema.max_iterations, 5,
            "CR 704.5a: headroom `21 - 1` over the charged magnitude `observed 2 + S 2`. The \
             published-point derivation charged nothing here and produced 10, declaring twice \
             as many repetitions legal as CR 732.2a permits"
        );
    }

    /// A slot an OFFER would publish for `CHURN_SRC`'s entries — built through the same
    /// authority the gates rebuild it with, so the rows prove the KEY matches rather than
    /// asserting a hand-written literal. `index: 0` is the CR 115.2 target choice,
    /// `index: 1` the CR 603.5 "may" gate.
    fn churn_src_slot(state: &GameState, index: u8) -> DecisionSlot {
        DecisionSlot {
            source: crate::game::engine::object_decision_source(state, ObjectId(CHURN_SRC))
                .expect("fixture: the churn source is on the battlefield"),
            index,
        }
    }

    /// The published-pin channel as a P0 offer would carry it.
    fn pinned_scope(slots: &[DecisionSlot]) -> LoopWindowScope<'_> {
        LoopWindowScope {
            phase_invariant: None,
            sole_driver: None,
            pinned: Some(PinnedChoices {
                proposer: PlayerId(0),
                slots,
            }),
            cast_card_ids: None,
            period: None,
        }
    }

    /// An optional (CR 603.5 "may") forced-unique drain — `MayPrompt` at
    /// `ability_scan.rs:6534` for exactly ONE reason, the one the offer publishes a
    /// `MayChoice` point for.
    fn optional_drain(id: u64) -> StackEntry {
        let mut ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
        ability.targets = vec![TargetRef::Player(PlayerId(1))];
        ability.optional = true;
        churn_entry(id, 0, ability, None)
    }

    /// The same drain whose `MayPrompt` ALSO has a second, unpublished cause: a
    /// CR 701.34a proliferate sub-ability.
    fn proliferate_drain(id: u64, optional: bool) -> StackEntry {
        let mut ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
        ability.targets = vec![TargetRef::Player(PlayerId(1))];
        ability.optional = optional;
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Proliferate,
            vec![],
            ObjectId(CHURN_SRC),
            PlayerId(0),
        )));
        churn_entry(id, 0, ability, None)
    }

    /// Grow a 2-entry window to 3 of the same kind: `[e(10),e(11)] → [e(20),e(21),e(22)]`.
    fn grown_window(players: u8, entry: impl Fn(u64) -> StackEntry) -> (GameState, GameState) {
        let mut prior = drain_state(players);
        prior.stack.push_back(entry(10));
        prior.stack.push_back(entry(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(entry(20));
        current.stack.push_back(entry(21));
        current.stack.push_back(entry(22));
        (prior, current)
    }

    /// CR 732.2a EXTENSION POINT: a per-iteration choice the OFFER publishes is a
    /// *specified* choice, not a free one, so gates (3) and (6) must stop rejecting on it —
    /// and ONLY on it. Every relief runs the MINT's own per-entry acceptance test
    /// (`game::engine::entry_publishes_pin_slots`), so the relief predicate cannot be
    /// coarser than the mint predicate.
    ///
    /// SIX ARMS. Arms 1–2 are matched pairs whose only variable is `scope.pinned`; arms
    /// 3–6 are the over-match controls that a coarser relief would fail.
    /// * arm 1 — gate (3), 3p open targeting (two legal opponents ⇒ `auto_select =>
    ///   Ok(None)` ⇒ NOT forced-unique). Same board as
    ///   `n1_open_target_growing_still_rejected`.
    /// * arm 2 — gate (6), an OPTIONAL drain: `MayPrompt` caused solely by CR 603.5
    ///   `optional`, the one axis the offer publishes a `MayChoice` point for.
    /// * arm 3 — gate (6) NON-ATTRIBUTION: a CR 701.34a proliferate choice is NOT relieved,
    ///   with or without an optional gate pinned on top of it. The proliferate board is
    ///   `item6_still_vetoes_under_forced_unique_targets`' board.
    /// * arm 4 — precondition (c): an entry the PROPOSER does not control is never relieved
    ///   by the proposer's pin, even when it shares the pinned source.
    /// * arm 5 — gate (3) SCOPE: the relief is a `continue`, so it discharges the whole
    ///   item-3 predicate — but a slot answers ONE target. `multi_target` (CR 601.2c),
    ///   `distribution` (CR 601.2d) and `target_constraints` (CR 601.2c) are separate
    ///   announcement-time facts no published slot specifies, each isolated as the sole
    ///   rejector on a forced-unique board. Its tail row covers the fourth such fact,
    ///   `pending_trigger_entry` (CR 603.3c), through the cover as well, with its own
    ///   gate-(1) control (see there).
    /// * arm 6 — gate (6) RESIDUAL: arm 2's relieved board plus an optional life
    ///   replacement still rejects, because a discharged CR 603.5 gate does not discharge
    ///   the CR 616.1 environmental surface.
    ///
    /// REVERT-PROBES (each measured; see the impl report):
    /// * drop the `entry_target_choice_is_pinned` guard at gate (3) ⇒ arm 1's PINNED half
    ///   stays `false` ⇒ FAILS.
    /// * drop the `pinned_may_choice_relief` arm at gate (6) ⇒ arm 2's PINNED half ⇒ FAILS.
    /// * drop the residual re-classification in `pinned_may_choice_relief` (relieve whenever
    ///   the may slot is pinned) ⇒ arm 3's optional+proliferate half ⇒ FAILS.
    /// * drop the `entry.controller != proposer` conjunct in `entry_publishes_pin_slots`
    ///   ⇒ arm 4 ⇒ FAILS.
    /// * drop the ordering-input block in `entry_publishes_pin_slots` ⇒ arm 5's PINNED
    ///   half covers ⇒ FAILS (once per mutated field).
    /// * turn gate (6)'s `needs_life_guard = true` re-arm back into a `continue` ⇒ arm 6
    ///   covers ⇒ FAILS.
    ///
    /// Non-vacuity: every arm asserts BOTH directions (or pairs its negative with arm 1/2's
    /// positive on the same predicate), so neither a constant-`true` nor a constant-`false`
    /// relief survives.
    #[test]
    fn a_pinned_slot_skips_gate_three_and_six() {
        // ── arm 1: gate (3), open (≥2-legal) targeting ──
        let (prior, current) = grown_window(3, |id| drain_entry(id, vec![]));

        // Reach-guard: the rejector really is gate (3) on this board.
        assert!(
            !forced_unique_targeting(&current, current.stack[2].ability().unwrap()),
            "reach-guard: two opponents ⇒ not forced-unique ⇒ gate (3) is the rejector"
        );
        assert!(
            !loop_states_cover_modulo_growth_scoped(
                &prior,
                &current,
                pinned_scope(&[]),
                &mut PeriodVerdicts::for_period(&[], &current, PlayerId(0))
            ),
            "UNPINNED: an open per-opponent target choice is a free choice ⇒ reject"
        );
        let target_slot = churn_src_slot(&current, 0);
        assert!(
            loop_states_cover_modulo_growth_scoped(
                &prior,
                &current,
                pinned_scope(std::slice::from_ref(&target_slot)),
                &mut PeriodVerdicts::for_period(&[], &current, PlayerId(0))
            ),
            "PINNED: the offer published this slot ⇒ CR 732.2a specified choice ⇒ cover"
        );

        // ── arm 2: gate (6), a CR 603.5 "may" gate — the published resolution choice ──
        let (p_may, c_may) = grown_window(2, optional_drain);
        let may_ability = c_may.stack[2].ability().unwrap();
        // Reach-guards: gates (3) and (4) PASS here, so gate (6) is the rejector, and its
        // MayPrompt is the `optional` one.
        assert!(
            forced_unique_targeting(&c_may, may_ability),
            "reach-guard: the single opponent is forced-unique ⇒ gate (3) passes"
        );
        assert!(
            !crate::game::ability_scan::ability_reads_projected_resource(may_ability),
            "reach-guard: gate (4) passes ⇒ gate (6) is the rejector"
        );
        assert_eq!(
            crate::game::resolution_prompt::ability_resolution_choice_freedom(
                &c_may,
                may_ability,
                &mut ProbeBudget::for_test(PROBE_BUDGET)
            ),
            crate::game::resolution_prompt::ResolutionChoiceFreedom::MayPrompt,
            "reach-guard: CR 603.5 `optional` is what makes this entry MayPrompt"
        );
        assert!(
            !loop_states_cover_modulo_growth_scoped(
                &p_may,
                &c_may,
                pinned_scope(&[]),
                &mut PeriodVerdicts::for_period(&[], &c_may, PlayerId(0))
            ),
            "UNPINNED: an optional trigger's take/decline is a free choice ⇒ reject"
        );
        let may_slots = [churn_src_slot(&c_may, 0), churn_src_slot(&c_may, 1)];
        assert!(
            loop_states_cover_modulo_growth_scoped(
                &p_may,
                &c_may,
                pinned_scope(&may_slots),
                &mut PeriodVerdicts::for_period(&[], &c_may, PlayerId(0))
            ),
            "PINNED: the published CR 603.5 gate specifies that choice ⇒ cover"
        );
        // The MayChoice point is load-bearing on its own: pinning only the target slot
        // leaves the resolution choice unspecified. NOTE the target slot is pinned but NOT
        // published on this board — the target is forced-unique (asserted above), so
        // `a_forced_target_is_not_a_published_decision_point` is the row that owns that fact.
        // Gate (3) still passes here on the ordering-input arm; only gate (6) rejects.
        assert!(
            !loop_states_cover_modulo_growth_scoped(
                &p_may,
                &c_may,
                pinned_scope(&may_slots[..1]),
                &mut PeriodVerdicts::for_period(&[], &c_may, PlayerId(0))
            ),
            "a pinned TARGET does not specify the CR 603.5 take/decline choice"
        );

        // ── arm 3: gate (6) NON-ATTRIBUTION — CR 701.34a proliferate is never relieved ──
        for optional in [false, true] {
            let (p6, c6) = grown_window(2, |id| proliferate_drain(id, optional));
            let ability = c6.stack[2].ability().unwrap();
            assert!(
                forced_unique_targeting(&c6, ability),
                "reach-guard: gate (3) passes ⇒ gate (6) is the rejector (optional={optional})"
            );
            assert!(
                !crate::game::ability_scan::ability_reads_projected_resource(ability),
                "reach-guard: gate (4) passes (optional={optional})"
            );
            // Publish EVERYTHING this entry could publish — target slot and, when the
            // ability is optional, its CR 603.5 gate. The proliferate choice still has no
            // published pin, so no relief may be granted.
            let slots = [churn_src_slot(&c6, 0), churn_src_slot(&c6, 1)];
            for pinned in [&slots[..0], &slots[..1], &slots[..]] {
                assert!(
                    !loop_states_cover_modulo_growth_scoped(
                        &p6,
                        &c6,
                        pinned_scope(pinned),
                        &mut PeriodVerdicts::for_period(&[], &c6, PlayerId(0))
                    ),
                    "CR 701.34a proliferate is a resolution-time choice NO published slot \
                     specifies (optional={optional}, {} pinned slot(s))",
                    pinned.len()
                );
            }
        }

        // ── arm 4: precondition (c) — a non-proposer entry sharing the pinned source ──
        let foreign_drain = |id| {
            let mut ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
            ability.controller = PlayerId(1);
            ability.targets = vec![TargetRef::Player(PlayerId(0))];
            churn_entry(id, 1, ability, None)
        };
        let (p_foreign, c_foreign) = grown_window(3, foreign_drain);
        assert!(
            !forced_unique_targeting(&c_foreign, c_foreign.stack[2].ability().unwrap()),
            "reach-guard: P1 has two opponents ⇒ not forced-unique ⇒ gate (3) rejects"
        );
        assert_eq!(
            churn_src_slot(&c_foreign, 0),
            target_slot,
            "the foreign entry's source is BYTE-IDENTICAL to arm 1's pinned slot — the pin \
             list cannot discriminate it, only the controller conjunct can"
        );
        assert!(
            !loop_states_cover_modulo_growth_scoped(
                &p_foreign,
                &c_foreign,
                pinned_scope(std::slice::from_ref(&target_slot)),
                &mut PeriodVerdicts::for_period(&[], &c_foreign, PlayerId(0))
            ),
            "CR 732.2a precondition (c): P0's offer specifies none of P1's choices"
        );

        // ── arm 5: gate (3) SCOPE OF DISCHARGE ──
        // The relief is a bare `continue`, so it skips the WHOLE item-3 predicate — but a
        // published slot answers ONE target. Each row mutates exactly one of the other
        // announcement-time facts `stack_entry_has_no_ordering_input` rejects on, on a 2p
        // board where the target fact PASSES, so the mutated field is the sole rejector.
        {
            use crate::types::ability::MultiTargetSpec;
            use crate::types::game_state::TargetSelectionConstraint;
            type Mutate = fn(&mut ResolvedAbility);
            let mutations: [(&str, Mutate); 3] = [
                ("multi_target — CR 601.2c variable target count", |a| {
                    a.multi_target = Some(MultiTargetSpec::fixed(1, 2))
                }),
                ("distribution — CR 601.2d divide-among", |a| {
                    a.distribution = Some(vec![(TargetRef::Player(PlayerId(1)), 1)])
                }),
                ("target_constraints — CR 601.2c cross-target", |a| {
                    a.target_constraints = vec![TargetSelectionConstraint::DifferentTargetPlayers]
                }),
            ];
            for (label, mutate) in mutations {
                let (p5, c5) = grown_window(2, |id| {
                    let mut e = drain_entry(id, vec![]);
                    let StackEntryKind::TriggeredAbility { ability, .. } = &mut e.kind else {
                        unreachable!("drain_entry builds a TriggeredAbility")
                    };
                    mutate(ability.as_mut());
                    e
                });
                assert!(
                    forced_unique_targeting(&c5, c5.stack[2].ability().unwrap()),
                    "reach-guard: the single opponent is forced-unique ⇒ the TARGET fact is \
                     not what rejects ({label})"
                );
                assert!(
                    !stack_entry_has_no_ordering_input(&c5, &c5.stack[2]),
                    "reach-guard: {label} is therefore item 3's SOLE rejector"
                );
                let slot = churn_src_slot(&c5, 0);
                for pinned in [&[][..], std::slice::from_ref(&slot)] {
                    assert!(
                        !loop_states_cover_modulo_growth_scoped(
                            &p5,
                            &c5,
                            pinned_scope(pinned),
                            &mut PeriodVerdicts::for_period(&[], &c5, PlayerId(0))
                        ),
                        "a published slot specifies ONE target; {label} is announcement-time \
                         ordering input no slot specifies ({} pinned)",
                        pinned.len()
                    );
                }
            }

            // The FOURTH fact, `pending_trigger_entry` (CR 603.3c mid-construction), is
            // state-dependent, so it lives on the relief predicate rather than in the pure
            // mint — and it is REACHABLE through the cover, so the row is written there.
            // Measured: `normalize_for_loop` leaves the field AS-IS, so a `current` carrying
            // one that `prior` lacks does fail gate (1) — but when BOTH frames carry it,
            // gate (1) passes and gate (3) is the sole rejector. Both controls below.
            // (Both production call sites compare states at `WaitingFor::Priority`, where
            // the field is `None`; that bounds the exposure, it does not make it
            // unreachable.)
            let (p_pend, c_pend) = grown_window(2, |id| drain_entry(id, vec![]));
            let pend_slot = churn_src_slot(&c_pend, 0);
            assert!(
                loop_states_cover_modulo_growth_scoped(
                    &p_pend,
                    &c_pend,
                    pinned_scope(std::slice::from_ref(&pend_slot)),
                    &mut PeriodVerdicts::for_period(&[], &c_pend, PlayerId(0))
                ),
                "positive control: this entry's published slot IS otherwise matched"
            );
            let (mut p_mid, mut c_mid) = (p_pend.clone(), c_pend.clone());
            let mid = c_mid.stack[2].id;
            for s in [&mut p_mid, &mut c_mid] {
                s.pending_trigger_entry = Some(mid);
            }
            assert!(
                !loop_states_cover_modulo_growth_scoped(
                    &p_mid,
                    &c_mid,
                    pinned_scope(std::slice::from_ref(&pend_slot)),
                    &mut PeriodVerdicts::for_period(&[], &c_mid, PlayerId(0))
                ),
                "CR 603.3c: a mid-construction announcement is not specified by any \
                 published slot — the relief must not discharge item 3's firewall"
            );
            // The gate-(1) control for the row above: the SAME both-frames shape naming no
            // live entry still covers, so the rejection there is gate (3), not the mere
            // presence of a non-`None` field.
            let (mut p_other, mut c_other) = (p_pend, c_pend);
            for s in [&mut p_other, &mut c_other] {
                s.pending_trigger_entry = Some(ObjectId(u64::MAX));
            }
            assert!(
                loop_states_cover_modulo_growth_scoped(
                    &p_other,
                    &c_other,
                    pinned_scope(std::slice::from_ref(&pend_slot)),
                    &mut PeriodVerdicts::for_period(&[], &c_other, PlayerId(0))
                ),
                "gate-(1) control: a `pending_trigger_entry` naming no live entry is not \
                 what rejects — CR 603.3c's firewall is entry-scoped"
            );
        }

        // ── arm 6: gate (6) RESIDUAL re-arms the CR 616.1 environmental guard ──
        // Arm 2's board (relief GRANTED there) plus one optional life replacement. A
        // pinned CR 603.5 gate says nothing about whose life-event replacements prompt.
        {
            use crate::types::ability::ReplacementMode;
            let (mut p_life, mut c_life) = grown_window(2, optional_drain);
            let mut def = ReplacementDefinition::new(ReplacementEvent::LoseLife);
            def.mode = ReplacementMode::Optional { decline: None };
            // CR 614.1a: a `valid_player`-less player-event replacement applies only
            // to ITS controller's events (`replacement_source_player`). The residual
            // this arm must re-arm is the drain's own `LifeLoss` on the OPPONENT
            // (P1), so the def has to sit on a P1-controlled permanent to be drawn
            // as a candidate at all. Measured: on a P0 permanent the same def draws
            // ZERO candidates for `LifeLoss{P1}` and the arm would pass vacuously.
            for state in [&mut p_life, &mut c_life] {
                let oid = bf_object_owned_by(state, 812, PlayerId(1));
                state
                    .objects
                    .get_mut(&oid)
                    .unwrap()
                    .replacement_definitions
                    .push(def.clone());
            }
            let life_loss = crate::types::proposed_event::ProposedEvent::LifeLoss {
                player_id: PlayerId(1),
                amount: 1,
                applied: Default::default(),
            };
            assert!(
                crate::game::replacement::proposed_event_prompt_cause(
                    &c_life,
                    &life_loss,
                    crate::game::replacement::replacement_registry(),
                )
                .contains(crate::game::replacement::ReplacementPromptCause::OptionalCandidate),
                "reach-guard: the installed optional def makes the CR 614.1a surface \
                 prompt-capable for a LifeLoss event (arm 2's bare board does not)"
            );
            let slots = [churn_src_slot(&c_life, 0), churn_src_slot(&c_life, 1)];
            assert!(
                !loop_states_cover_modulo_growth_scoped(
                    &p_life,
                    &c_life,
                    pinned_scope(&slots),
                    &mut PeriodVerdicts::for_period(&[], &c_life, PlayerId(0))
                ),
                "the discharged `may` leaves a FreeUnlessReplacements(LIFE) RESIDUAL that must \
                 re-arm the CR 616.1 guard — relief is not a `continue`"
            );
        }

        // ── non-widening control: an unrelated slot relieves nothing ──
        let unrelated = DecisionSlot {
            source: crate::types::game_state::YieldTarget::ThisObject {
                source_id: ObjectId(CHURN_SRC + 1),
                incarnation: Some(0),
                trigger_description: None,
            },
            index: 0,
        };
        for (p, c, label) in [(&prior, &current, "gate (3)"), (&p_may, &c_may, "gate (6)")] {
            assert!(
                !loop_states_cover_modulo_growth_scoped(
                    p,
                    c,
                    pinned_scope(std::slice::from_ref(&unrelated)),
                    &mut PeriodVerdicts::for_period(&[], c, PlayerId(0))
                ),
                "{label}: a pin on a DIFFERENT source must not relieve this entry"
            );
        }

        // ── CR 400.7 control: a stale incarnation is a different slot ──
        let stale = DecisionSlot {
            source: crate::types::game_state::YieldTarget::ThisObject {
                source_id: ObjectId(CHURN_SRC),
                incarnation: Some(u64::MAX),
                trigger_description: None,
            },
            index: 0,
        };
        assert!(
            !loop_states_cover_modulo_growth_scoped(
                &prior,
                &current,
                pinned_scope(std::slice::from_ref(&stale)),
                &mut PeriodVerdicts::for_period(&[], &current, PlayerId(0))
            ),
            "CR 400.7: a pin latched to a stale incarnation does not match the live source"
        );
    }

    /// CONSTRAINT-3 ORTHOGONALITY: an item-3-passing, item-4-clean forced-unique
    /// drain that ALSO carries a `Proliferate` sub_ability (CR 701.34a resolution
    /// choice ⇒ `MayPrompt`) is vetoed by item-6. Revert-probe: dropping the
    /// Proliferate sub (choice-free) flips this TRUE (= the positive fixture).
    #[test]
    fn item6_still_vetoes_under_forced_unique_targets() {
        let drain_prolif = |id| {
            let mut ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
            ability.targets = vec![TargetRef::Player(PlayerId(1))];
            ability.sub_ability = Some(Box::new(ResolvedAbility::new(
                Effect::Proliferate,
                vec![],
                ObjectId(CHURN_SRC),
                PlayerId(0),
            )));
            churn_entry(id, 0, ability, None)
        };
        let mut prior = drain_state(2);
        prior.stack.push_back(drain_prolif(10));
        prior.stack.push_back(drain_prolif(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(drain_prolif(20));
        current.stack.push_back(drain_prolif(21));
        current.stack.push_back(drain_prolif(22));

        // Reach-guard (mandate 4 anti-vacuity): item-3 AND item-4 PASS for this entry,
        // so the FALSE below is ATTRIBUTABLE to item-6's Proliferate veto — not an
        // upstream conjunct short-circuiting first.
        let ability = current.stack[2].ability().unwrap();
        assert!(
            forced_unique_targeting(&current, ability),
            "item-3 passes (single forced-unique opponent) even with the Proliferate sub"
        );
        assert!(
            !crate::game::ability_scan::ability_reads_projected_resource(ability),
            "item-4 passes (Proliferate sub scans NONE; pure-controller Typed target)"
        );

        assert!(
            !loop_states_cover_modulo_growth(&prior, &current),
            "item-6 vetoes the resolution-choice-bearing drain even when item-3/4 pass"
        );
    }

    /// (c) the grown entry is a SPELL ⇒ false (not a mandatory trigger). Isolates
    /// item 3's `TriggeredAbility`-only requirement.
    #[test]
    fn n1_c_grown_entry_spell_false() {
        let spell = |id| StackEntry {
            id: ObjectId(id),
            source_id: ObjectId(CHURN_SRC),
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        };
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(spell(10));
        prior.stack.push_back(spell(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(spell(20));
        current.stack.push_back(spell(21));
        current.stack.push_back(spell(22));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (d) a prior entry-kind absent from `current` ⇒ false (embedding fails).
    /// prior `[G, B]`, current `[G, G]` — B (controller 1) never matches.
    #[test]
    fn n1_d_embedding_missing_kind_false() {
        let b = |id| churn_entry(id, 1, gain_ability(1), None);
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(g(10));
        prior.stack.push_back(b(11));
        let mut current = GameState::new_two_player(7);
        current.stack.push_back(g(20));
        current.stack.push_back(g(21));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (e) equal stacks, no strict growth ⇒ false (that is the equality case).
    #[test]
    fn n1_e_no_growth_false() {
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(g(10));
        prior.stack.push_back(g(11));
        let current = prior.clone();
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (f) WIPE-PENDING (R1-B1): a distinct mandatory no-input trigger kind absent
    /// from `prior` grows 0→1 at an UNOCCUPIED place ⇒ false. `W` reads no projected
    /// resource, so removing the prior-occupancy guard (2b) flips this true — the
    /// false win fires.
    #[test]
    fn n1_f_wipe_pending_unoccupied_growth_false() {
        // W = a distinct-kind mandatory no-input trigger (GainLife 7, no read).
        let w = |id| churn_entry(id, 0, gain_ability(7), None);
        let (mut prior, mut current) = cover_base(); // [G,G] / [G,G,G]
                                                     // Rebuild current as [G,G,W]: G did not grow, W is the 0→1 new kind.
        current.stack.clear();
        current.stack.push_back(g(20));
        current.stack.push_back(g(21));
        current.stack.push_back(w(22));
        let _ = &mut prior;
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (g) PERMUTATION (R1-M3): prior `[B,A]`, current `[A,B,B]` ⇒ false (no
    /// bottom-up embedding: no A after the first B match). Revert-fail for replacing
    /// embedding with order-blind multiset containment.
    #[test]
    fn n1_g_permutation_false() {
        let a = |id| churn_entry(id, 0, gain_ability(1), None);
        let b = |id| churn_entry(id, 1, gain_ability(1), None);
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(b(10)); // [B, A]
        prior.stack.push_back(a(11));
        let mut current = GameState::new_two_player(7);
        current.stack.push_back(a(20)); // [A, B, B]
        current.stack.push_back(b(21));
        current.stack.push_back(b(22));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (h) RESOURCE-READ (R1-B2): a churning entry whose trigger-level intervening-if
    /// reads a projected resource (life) ⇒ false. Revert-fail for dropping item 4.
    #[test]
    fn n1_h_resource_read_false() {
        let h = |id| {
            churn_entry(
                id,
                0,
                gain_ability(1),
                Some(TriggerCondition::LifeTotalGE { minimum: 10 }),
            )
        };
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(h(10));
        prior.stack.push_back(h(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(h(20));
        current.stack.push_back(h(21));
        current.stack.push_back(h(22));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (i) an OPPONENT-controlled otherwise-identical grown trigger ⇒ distinct
    /// normalized kind (controller kept). prior occupied only by the controller's
    /// kind ⇒ the grown opponent kind is 0→1 unoccupied ⇒ false. Revert-fail:
    /// dropping `controller` from the key flips this true.
    #[test]
    fn n1_i_opponent_controlled_growth_false() {
        let (_p, _c) = cover_base();
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(g(10)); // [G(c0), G(c0)]
        prior.stack.push_back(g(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(g(20)); // [G(c0), G(c0), G(c1)]
        current.stack.push_back(g(21));
        current
            .stack
            .push_back(churn_entry(22, 1, gain_ability(1), None));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (j) JOURNAL-READER (R2 B-R2-1): a fixed-amount drain churner whose embedded
    /// ability carries an `NthResolutionThisTurn`-gated branch reads the cleared
    /// per-ability resolution journal ⇒ false. Revert-fail: narrowing the walker
    /// guard axis back to resources-only (dropping journal readers) flips this true.
    #[test]
    fn n1_j_journal_reader_false() {
        let j = |id| {
            let mut ability = gain_ability(1);
            ability.condition = Some(AbilityCondition::NthResolutionThisTurn { n: 10 });
            churn_entry(id, 0, ability, None)
        };
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(j(10));
        prior.stack.push_back(j(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(j(20));
        current.stack.push_back(j(21));
        current.stack.push_back(j(22));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (k) DORMANT-TRIGGER (R4-G1): a genuine covering drain while a battlefield
    /// permanent carries a mandatory trigger DEFINITION whose fire-time condition
    /// reads life — it produces NO stack entry on either frame ⇒ false via the
    /// second (off-stack) scan surface. Revert-fail: removing the item-5 scan.
    #[test]
    fn n1_k_dormant_trigger_condition_false() {
        let (mut prior, mut current) = cover_base();
        for state in [&mut prior, &mut current] {
            let oid = bf_object(state, 800);
            let mut def = TriggerDefinition::new(TriggerMode::LifeLost);
            def.condition = Some(TriggerCondition::LifeTotalGE { minimum: 6 });
            state
                .objects
                .get_mut(&oid)
                .unwrap()
                .trigger_definitions
                .push(def);
        }
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (k-g) DORMANT GRANTED-KEYWORD TRIGGER (inc2b hole): a genuine covering drain
    /// while a battlefield permanent carries a runtime-GRANTED Dethrone (CR 702.105a)
    /// whose synthesized fire-time intervening-if reads `LifeTotal` (CR 119,
    /// projected). The granted trigger is NOT on `obj.trigger_definitions` — it is
    /// synthesized on-the-fly by `synthesize_granted_keyword_triggers`, so loop (i)
    /// never sees it; only loop (iv)'s reuse of `granted_keyword_triggers_in_zone`
    /// catches the dormant condition ⇒ false. Revert-fail: deleting loop (iv) leaves
    /// the synthesized def unscanned, item-5 returns false, and the cover shortcut
    /// (a false WIN, N1(k) class) is wrongly taken ⇒ this assertion flips to true.
    #[test]
    fn n1_kg_dormant_granted_keyword_trigger_condition_false() {
        let (mut prior, mut current) = cover_base();
        for state in [&mut prior, &mut current] {
            let oid = bf_object(state, 803);
            // Granted (not printed): push onto `keywords` only, leaving
            // `base_keywords` empty so `synthesize_granted_keyword_triggers`
            // classifies it as granted and produces the life-reading trigger. The
            // trigger itself is deliberately NOT installed on `trigger_definitions`
            // (that is what makes loop (i) miss it, per the inc2b hole).
            state
                .objects
                .get_mut(&oid)
                .unwrap()
                .keywords
                .push(crate::types::keywords::Keyword::Dethrone);
        }
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (k-r) a battlefield REPLACEMENT definition whose condition reads life ⇒ false.
    #[test]
    fn n1_kr_dormant_replacement_condition_false() {
        let (mut prior, mut current) = cover_base();
        for state in [&mut prior, &mut current] {
            let oid = bf_object(state, 801);
            let mut def = ReplacementDefinition::new(ReplacementEvent::LoseLife);
            def.condition = Some(ReplacementCondition::UnlessPlayerLifeAtMost { amount: 5 });
            state
                .objects
                .get_mut(&oid)
                .unwrap()
                .replacement_definitions
                .push(def);
        }
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (k-s) a dormant condition-gated STATIC (any mode) whose condition reads a
    /// projected axis (poison) ⇒ false (the CR 101.2 firewall reads only live state
    /// and cannot see it arm; the off-stack static scan catches it).
    #[test]
    fn n1_ks_dormant_static_condition_false() {
        let (mut prior, mut current) = cover_base();
        for state in [&mut prior, &mut current] {
            let oid = bf_object(state, 802);
            let mut def = StaticDefinition::new(StaticMode::CantLoseTheGame);
            def.condition = Some(StaticCondition::OpponentPoisonAtLeast { count: 1 });
            state
                .objects
                .get_mut(&oid)
                .unwrap()
                .static_definitions
                .push(def);
        }
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (l) DRIFTING MISSED READER (R4-G3): an on-stack entry whose trigger-level
    /// intervening-if is `GainedLife` — reads `life_gained_this_turn`, which drifts
    /// +1/cycle in the very drain window being certified ⇒ false. Revert-fail:
    /// classifying `GainedLife` as a non-reader in the walker flips this true.
    #[test]
    fn n1_l_gained_life_journal_reader_false() {
        let l = |id| {
            churn_entry(
                id,
                0,
                gain_ability(1),
                Some(TriggerCondition::GainedLife { minimum: 30 }),
            )
        };
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(l(10));
        prior.stack.push_back(l(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(l(20));
        current.stack.push_back(l(21));
        current.stack.push_back(l(22));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (m) OBJECT-AXIS COUNTER RIDER (R5-B1): a genuine covering drain but `current`
    /// carries one more monotone `-1/-1` counter on a shared battlefield creature
    /// than `prior` (projection-invisible) ⇒ false via `object_resource_axes_match`.
    /// Revert-fail: dropping that strict compare flips this true (and in real play
    /// CR 704.5f/g graveyards the churner source and the cascade extinguishes).
    #[test]
    fn n1_m_object_counter_rider_false() {
        let (mut prior, mut current) = cover_base();
        // Shared creature in both frames; monotone -1/-1 counter drifts +1 in current.
        for (state, extra) in [(&mut prior, 1u32), (&mut current, 2u32)] {
            let oid = ObjectId(850);
            let mut object = crate::game::game_object::GameObject::new(
                oid,
                CardId(9),
                PlayerId(0),
                "Test Churner Source".to_string(),
                Zone::Battlefield,
            );
            object.card_types.core_types = vec![CoreType::Creature];
            object.counters.insert(CounterType::Minus1Minus1, extra);
            state.objects.insert(oid, object);
            state.battlefield.push_back(oid);
        }
        // Sanity: the projection hides it (the 2p equality path would still match).
        let mut pa = project_out_resources(&prior);
        let mut pb = project_out_resources(&current);
        pa.stack.clear();
        pb.stack.clear();
        assert!(
            loop_states_equal(&pa, &pb),
            "fixture: the -1/-1 counter drift is projection-invisible (isolates B1)"
        );
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    /// (n) PLAYER-COUNTER RIDER (R5-MAJOR): a fixed-amount drain churner whose ability
    /// reads a projected player-counter axis (experience — NO winner-predicate
    /// firewall) ⇒ false. Revert-fail: declassifying `PlayerCounter` in the walker.
    #[test]
    fn n1_n_player_counter_reader_false() {
        let n = |id| {
            let ability = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::PlayerCounter {
                            kind: PlayerCounterKind::Experience,
                            scope: CountScope::Controller,
                        },
                    },
                    player: TargetFilter::Controller,
                },
                vec![],
                ObjectId(CHURN_SRC),
                PlayerId(0),
            );
            churn_entry(id, 0, ability, None)
        };
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(n(10));
        prior.stack.push_back(n(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(n(20));
        current.stack.push_back(n(21));
        current.stack.push_back(n(22));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));
    }

    // ===================================================================
    // N1 item-6 hostiles (resolution-time choice gate). n1_o/q/r/s.
    // ===================================================================

    /// A no-ordering-input `Effect::Proliferate` churner (unit variant, empty
    /// announced targets) — passes items 1-5 (Proliferate reads no projected
    /// axis, scan_effect ⇒ Axes::NONE) but is a resolution-choice opener (item 6).
    fn proliferate_ability() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Proliferate,
            vec![],
            ObjectId(CHURN_SRC),
            PlayerId(0),
        )
    }

    /// Fixed-amount `LoseLife` churner — allow-listed
    /// (`FreeUnlessReplacements(LIFE)`), reads no projected resource. Distinct
    /// normalized kind from `gain_ability`.
    fn lose_ability(amount: i32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: amount },
                target: None,
            },
            vec![],
            ObjectId(CHURN_SRC),
            PlayerId(0),
        )
    }

    /// (o) GROWN CHOICE-OPENING KIND (finding fixtures i + iii): prior `[G, P]`,
    /// current `[G, P, P]` — `P` (Proliferate) grows on an occupied place. ZERO
    /// counters anywhere, so in `current` the grown `P` would AUTO-resolve without
    /// a prompt (`eligible.is_empty()`, proliferate.rs:90) — proving the gate is
    /// STRUCTURAL, not observational (the projected poison axis, CR 701.34a, can
    /// inhabit the option surface mid-extrapolation). Item 4 does NOT mask this:
    /// `scan_effect(Proliferate)` is `Axes::NONE`. Revert-fail: delete the item-6
    /// loop, or classify `Proliferate` ⇒ `FreeUnlessReplacements`.
    #[test]
    fn n1_o_grown_choice_opening_proliferate_false() {
        let p = |id| churn_entry(id, 0, proliferate_ability(), None);
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(g(10)); // [G, P]
        prior.stack.push_back(p(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(g(20)); // [G, P, P]
        current.stack.push_back(p(21));
        current.stack.push_back(p(22));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));

        // Reach-guard: swap `P` for a distinct GainLife kind (gain_ability(2)) ⇒
        // the same growth passes items 1-5 AND item 6 (all allow-listed, no life
        // replacements) ⇒ cover true. Isolates item 6's Proliferate reject.
        let g2 = |id| churn_entry(id, 0, gain_ability(2), None);
        let mut prior2 = GameState::new_two_player(7);
        prior2.stack.push_back(g(30));
        prior2.stack.push_back(g2(31));
        let mut current2 = prior2.clone();
        current2.stack.clear();
        current2.stack.push_back(g(40));
        current2.stack.push_back(g2(41));
        current2.stack.push_back(g2(42));
        assert!(loop_states_cover_modulo_growth(&prior2, &current2));
    }

    /// (q) UN-GROWN CHOICE-OPENING ENTRY (H2 discriminator): prior `[P, G]`,
    /// current `[P, G, G]` — `P` count EQUAL (un-grown), `G` (allow-listed) grows.
    /// Item 3 only checks GROWN entries, so the un-grown `P` is invisible to it;
    /// ONLY item 6's all-entries scope rejects the `P`. Revert-fail: scope item 6
    /// to `cn > pn` entries only ⇒ this flips true.
    #[test]
    fn n1_q_ungrown_choice_opening_entry_false() {
        let p = |id| churn_entry(id, 0, proliferate_ability(), None);
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(p(10)); // [P, G]
        prior.stack.push_back(g(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(p(20)); // [P, G, G]
        current.stack.push_back(g(21));
        current.stack.push_back(g(22));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));

        // Reach-guard: drop the un-grown `P` ⇒ pure GainLife growth ⇒ cover true.
        let mut prior2 = GameState::new_two_player(7);
        prior2.stack.push_back(g(30));
        let mut current2 = prior2.clone();
        current2.stack.clear();
        current2.stack.push_back(g(40));
        current2.stack.push_back(g(41));
        assert!(loop_states_cover_modulo_growth(&prior2, &current2));
    }

    /// (r) LIFE-REPLACEMENT ENVIRONMENT (H4): a genuine covering drain while a
    /// battlefield (or floating) replacement can open a resolution-time prompt on
    /// the grown `GainLife`/`LoseLife` resolution. Five arms — each def is
    /// condition-free with no projected-reading body, so it SURVIVES items 1-5
    /// and ONLY item 6's environmental guard rejects. The shared reach-guard (a
    /// non-life event ⇒ cover true) proves the fixtures pass gates 1-5.
    #[test]
    fn n1_r_life_replacement_environment_false() {
        use crate::types::ability::ReplacementMode;

        // Install a replacement def on a battlefield object present in BOTH states.
        fn with_object_def(def: ReplacementDefinition) -> (GameState, GameState) {
            let (mut prior, mut current) = cover_base();
            for state in [&mut prior, &mut current] {
                // Owned by P0 — the player whose life gain these defs replace.
                let oid = bf_object_owned_by(state, 810, PlayerId(0));
                state
                    .objects
                    .get_mut(&oid)
                    .unwrap()
                    .replacement_definitions
                    .push(def.clone());
            }
            (prior, current)
        }

        // Arm 1 (clause a): a single OPTIONAL GainLife def ⇒ prompt
        // (replacement.rs:6221). Mutation: delete the `needs_life_guard` block ⇒ RED.
        let mut def = ReplacementDefinition::new(ReplacementEvent::GainLife);
        def.mode = ReplacementMode::Optional { decline: None };
        let (prior, current) = with_object_def(def);
        assert!(
            !loop_states_cover_modulo_growth(&prior, &current),
            "arm1 optional GainLife"
        );

        // Arm 2 (clause c): TWO MANDATORY GainLife defs ⇒ ≥2 per LifeGain class
        // (CR 616.1 material ordering). Mutation: drop clause (c) ⇒ RED.
        {
            let (mut prior, mut current) = cover_base();
            for state in [&mut prior, &mut current] {
                let oid = bf_object_owned_by(state, 811, PlayerId(0));
                let obj = state.objects.get_mut(&oid).unwrap();
                // CR 616.1: ordering is a choice only when it is MATERIAL. Two
                // no-op definitions COMMUTE, so the fixture must carry two
                // modifications whose composition order changes the result —
                // `+1` then `×2` is 4, `×2` then `+1` is 3. The def-scan this
                // replaces counted definitions instead of asking the pipeline.
                let mut plus = ReplacementDefinition::new(ReplacementEvent::GainLife);
                plus.quantity_modification =
                    Some(crate::types::ability::QuantityModification::Plus { value: 1 });
                let mut times = ReplacementDefinition::new(ReplacementEvent::GainLife);
                times.quantity_modification =
                    Some(crate::types::ability::QuantityModification::Times { factor: 2 });
                obj.replacement_definitions.push(plus);
                obj.replacement_definitions.push(times);
            }
            assert!(
                !loop_states_cover_modulo_growth(&prior, &current),
                "arm2 two mandatory GainLife defs"
            );
        }

        // Arm 3 (B1 — PayLife class-set completeness): an optional PayLife def
        // (matcher matches ProposedEvent::LifeLoss, replacement.rs:3324) over a
        // LoseLife drain ⇒ prompt. Mutation: narrow the life-class set to
        // {GainLife, LoseLife} (drop PayLife) ⇒ RED.
        {
            let l = |id| churn_entry(id, 0, lose_ability(1), None);
            let mut prior = GameState::new_two_player(7);
            prior.stack.push_back(l(10));
            prior.stack.push_back(l(11));
            let mut current = prior.clone();
            current.stack.clear();
            current.stack.push_back(l(20));
            current.stack.push_back(l(21));
            current.stack.push_back(l(22));
            for state in [&mut prior, &mut current] {
                let oid = bf_object_owned_by(state, 812, PlayerId(0));
                let mut def = ReplacementDefinition::new(ReplacementEvent::PayLife);
                def.mode = ReplacementMode::Optional { decline: None };
                state
                    .objects
                    .get_mut(&oid)
                    .unwrap()
                    .replacement_definitions
                    .push(def);
            }
            assert!(
                !loop_states_cover_modulo_growth(&prior, &current),
                "arm3 optional PayLife over LoseLife drain"
            );
        }

        // Arm 4 (B2 — clause b): a single MANDATORY GainLife def with a
        // prompt-capable, non-projected-reading `runtime_execute` body ⇒ prompt.
        // Mutation: drop the `runtime_execute.is_some()` half of clause (b) ⇒ RED.
        {
            let runtime_body = ResolvedAbility::new(
                Effect::Sacrifice {
                    target: TargetFilter::Any,
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                },
                vec![],
                ObjectId(CHURN_SRC),
                PlayerId(0),
            );
            // Item-5 pass proof: the body reads NO projected resource, so item 5
            // (which scans `runtime_execute` only for projected reads) lets the def
            // through — only clause (b) rejects.
            assert!(!crate::game::ability_scan::ability_reads_projected_resource(&runtime_body));
            let def = ReplacementDefinition::new(ReplacementEvent::GainLife)
                .runtime_execute(runtime_body);
            let (prior, current) = with_object_def(def);
            assert!(
                !loop_states_cover_modulo_growth(&prior, &current),
                "arm4 mandatory GainLife with runtime_execute body"
            );
        }

        // Arm 5 (M3 — floating store): the arm-1 optional GainLife def placed in
        // `state.pending_damage_replacements` (no object def) ⇒ prompt. Mutation:
        // drop the floating-store chain from the guard's def sources ⇒ RED.
        {
            let (mut prior, mut current) = cover_base();
            let mut def = ReplacementDefinition::new(ReplacementEvent::GainLife);
            def.mode = ReplacementMode::Optional { decline: None };
            for state in [&mut prior, &mut current] {
                state.pending_damage_replacements.push(def.clone());
            }
            assert!(
                !loop_states_cover_modulo_growth(&prior, &current),
                "arm5 floating-store optional GainLife"
            );
        }

        // Shared reach-guard: the arm-1 def with a NON-LIFE event (Mill) ⇒ cover
        // true (proves the fixtures pass gates 1-5; only the life-class match rejects).
        {
            let mut def = ReplacementDefinition::new(ReplacementEvent::Mill);
            def.mode = ReplacementMode::Optional { decline: None };
            let (prior, current) = with_object_def(def);
            assert!(
                loop_states_cover_modulo_growth(&prior, &current),
                "reach-guard: non-life (Mill) replacement does not reject"
            );
        }
    }

    /// (s) RESOLUTION-TIMING TARGET SLOTS (H3): a grown GainLife whose ability
    /// defers target choice to RESOLUTION (CR 608.2d). `targets` is empty on the
    /// stack, so today's ordering gate (item 3) passes it; only item 6's
    /// `target_choice_timing == Resolution` row rejects. Revert-fail: remove the
    /// `target_choice_timing` row from the ability classifier ⇒ this flips true.
    #[test]
    fn n1_s_resolution_timing_targets_false() {
        use crate::types::ability::TargetChoiceTiming;
        let res = |id| {
            let mut ability = gain_ability(1);
            ability.target_choice_timing = TargetChoiceTiming::Resolution;
            churn_entry(id, 0, ability, None)
        };
        let mut prior = GameState::new_two_player(7);
        prior.stack.push_back(res(10));
        prior.stack.push_back(res(11));
        let mut current = prior.clone();
        current.stack.clear();
        current.stack.push_back(res(20));
        current.stack.push_back(res(21));
        current.stack.push_back(res(22));
        assert!(!loop_states_cover_modulo_growth(&prior, &current));

        // Reach-guard: identical ability with STACK timing ⇒ cover true.
        let stk = |id| churn_entry(id, 0, gain_ability(1), None);
        let mut prior2 = GameState::new_two_player(7);
        prior2.stack.push_back(stk(10));
        prior2.stack.push_back(stk(11));
        let mut current2 = prior2.clone();
        current2.stack.clear();
        current2.stack.push_back(stk(20));
        current2.stack.push_back(stk(21));
        current2.stack.push_back(stk(22));
        assert!(loop_states_cover_modulo_growth(&prior2, &current2));
    }

    // =======================================================================
    // PR-7 Phase 4a — offline OBJECT-GROWTH cover predicate
    // (`loop_states_cover_modulo_object_growth`). Synthetic frame-pairs assert
    // the bool. Non-vacuous: each REJECT fails (returns COVER) if its named gate
    // is reverted; each COVER fails if a gate over-rejects.
    // =======================================================================

    /// An inert battlefield token: `GameObject::new` defaults (no defs, no
    /// abilities, no keywords, no counters, non-legendary), inserted into BOTH the
    /// object map AND `state.battlefield` (the inert-class confine iterates the
    /// battlefield vector). Same `name` ⇒ same inert class.
    fn inert_token(state: &mut GameState, id: u64, controller: u8, name: &str) -> ObjectId {
        let oid = ObjectId(id);
        let object = GameObject::new(
            oid,
            CardId(id),
            PlayerId(controller),
            name.into(),
            Zone::Battlefield,
        );
        state.objects.insert(oid, object);
        state.battlefield.push_back(oid);
        oid
    }

    /// A card in hand carrying `keywords`, identical in both frames (a recast
    /// engine's off-battlefield source). Scanned by the all-zones cost firewall.
    fn hand_card_with_keywords(
        state: &mut GameState,
        id: u64,
        keywords: Vec<crate::types::keywords::Keyword>,
    ) {
        let oid = ObjectId(id);
        let mut object = GameObject::new(oid, CardId(id), PlayerId(0), "Engine".into(), Zone::Hand);
        object.keywords = keywords;
        state.objects.insert(oid, object);
    }

    /// C1 base: a steady-state inert-token engine grown by exactly one token of the
    /// SAME inert class. Prior = 2 tokens, current = 3.
    fn og_cover_base() -> (GameState, GameState) {
        let mut prior = GameState::new_two_player(7);
        inert_token(&mut prior, 700, 0, "Saproling");
        inert_token(&mut prior, 701, 0, "Saproling");
        let mut current = prior.clone();
        inert_token(&mut current, 702, 0, "Saproling");
        (prior, current)
    }

    fn cover(prior: &GameState, current: &GameState) -> bool {
        loop_states_cover_modulo_object_growth(prior, current)
    }

    /// A CONSERVATIVE (sibling-reading) effect: `Effect::Pump` classifies
    /// `Axes::CONSERVATIVE` regardless of its fields (ability_scan.rs).
    fn sibling_reading_effect() -> crate::types::ability::Effect {
        use crate::types::ability::{Effect, PtValue, TargetFilter};
        Effect::Pump {
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            target: TargetFilter::SelfRef,
        }
    }

    /// C1 (COVER): a mana-neutral inert-token engine, grown by one same-class token.
    #[test]
    fn object_growth_c1_inert_token_engine_covers() {
        let (prior, current) = og_cover_base();
        assert!(
            cover(&prior, &current),
            "pure inert single-token growth of an existing class must COVER"
        );
    }

    /// C2 (COVER): growth by MORE than one same-class token still covers.
    #[test]
    fn object_growth_c2_multi_token_growth_covers() {
        let mut prior = GameState::new_two_player(7);
        inert_token(&mut prior, 700, 0, "Saproling");
        let mut current = prior.clone();
        inert_token(&mut current, 701, 0, "Saproling");
        inert_token(&mut current, 702, 0, "Saproling");
        assert!(
            cover(&prior, &current),
            "multi-token inert growth must COVER"
        );
    }

    /// K-offline (HARD GATE, REJECT): the Witherbloom + Sprout Swarm shape — inert
    /// Saproling growth driven by a Convoke recast. §6 keystone: the detector models
    /// NO cast-time cost, so a board-scaling cost keyword is REJECTED. Revert-failing:
    /// removing Convoke from `keyword_cost_reads_growing_class` flips this to COVER —
    /// the paired control proves Convoke is the sole rejector.
    #[test]
    fn object_growth_k_offline_convoke_rejects() {
        use crate::types::keywords::Keyword;
        let (mut prior, mut current) = og_cover_base();
        hand_card_with_keywords(&mut prior, 900, vec![Keyword::Convoke]);
        hand_card_with_keywords(&mut current, 900, vec![Keyword::Convoke]);
        assert!(
            !cover(&prior, &current),
            "K-offline: a Convoke recast over growing Saprolings must REJECT (§6 keystone)"
        );
        // Control: the SAME frame-pair with a non-cost keyword COVERS — proving the
        // reject is the cost-keyword classifier, not any other gate.
        let (mut p2, mut c2) = og_cover_base();
        hand_card_with_keywords(&mut p2, 900, vec![Keyword::Flying]);
        hand_card_with_keywords(&mut c2, 900, vec![Keyword::Flying]);
        assert!(
            cover(&p2, &c2),
            "control: an inert (non-cost) keyword must NOT reject the same growth"
        );
    }

    /// R-a (REJECT): a battlefield object LEAVES while another is added — a shrink is
    /// a real board change, not ω-cover.
    #[test]
    fn object_growth_r_a_shrink_rejects() {
        let mut prior = GameState::new_two_player(7);
        inert_token(&mut prior, 700, 0, "Saproling");
        inert_token(&mut prior, 701, 0, "Saproling");
        let mut current = prior.clone();
        // Remove 701 (shrink) and add 702 (growth).
        current.objects.remove(&ObjectId(701));
        current.battlefield.retain(|id| *id != ObjectId(701));
        inert_token(&mut current, 702, 0, "Saproling");
        assert!(
            !cover(&prior, &current),
            "a concurrent battlefield shrink must REJECT"
        );
    }

    /// R-a2 (REJECT): a NON-grown battlefield object drifts (tapped) while the board
    /// grows — `board_covers` non-grown content equality fails.
    #[test]
    fn object_growth_r_a2_nongrown_drift_rejects() {
        let (prior, mut current) = og_cover_base();
        current.objects.get_mut(&ObjectId(700)).unwrap().tapped = true;
        assert!(
            !cover(&prior, &current),
            "a non-grown object drifting (tapped) must REJECT"
        );
    }

    /// R-a3 (REJECT): an extra OFF-battlefield object exists only in current — the
    /// all-zones `objects_content_eq` len check fails.
    #[test]
    fn object_growth_r_a3_extra_offbattlefield_object_rejects() {
        let (prior, mut current) = og_cover_base();
        let oid = ObjectId(950);
        current.objects.insert(
            oid,
            GameObject::new(oid, CardId(950), PlayerId(0), "Extra".into(), Zone::Hand),
        );
        assert!(
            !cover(&prior, &current),
            "an extra non-battlefield object in current must REJECT"
        );
    }

    /// R-b (REJECT): a grown token is NOT churn-inert (carries a keyword). Passes
    /// `board_covers` (keywords are bucket-(ii), uncompared) then fails gate (2″).
    #[test]
    fn object_growth_r_b_grown_not_inert_keyword_rejects() {
        use crate::types::keywords::Keyword;
        let (prior, mut current) = og_cover_base();
        current.objects.get_mut(&ObjectId(702)).unwrap().keywords = vec![Keyword::Flying];
        assert!(
            !cover(&prior, &current),
            "a grown token with a keyword is not churn-inert ⇒ REJECT"
        );
    }

    /// ADV-3 (REQ-1 census-base END-TO-END, cover-level): a battlefield permanent
    /// present in BOTH frames carries an ability gated on a DELEGATING hole condition
    /// (`ControllerControlsMatching`) with a NON-`Typed` filter (`TargetFilter::Any`).
    /// The required-`ctx` census BASE vetoes for ANY filter shape ⇒ firewall fires ⇒
    /// cover FALSE. Pre-P3 this arm delegated to `scan_target_filter(Any)=NONE` and was
    /// MISSED (fail-OPEN false COVER); `census_hole_arms_are_load_bearing`
    /// (ability_scan.rs) proves the arm at the scan level, this proves it REACHES
    /// `cover` via firewall block-(2). Distinct from `gaeas_cradle_*` / `mana_board_*`
    /// (self-asserting aggregates, not delegating holes). Reach-guard: the no-observer
    /// control COVERS, so the observer condition is the sole rejector.
    #[test]
    fn object_growth_adv3_delegating_hole_reaches_firewall() {
        use crate::types::ability::{
            AbilityCondition, AbilityDefinition, AbilityKind, Effect, TargetFilter,
        };
        use std::sync::Arc;
        // Reach-guard: the SAME inert-token growth with NO observer COVERS.
        let (prior, current) = og_cover_base();
        assert!(cover(&prior, &current), "reach-guard: no observer ⇒ COVER");
        let (mut prior, mut current) = og_cover_base();
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::unimplemented("adv3", "gate"),
        )
        .condition(AbilityCondition::ControllerControlsMatching {
            filter: TargetFilter::Any,
        });
        for st in [&mut prior, &mut current] {
            let obs = inert_token(st, 600, 0, "Gate");
            st.objects.get_mut(&obs).unwrap().abilities = Arc::new(vec![def.clone()]);
        }
        assert!(
            !cover(&prior, &current),
            "REQ-1: a non-Typed delegating-hole census read vetoes the firewall (fail-closed)"
        );
    }

    /// ADV-5 (RELAXATION — the P3 canary mechanism, cover-level): a battlefield
    /// permanent present in BOTH frames carries a `SetTapState{Typed Creature, All}`
    /// effect BODY (Intruder Alarm's `untap all creatures` shape). Under the CR 732.2a
    /// `Typed`-precision firewall this body RELAXES (SnapshotOrEvent — the pinned
    /// inert-checkable exception) so pure inert-token growth COVERS ⇒ the detector can
    /// OFFER. Discriminating control: swapping the body for a CONSERVATIVE sibling
    /// reader (`Effect::Pump`) VETOES ⇒ cover FALSE. Reverting the `Typed` relaxation
    /// (Conservative `sibling:true` for the SetTapState target) flips the main
    /// assertion to FALSE.
    #[test]
    fn object_growth_adv5_relaxed_settap_body_covers() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, Effect, EffectScope, TapStateChange, TargetFilter,
            TypedFilter,
        };
        use std::sync::Arc;
        let settap = Effect::SetTapState {
            target: TargetFilter::Typed(TypedFilter::creature()),
            scope: EffectScope::All,
            state: TapStateChange::Untap,
        };
        let (mut prior, mut current) = og_cover_base();
        let def = AbilityDefinition::new(AbilityKind::Activated, settap);
        for st in [&mut prior, &mut current] {
            let obs = inert_token(st, 600, 0, "Alarm");
            st.objects.get_mut(&obs).unwrap().abilities = Arc::new(vec![def.clone()]);
        }
        assert!(
            cover(&prior, &current),
            "a relaxed SetTapState Typed body over inert growth ⇒ COVER (the canary mechanism)"
        );
        // Discriminating control: a CONSERVATIVE sibling body vetoes the SAME growth.
        let (mut prior, mut current) = og_cover_base();
        let pump = AbilityDefinition::new(AbilityKind::Activated, sibling_reading_effect());
        for st in [&mut prior, &mut current] {
            let obs = inert_token(st, 600, 0, "Alarm");
            st.objects.get_mut(&obs).unwrap().abilities = Arc::new(vec![pump.clone()]);
        }
        assert!(
            !cover(&prior, &current),
            "control: a CONSERVATIVE (Pump) body vetoes ⇒ the relaxation is load-bearing"
        );
    }

    /// ADV-6 (BLOCKER-1 fail-CLOSED non-vacuity, cover-level): a battlefield permanent
    /// present in BOTH frames carries an `EachSourceDealsDamage{sources:Typed Creature}`
    /// effect BODY whose `sources` cardinality DRIVES escalating player damage. Its
    /// effect-target ctx is the census DEFAULT (`EachSourceDealsDamage` ∉ the pinned
    /// `{SetTapState}` set) ⇒ `sources` reads the growing class ⇒ the firewall VETOES ⇒
    /// cover FALSE, even over otherwise-inert token growth. `recipient` is the read-free
    /// `EachController`, so `sources` is the SOLE census read. Discriminating control:
    /// the SAME shape with a RELAXED `SetTapState{Typed}` body COVERS ⇒ the census
    /// default for the damage aggregate is the sole rejector. The executed code
    /// revert-probe (reclassify EachSourceDealsDamage ⇒ SnapshotOrEvent) flips this to a
    /// WRONG COVER and turns `census_tag_set_is_exactly_enumerated` (guard#3) RED —
    /// EachSourceDealsDamage would drop from the enumerated 18-member census tag set.
    #[test]
    fn object_growth_adv6_each_source_damage_body_vetoes() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, EachDamageRecipient, Effect, EffectScope, QuantityExpr,
            TapStateChange, TargetFilter, TypedFilter,
        };
        use std::sync::Arc;
        let cannon = Effect::EachSourceDealsDamage {
            sources: TargetFilter::Typed(TypedFilter::creature()),
            amount: QuantityExpr::Fixed { value: 1 },
            recipient: EachDamageRecipient::EachController,
        };
        let (mut prior, mut current) = og_cover_base();
        let def = AbilityDefinition::new(AbilityKind::Activated, cannon);
        for st in [&mut prior, &mut current] {
            let obs = inert_token(st, 600, 0, "Cannon");
            st.objects.get_mut(&obs).unwrap().abilities = Arc::new(vec![def.clone()]);
        }
        assert!(
            !cover(&prior, &current),
            "EachSourceDealsDamage sources is the census default ⇒ firewall VETOES (BLOCKER-1)"
        );
        // Discriminating control: a RELAXED SetTapState body over the SAME growth COVERS.
        let settap = Effect::SetTapState {
            target: TargetFilter::Typed(TypedFilter::creature()),
            scope: EffectScope::All,
            state: TapStateChange::Untap,
        };
        let (mut prior, mut current) = og_cover_base();
        let def = AbilityDefinition::new(AbilityKind::Activated, settap);
        for st in [&mut prior, &mut current] {
            let obs = inert_token(st, 600, 0, "Cannon");
            st.objects.get_mut(&obs).unwrap().abilities = Arc::new(vec![def.clone()]);
        }
        assert!(
            cover(&prior, &current),
            "control: the RELAXED SetTapState body over the SAME growth COVERS"
        );
    }

    /// R-c (REJECT): a strict-compared GameState field (turn_number) drifts —
    /// `eq_except_growable` (reused `PartialEq`) fails.
    #[test]
    fn object_growth_r_c_gamestate_field_drift_rejects() {
        let (prior, mut current) = og_cover_base();
        current.turn_number += 1;
        assert!(
            !cover(&prior, &current),
            "a drifting non-object GameState field must REJECT"
        );
    }

    /// R-d (REJECT): the grown token is a NEW class with no inert member already in
    /// prior — a never-observed 0→1 introduction, not ω-growth of an existing class.
    #[test]
    fn object_growth_r_d_new_class_growth_rejects() {
        let (prior, mut current) = og_cover_base();
        // Grow a DIFFERENT class (no inert member of this class in prior). `name` is
        // layer-derived from `base_name`, so set BOTH so the rename survives flush.
        {
            let o = current.objects.get_mut(&ObjectId(702)).unwrap();
            o.name = "Beast".into();
            o.base_name = "Beast".into();
        }
        assert!(
            !cover(&prior, &current),
            "growth of a class not already present in prior must REJECT"
        );
    }

    /// R-e / R-e2 / R-e3 / R-e5 (REJECT) + R-e4 (COVER, Undaunted-safe): the
    /// cost-keyword family. Each board-scaling cost reducer rejects; Undaunted (reads
    /// the opponent count, CR 119, not a board object) covers. Revert-failing: each
    /// rejector flips to COVER if dropped from `keyword_cost_reads_growing_class`.
    #[test]
    fn object_growth_r_e_cost_keyword_family() {
        use crate::types::keywords::Keyword;
        let reject_cases = [
            ("Affinity", Keyword::Affinity(Default::default())),
            ("Improvise", Keyword::Improvise),
            ("Delve", Keyword::Delve),
            ("Emerge", Keyword::Emerge(Default::default())),
            // GAP-2: previously fail-OPEN under the old `matches!` classifier —
            // reverting FIX 2 (exhaustive match) flips each of these to COVER, so
            // each is a revert-failing discriminator for the exhaustive classifier.
            ("Offering", Keyword::Offering("Goblin".into())),
            ("Bargain", Keyword::Bargain),
            ("Assist", Keyword::Assist),
            // Tap-a-board-aggregate keywords (structurally identical to Convoke)
            // that the old 5-entry `matches!` also missed.
            (
                "Crew",
                Keyword::Crew {
                    power: 3,
                    once_per_turn: None,
                },
            ),
            ("Conspire", Keyword::Conspire),
        ];
        for (label, kw) in reject_cases {
            let (mut prior, mut current) = og_cover_base();
            hand_card_with_keywords(&mut prior, 900, vec![kw.clone()]);
            hand_card_with_keywords(&mut current, 900, vec![kw]);
            assert!(
                !cover(&prior, &current),
                "{label}: a board-scaling cost keyword must REJECT"
            );
        }
        // R-e4 Undaunted-safe COVER.
        let (mut prior, mut current) = og_cover_base();
        hand_card_with_keywords(&mut prior, 900, vec![Keyword::Undaunted]);
        hand_card_with_keywords(&mut current, 900, vec![Keyword::Undaunted]);
        assert!(
            cover(&prior, &current),
            "R-e4: Undaunted reads the opponent count, not |G| ⇒ COVER"
        );
    }

    /// Attach a bare `StaticDefinition` (empty `modifications`, `condition: None`) to
    /// a STABLE battlefield object in BOTH frames, then grow the board by one same-
    /// class token. The static object is non-grown, so gate (2″) inertness never sees
    /// it, and the empty modifications keep the §5.3a firewall gate (4) silent — the
    /// `StaticMode` cost scan (§5.4) is the SOLE differentiator between the REJECT
    /// mode and the COVER mode. Returns `cover(...)`.
    fn cover_with_static_on_stable(mode: StaticMode) -> bool {
        let mut prior = GameState::new_two_player(7);
        let sid = inert_token(&mut prior, 600, 0, "StaticSource");
        inert_token(&mut prior, 700, 0, "Saproling");
        inert_token(&mut prior, 701, 0, "Saproling");
        prior
            .objects
            .get_mut(&sid)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(mode));
        let mut current = prior.clone();
        inert_token(&mut current, 702, 0, "Saproling");
        cover(&prior, &current)
    }

    /// A `QuantityRef::ObjectCount` (reads the sibling/board axis ⇒ |G|).
    fn object_count_ref() -> QuantityRef {
        QuantityRef::ObjectCount {
            filter: TargetFilter::Any,
        }
    }

    /// R-e2 (GAP-1, REJECT + paired COVER): a `ModifyCost { mode: Raise,
    /// dynamic_count: Some(ObjectCount) }` static on a STABLE object over a growing
    /// board REJECTs (the false-positive-∞ direction — a per-cast tax that climbs as
    /// |G| grows). Non-vacuous: the SAME static with `dynamic_count: None` (a fixed
    /// `ManaCost` raise) COVERS, proving the `dynamic_count` scan — not the mere
    /// presence of a cost static — is the differentiator. Revert-failing: deleting
    /// the `def.mode` scan (or restoring the false "ModifyCost is fixed" comment's
    /// no-op) flips the REJECT case to a false-COVER.
    #[test]
    fn object_growth_r_e2_modifycost_dynamic_rejects() {
        use crate::types::mana::ManaCost;
        use crate::types::statics::CostModifyMode;
        let modify = |dynamic_count| StaticMode::ModifyCost {
            mode: CostModifyMode::Raise,
            amount: ManaCost::default(),
            spell_filter: None,
            dynamic_count,
        };
        assert!(
            !cover_with_static_on_stable(modify(Some(object_count_ref()))),
            "R-e2: ModifyCost.dynamic_count = ObjectCount(|G|) must REJECT"
        );
        assert!(
            cover_with_static_on_stable(modify(None)),
            "R-e2 control: a fixed (dynamic_count = None) ModifyCost must COVER"
        );
    }

    /// R-e2-impose (REJECT + paired COVER): an `ImposeAdditionalCost` whose
    /// `AbilityCost` reads `ObjectCount(|G|)` (a `PayLife` scaling with the board)
    /// REJECTs; the same static with a FIXED `PayLife` COVERS.
    #[test]
    fn object_growth_r_e2_impose_additional_cost_rejects() {
        use crate::types::ability::AbilityCost;
        use crate::types::statics::AdditionalCostTaxAction;
        let impose = |amount| StaticMode::ImposeAdditionalCost {
            cost: AbilityCost::PayLife { amount },
            spell_filter: None,
            action: AdditionalCostTaxAction::Cast,
        };
        assert!(
            !cover_with_static_on_stable(impose(QuantityExpr::Ref {
                qty: object_count_ref()
            })),
            "R-e2-impose: ImposeAdditionalCost reading ObjectCount(|G|) must REJECT"
        );
        assert!(
            cover_with_static_on_stable(impose(QuantityExpr::Fixed { value: 3 })),
            "R-e2-impose control: a fixed additional cost must COVER"
        );
    }

    /// R-e2-reduceability (REJECT + paired COVER): a `ReduceAbilityCost` whose
    /// `dynamic_count` reads `ObjectCount(|G|)` ("for each X you control") REJECTs;
    /// the same static with `dynamic_count: None` COVERS.
    #[test]
    fn object_growth_r_e2_reduce_ability_cost_rejects() {
        use crate::types::statics::CostModifyMode;
        let reduce = |dynamic_count| StaticMode::ReduceAbilityCost {
            mode: CostModifyMode::Reduce,
            keyword: "activated".to_string(),
            amount: 1,
            minimum_mana: None,
            dynamic_count,
            exemption: Default::default(),
            activator: None,
        };
        assert!(
            !cover_with_static_on_stable(reduce(Some(object_count_ref()))),
            "R-e2-reduceability: ReduceAbilityCost.dynamic_count = ObjectCount(|G|) must REJECT"
        );
        assert!(
            cover_with_static_on_stable(reduce(None)),
            "R-e2-reduceability control: a fixed ReduceAbilityCost must COVER"
        );
    }

    /// R-f (REJECT): a NON-grown battlefield permanent carries an ability whose
    /// effect reads the sibling (board-aggregate) axis — the §5.3a firewall (item 2)
    /// rejects even though the permanent is content-equal (abilities uncompared).
    #[test]
    fn object_growth_r_f_sibling_reading_ability_rejects() {
        use crate::types::ability::{AbilityDefinition, AbilityKind};
        use std::sync::Arc;
        let mut prior = GameState::new_two_player(7);
        let observer = inert_token(&mut prior, 600, 0, "Observer");
        let def = AbilityDefinition::new(AbilityKind::Activated, sibling_reading_effect());
        prior.objects.get_mut(&observer).unwrap().abilities = Arc::new(vec![def]);
        inert_token(&mut prior, 700, 0, "Saproling");
        let mut current = prior.clone();
        inert_token(&mut current, 702, 0, "Saproling");
        assert!(
            !cover(&prior, &current),
            "a live ability reading the growing class must REJECT (firewall item 2)"
        );
    }

    /// R-g (REJECT): a grown token carries an ACTIVATED ability (a churn lever the
    /// extrapolation cannot bound). Firewall-blind body (`Unimplemented` ⇒ NONE) so
    /// gate (2″) inertness — not the firewall — is the sole rejector.
    #[test]
    fn object_growth_r_g_grown_activated_ability_rejects() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};
        use std::sync::Arc;
        let (prior, mut current) = og_cover_base();
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::unimplemented("r-g", "activated"),
        );
        current.objects.get_mut(&ObjectId(702)).unwrap().abilities = Arc::new(vec![def]);
        assert!(
            !cover(&prior, &current),
            "a grown token with an activated ability is not churn-inert ⇒ REJECT"
        );
    }

    // ---- P2 (CR 732.2a): the firewall DESCENDS Token/Mana bodies (LoopFirewall) ----

    /// P2-9 (firewall): Gaea's Cradle's `{T}: Add {G} for each creature you control`
    /// on a functioning battlefield permanent. The S5 ability-body scan (firewall
    /// item 2) runs `LoopFirewall`, descends `Effect::Mana`, and vetoes via the
    /// COUNT path (`AnyOneColor.count` → `scan_quantity_ref::ObjectCount`). That the
    /// firewall flips to false when the count is dropped (revert-probe: bind
    /// `AnyOneColor.count` to `_` in `scan_mana_production`) proves the descent is
    /// `LoopFirewall`, not the fail-closed `Conservative` blanket.
    #[test]
    fn gaeas_cradle_firewall_vetoes_via_count_path() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction, QuantityExpr,
        };
        use crate::types::mana::ManaColor;
        use std::sync::Arc;
        let mut state = GameState::new_two_player(7);
        let land = inert_token(&mut state, 800, 0, "Gaea's Cradle");
        let mana = Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Ref {
                    qty: object_count_ref(),
                },
                color_options: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        };
        state.objects.get_mut(&land).unwrap().abilities =
            Arc::new(vec![AbilityDefinition::new(AbilityKind::Activated, mana)]);
        assert!(
            fire_time_conditions_read_growing_class(&state, None),
            "Gaea's Cradle mana ability reads |G| via its count (S5 LoopFirewall descent)"
        );
    }

    /// P2-7 (firewall): a board-color mana aggregate (`DistinctColorsAmongPermanents`)
    /// with a NON-`Typed` filter still vetoes — the arm self-asserts its own
    /// `sibling` (the signal cannot come from the `Typed` arm). Revert-probe: strip
    /// the arm's own `sibling:true` literal in `scan_mana_production` ⇒ firewall false.
    #[test]
    fn mana_board_aggregate_firewall_vetoes() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, ManaProduction};
        use std::sync::Arc;
        let mut state = GameState::new_two_player(7);
        let src = inert_token(&mut state, 810, 0, "Faeburrow Elder");
        let mana = Effect::Mana {
            produced: ManaProduction::DistinctColorsAmongPermanents {
                filter: TargetFilter::Controller,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        };
        state.objects.get_mut(&src).unwrap().abilities =
            Arc::new(vec![AbilityDefinition::new(AbilityKind::Activated, mana)]);
        assert!(
            fire_time_conditions_read_growing_class(&state, None),
            "a board-color mana aggregate self-asserts sibling ⇒ firewall vetoes"
        );
    }

    /// P2-10 (M9, U3): a projected-reading modification (`SetDynamicPower{Ref(LifeTotal)}`)
    /// on a live static VETOES the firewall via the `:1539` descent's PROJECTED axis
    /// — the projected-resource firewall has NO modification scan, so this descent is
    /// the sole guard. AXIS ISOLATION: the modification reads projected, NOT sibling.
    /// Revert-probe: drop `|| continuous_modification_reads_projected_resource(m)`
    /// from the `:1539` descent ⇒ firewall false.
    #[test]
    fn projected_reading_modification_still_vetoes_the_firewall() {
        use crate::game::ability_scan::{
            continuous_modification_reads_projected_resource,
            continuous_modification_reads_sibling_mutable,
        };
        use crate::types::ability::{ContinuousModification, PlayerScope, QuantityExpr};
        let m = ContinuousModification::SetDynamicPower {
            value: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Controller,
                },
            },
        };
        // AXIS ISOLATION (scanner level): projected, not sibling.
        assert!(
            !continuous_modification_reads_sibling_mutable(&m),
            "a LifeTotal read is projected, not sibling"
        );
        assert!(
            continuous_modification_reads_projected_resource(&m),
            "a LifeTotal read is projected"
        );
        // FIREWALL level: the :1539 descent's projected axis vetoes.
        let mut state = GameState::new_two_player(7);
        let src = inert_token(&mut state, 820, 0, "AnthemSource");
        state
            .objects
            .get_mut(&src)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::continuous().modifications(vec![m]));
        assert!(
            fire_time_conditions_read_growing_class(&state, None),
            "a projected-reading modification vetoes via the :1539 projected axis (M9)"
        );
    }

    /// FIREWALL block(1) matched pair (CR 603.6a): the ETB-observer gate skips ONLY a
    /// PROVABLY-disjoint observer, and only when a fodder-class representative is supplied.
    ///
    /// Non-vacuity / reach-guard: case (c) (`None`) proves the observer's sibling-reading execute
    /// body alone trips the block(1) execute scan — so case (a)'s `false` is the GATE skipping the
    /// observer, not a body that never vetoes. It also pins the object-growth (`None`) path
    /// byte-identical. Revert-probe: hardcoding `etb_observer_provably_excludes_class` to `false`
    /// (or deleting its body) flips (a) `false → true`; breaking `valid_card_matches` to always
    /// `false` flips (b) `true → false`.
    #[test]
    fn etb_observer_gate_skips_only_provably_disjoint_observer() {
        use crate::types::ability::{AbilityDefinition, AbilityKind};

        // The P0 fodder Saproling creature-token id (the growing-class representative).
        let member = ObjectId(900);
        // Minimal state: a P1 ETB observer carrying `valid_card` + a firewall-flagged
        // (sibling-reading) execute body, watching the battlefield, plus the P0 fodder member.
        let build = |valid_card: TargetFilter| {
            let mut state = GameState::new_two_player(7);
            let m = inert_token(&mut state, 900, 0, "Saproling");
            {
                let o = state.objects.get_mut(&m).unwrap();
                o.card_types.core_types = vec![CoreType::Creature];
                o.card_types.subtypes = vec!["Saproling".to_string()];
                o.is_token = true;
            }
            let observer = inert_token(&mut state, 910, 1, "Eminence Observer");
            let trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(valid_card)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    sibling_reading_effect(),
                ));
            state
                .objects
                .get_mut(&observer)
                .unwrap()
                .trigger_definitions
                .push(trig);
            state
        };

        // "another nontoken Wizard you control" — triple-disjoint from the P0 Saproling token
        // (subtype, controller You=P1, NonToken). Mirrors Inalla's Eminence matcher.
        let disjoint = TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Wizard".to_string())
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::NonToken, FilterProp::Another]),
        );
        // A broad "whenever a creature enters" matcher that DOES match the P0 Saproling.
        let broad = TargetFilter::Typed(TypedFilter::creature());

        // (c) REACH-GUARD (`None` ⇒ no class context): the disjoint observer's body vetoes,
        // proving it reaches the block(1) execute scan; also pins the object-growth path.
        assert!(
            fire_time_conditions_read_growing_class(&build(disjoint.clone()), None),
            "None class context: even a disjoint ETB observer keeps the conservative veto"
        );
        // (a) DISJOINT + `Some(class)`: the gate skips the observer ⇒ NOT vetoed.
        assert!(
            !fire_time_conditions_read_growing_class(
                &build(disjoint),
                Some(&HashSet::from([member]))
            ),
            "a provably-disjoint ETB observer is skipped when the proven class is supplied"
        );
        // (b) MATCHING (broad matcher matches the fodder) + `Some(class)`: still vetoed — the
        // gate only skips PROVABLY-disjoint observers.
        assert!(
            fire_time_conditions_read_growing_class(&build(broad), Some(&HashSet::from([member]))),
            "a broad ETB observer whose matcher matches the fodder still vetoes"
        );
    }

    /// R-s5-abilitykind (REJECT): a NON-`Activated` ability (kind `Spell`) whose body
    /// reads the sibling axis, on a non-grown permanent. Firewall item (2) scans
    /// EVERY kind (S5) — revert to a `kind == Activated` narrowing and this is missed
    /// (false COVER).
    #[test]
    fn object_growth_r_s5_non_activated_ability_kind_rejects() {
        use crate::types::ability::{AbilityDefinition, AbilityKind};
        use std::sync::Arc;
        let mut prior = GameState::new_two_player(7);
        let observer = inert_token(&mut prior, 600, 0, "Observer");
        let def = AbilityDefinition::new(AbilityKind::Spell, sibling_reading_effect());
        prior.objects.get_mut(&observer).unwrap().abilities = Arc::new(vec![def]);
        inert_token(&mut prior, 700, 0, "Saproling");
        let mut current = prior.clone();
        inert_token(&mut current, 702, 0, "Saproling");
        assert!(
            !cover(&prior, &current),
            "S5: a non-Activated sibling-reading ability must REJECT (scanned regardless of kind)"
        );
    }

    /// ITEM A — a FOREIGN, NON-`Activated` sibling-reading def is NOT relieved by
    /// `sole_driver`. CR 117.1b licenses relief only for ACTIVATED abilities ("a player
    /// may activate an activated ability any time they have priority"); a `Spell`-kind
    /// def is not reached through the priority rule at all, so a priority-based rationale
    /// can say nothing about it.
    ///
    /// The subject and the MATCHED POSITIVE CONTROL come from ONE builder, so the only
    /// variable between them is `kind` — which is what makes the subject's veto
    /// attributable to `kind` rather than to some other surface on the board.
    ///
    /// REVERT-PROBE: delete `ability.kind == AbilityKind::Activated &&` from block (2)'s
    /// `relieved` closure ⇒ the subject is relieved too ⇒ the subject assertion FAILS,
    /// deterministically.
    #[test]
    fn foreign_non_activated_ability_is_not_relieved_by_sole_driver() {
        use crate::game::ability_scan as scan;
        use crate::types::ability::{AbilityDefinition, AbilityKind};
        use std::sync::Arc;

        // ONE builder ⇒ subject and control are byte-identical except `kind`.
        let build = |kind: AbilityKind| {
            let mut state = GameState::new_two_player(7);
            let observer = inert_token(&mut state, 950, 1, "Foreign Observer");
            let def = AbilityDefinition::new(kind, sibling_reading_effect());
            state.objects.get_mut(&observer).unwrap().abilities = Arc::new(vec![def]);
            (state, observer)
        };
        // `LoopWindowScope` derives `Copy`, so one binding serves both calls.
        let driver_scope = LoopWindowScope {
            phase_invariant: None,
            sole_driver: Some(PlayerId(0)),
            pinned: None,
            cast_card_ids: None,
            period: None,
        };

        let (subject, observer) = build(AbilityKind::Spell);
        // ---- REACH-GUARDS: all of them, before any outcome assertion ----
        {
            let obj = &subject.objects[&observer];
            assert_eq!(obj.abilities.len(), 1);
            assert_eq!(obj.abilities[0].kind, AbilityKind::Spell);
            assert!(
                scan::ability_definition_reads_sibling_mutable_for_loop(&obj.abilities[0]),
                "reach-guard: the scan must SEE the sibling axis, else the row proves nothing \
                 (subsumes the `Effect::Unimplemented => Axes::NONE` vacuity)"
            );
            assert!(
                !crate::game::mana_abilities::is_mana_ability(&obj.abilities[0]),
                "reach-guard: CR 605.3a is NOT what carries this row's verdict"
            );
            assert_eq!(obj.zone, Zone::Battlefield);
            assert!(!obj.is_phased_out());
            assert!(
                obj.trigger_definitions.is_empty(),
                "reach-guard: block (1) must be silent, so the verdict is attributable to block (2)"
            );
            assert_ne!(
                obj.controller,
                PlayerId(0),
                "reach-guard: the observer really is FOREIGN"
            );
        }
        // ---- SUBJECT ----
        assert!(
            fire_time_conditions_read_growing_class_scoped(&subject, None, driver_scope),
            "CR 117.1b licenses relief only for ACTIVATED abilities; a Spell-kind def is not \
             reached through the priority rule at all"
        );
        // ---- MATCHED POSITIVE CONTROL: the ONLY variable is `kind` ----
        let (control, _) = build(AbilityKind::Activated);
        assert!(
            !fire_time_conditions_read_growing_class_scoped(&control, None, driver_scope),
            "control: the identical def at kind=Activated IS relieved — so the subject's veto is \
             attributable to `kind` and not to some unrelated surface on this board"
        );
    }

    /// ITEM E — a FOREIGN `Activated` def carrying an `activator_filter` is NOT relieved.
    /// CR 602.2: "Only an object's controller (or its owner, if it doesn't have a
    /// controller) can activate its activated ability UNLESS THE OBJECT SPECIFICALLY SAYS
    /// OTHERWISE." `activator_filter` is that "otherwise", so `obj.controller != driver`
    /// does not imply the sole driver cannot activate it inside the window.
    ///
    /// The guard fails closed on ANY `Some(..)` rather than on an enumeration of the
    /// widening variants, so this row's subject uses one representative (`All`) and the
    /// claim under test is the `is_none()` predicate, not that variant.
    ///
    /// REVERT-PROBE: delete `&& ability.activator_filter.is_none()` ⇒ the subject is
    /// relieved ⇒ the subject assertion FAILS.
    #[test]
    fn foreign_activator_filter_ability_is_not_relieved_by_sole_driver() {
        use crate::game::ability_scan as scan;
        use crate::types::ability::{AbilityDefinition, AbilityKind, PlayerFilter};
        use std::sync::Arc;

        let build = |activator_filter: Option<PlayerFilter>| {
            let mut state = GameState::new_two_player(7);
            let observer = inert_token(&mut state, 951, 1, "Foreign Widened Observer");
            let mut def = AbilityDefinition::new(AbilityKind::Activated, sibling_reading_effect());
            def.activator_filter = activator_filter; // `pub` field on `AbilityDefinition`
            state.objects.get_mut(&observer).unwrap().abilities = Arc::new(vec![def]);
            (state, observer)
        };
        let driver_scope = LoopWindowScope {
            phase_invariant: None,
            sole_driver: Some(PlayerId(0)),
            pinned: None,
            cast_card_ids: None,
            period: None,
        };

        let (subject, observer) = build(Some(PlayerFilter::All));
        {
            let obj = &subject.objects[&observer];
            assert_eq!(obj.abilities.len(), 1);
            assert_eq!(obj.abilities[0].kind, AbilityKind::Activated);
            assert!(
                obj.abilities[0].activator_filter.is_some(),
                "reach-guard: the subject must actually carry the widening field"
            );
            assert!(
                scan::ability_definition_reads_sibling_mutable_for_loop(&obj.abilities[0]),
                "reach-guard: the scan must SEE the sibling axis, else the row proves nothing"
            );
            assert!(
                !crate::game::mana_abilities::is_mana_ability(&obj.abilities[0]),
                "reach-guard: CR 605.3a is NOT what carries this row's verdict"
            );
            assert_eq!(obj.zone, Zone::Battlefield);
            assert!(!obj.is_phased_out());
            assert!(
                obj.trigger_definitions.is_empty(),
                "reach-guard: block (1) must be silent, so the verdict is attributable to block (2)"
            );
            assert_ne!(
                obj.controller,
                PlayerId(0),
                "reach-guard: the observer really is FOREIGN"
            );
        }
        assert!(
            fire_time_conditions_read_growing_class_scoped(&subject, None, driver_scope),
            "CR 602.2: an `activator_filter` is the object saying otherwise, so the sole \
             driver MAY activate this foreign ability inside the window"
        );
        let (control, _) = build(None);
        assert!(
            !fire_time_conditions_read_growing_class_scoped(&control, None, driver_scope),
            "control: the identical def with `activator_filter: None` IS relieved — so the \
             subject's veto is attributable to that field alone"
        );
    }

    /// R-s4-objfield (two-sided): a non-grown object's §5.2c ADD field (`intensity`)
    /// accumulates while the board grows ⇒ REJECT; held constant ⇒ COVER.
    /// Revert-failing: dropping `intensity` from `object_content_eq` flips the REJECT
    /// arm to COVER.
    #[test]
    fn object_growth_r_s4_objfield_intensity_two_sided() {
        // 700 = plain inert token (the grown 702's confine class); 701 = the stable
        // carrier whose `intensity` is the accumulator under test.
        let (mut prior, mut current) = og_cover_base();
        let carrier = ObjectId(701);
        prior.objects.get_mut(&carrier).unwrap().intensity = 1;
        current.objects.get_mut(&carrier).unwrap().intensity = 1;

        // Control (COVER): intensity equal on both frames.
        assert!(
            cover(&prior, &current),
            "control: constant intensity ⇒ growth COVERS"
        );
        // Reject: intensity accumulates on the stable carrier.
        current.objects.get_mut(&carrier).unwrap().intensity = 2;
        assert!(
            !cover(&prior, &current),
            "a per-iteration intensity delta on a stable object must REJECT"
        );
    }

    /// R-s4-chosen (two-sided, S6, firewall-blind reach-guard): a non-grown object's
    /// `chosen_attributes` accumulates ⇒ REJECT; held constant ⇒ COVER. The carrier
    /// ALSO holds a `RememberCard{SelfRef}` ability — `resolved_ability_axes` = NONE
    /// (firewall-blind), so the COVER control proves the firewall does NOT catch it
    /// and ONLY `object_content_eq` (the §5.2c `chosen_attributes` ADD) does.
    /// Revert-failing: dropping `chosen_attributes` from `object_content_eq` flips
    /// the REJECT arm to COVER.
    #[test]
    fn object_growth_r_s4_chosen_attributes_two_sided() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ChosenAttribute, Effect, TargetFilter,
        };
        use std::sync::Arc;

        // 700 = plain inert token (the grown 702's confine class); 701 = the stable
        // carrier bearing the firewall-blind writer + the `chosen_attributes` accumulator.
        let (mut prior, _c) = og_cover_base();
        let carrier = ObjectId(701);
        // Firewall-blind writer: RememberCard{SelfRef} ⇒ sibling axis NONE. Set in
        // BOTH `abilities` and `base_abilities` so it survives the layer flush and is
        // actually scanned (and passed over) by the firewall.
        let remember = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::RememberCard {
                target: TargetFilter::SelfRef,
            },
        );
        {
            let o = prior.objects.get_mut(&carrier).unwrap();
            o.abilities = Arc::new(vec![remember.clone()]);
            o.base_abilities = Arc::new(vec![remember]);
            o.chosen_attributes = vec![ChosenAttribute::Number(1)];
        }
        // Clone AFTER carrier setup so current's 701 matches prior's; then grow.
        let mut current = prior.clone();
        inert_token(&mut current, 702, 0, "Saproling");

        // Control (COVER): the firewall-blind RememberCard ability does NOT reject,
        // and chosen_attributes is constant ⇒ growth covers.
        assert!(
            cover(&prior, &current),
            "control: firewall-blind RememberCard + constant chosen_attributes ⇒ COVER"
        );
        // Reject: chosen_attributes accumulates on the stable carrier — caught ONLY by
        // object_content_eq (the firewall is provably blind, per the control).
        current.objects.get_mut(&carrier).unwrap().chosen_attributes =
            vec![ChosenAttribute::Number(1), ChosenAttribute::Number(2)];
        assert!(
            !cover(&prior, &current),
            "a per-iteration chosen_attributes delta must REJECT (object_content_eq, not the firewall)"
        );
    }

    /// R-s3-accum + R-s3-sync (the mutate-each-field sync test): each strict-compared
    /// GameState field that survives projection, mutated one at a time on a covering
    /// base, must REJECT via `eq_except_growable`. Proves the reused `PartialEq`
    /// (guarded total by `_gamestate_partition_is_total`) catches every one.
    #[test]
    fn object_growth_r_s3_gamestate_accumulator_sync() {
        // R-s3-accum: a per-turn accumulator PartialEq compares.
        let (prior, mut current) = og_cover_base();
        current.lands_played_this_turn += 1;
        assert!(
            !cover(&prior, &current),
            "R-s3-accum: a hidden per-turn accumulator delta must REJECT"
        );

        // R-s3-sync: sweep several strict-compared fields, each independently. Each
        // mutation on the covering base must independently flip the verdict to REJECT.
        let sync = |mutate: &dyn Fn(&mut GameState), label: &str| {
            let (prior, mut current) = og_cover_base();
            mutate(&mut current);
            assert!(
                !cover(&prior, &current),
                "R-s3-sync: a delta in `{label}` must REJECT (eq_except_growable)"
            );
        };
        sync(&|s| s.turn_number += 1, "turn_number");
        sync(&|s| s.active_player = PlayerId(1), "active_player");
        sync(&|s| s.priority_player = PlayerId(1), "priority_player");
        sync(&|s| s.lands_played_this_turn += 1, "lands_played_this_turn");
    }

    // =======================================================================
    // PR-7 Phase 4d-i — offline FODDER-GROWTH cover predicate
    // (`loop_states_cover_modulo_fodder_growth`) + the tapped-split multiset.
    // Synthetic frame-pairs assert the bool. Non-vacuous: each REJECT names a
    // paired positive reach-guard and fails (returns COVER) if its named
    // authority is reverted.
    // =======================================================================

    /// A TAPPED inert battlefield token of class `name` (fodder that has already been
    /// tapped to a convoke/affinity cost). Otherwise identical to `inert_token`.
    fn tapped_inert_token(state: &mut GameState, id: u64, controller: u8, name: &str) -> ObjectId {
        let oid = inert_token(state, id, controller, name);
        state.objects.get_mut(&oid).unwrap().tapped = true;
        oid
    }

    /// F2: the fodder-class representative, constructed IDENTICALLY to the fodder
    /// tokens (bare `GameObject::new` ⇒ `power = None`, no counters, untapped). If it
    /// carried a synthetic P/T it would mis-partition as stable-engine and the
    /// positive cover would wrongly reject. `object_content_eq` ignores `id`, so the
    /// id here is irrelevant.
    fn saproling_class() -> GameObject {
        GameObject::new(
            ObjectId(999),
            CardId(999),
            PlayerId(0),
            "Saproling".into(),
            Zone::Battlefield,
        )
    }

    fn fodder_cover(prior: &GameState, current: &GameState) -> bool {
        loop_states_cover_modulo_fodder_growth(prior, current, &saproling_class())
    }

    /// F+ base: an inert engine (800) + 4 untapped + 1 tapped Saproling (prior);
    /// current taps one untapped (700) and reproduces one untapped (705). Fodder
    /// split moves untapped 4→4, tapped 1→2, total 5→6 — a valid tapped-split cover.
    fn fodder_cover_base() -> (GameState, GameState) {
        let mut prior = GameState::new_two_player(7);
        inert_token(&mut prior, 800, 0, "Engine");
        inert_token(&mut prior, 700, 0, "Saproling");
        inert_token(&mut prior, 701, 0, "Saproling");
        inert_token(&mut prior, 702, 0, "Saproling");
        inert_token(&mut prior, 703, 0, "Saproling");
        tapped_inert_token(&mut prior, 704, 0, "Saproling");
        let mut current = prior.clone();
        current.objects.get_mut(&ObjectId(700)).unwrap().tapped = true;
        inert_token(&mut current, 705, 0, "Saproling");
        (prior, current)
    }

    /// F+ COVER (tapped-split, NO cost keyword). Revert-failing: swapping
    /// `fodder_cover` to `loop_states_cover_modulo_object_growth` (absolute-ObjectId)
    /// rejects — 700's untapped→tapped drift fails `board_covers`' non-grown eq.
    #[test]
    fn fodder_cover_tapped_split_covers() {
        let (prior, current) = fodder_cover_base();
        assert!(
            fodder_cover(&prior, &current),
            "tapped-split fodder growth (untapped 4→4, total 5→6) must COVER"
        );
        // Control: the object-growth predicate REJECTS the same frames (proves the
        // tapped-tolerant multiset is the load-bearing difference, not some other gate).
        assert!(
            !loop_states_cover_modulo_object_growth(&prior, &current),
            "the absolute-ObjectId object-growth predicate must reject the tap drift"
        );
    }

    /// F-B1 (untapped ↓): total STILL grows (5→6) but untapped DROPS (4→3) — a
    /// draining loop. First branch: `board_covers_modulo_fodder` B1. Revert-failing:
    /// dropping the `current_untapped >= prior_untapped` guard (leaving only strict
    /// total growth) covers this draining loop.
    #[test]
    fn fodder_reject_untapped_decrease() {
        let mut prior = GameState::new_two_player(7);
        inert_token(&mut prior, 800, 0, "Engine");
        inert_token(&mut prior, 700, 0, "Saproling");
        inert_token(&mut prior, 701, 0, "Saproling");
        inert_token(&mut prior, 702, 0, "Saproling");
        inert_token(&mut prior, 703, 0, "Saproling");
        tapped_inert_token(&mut prior, 704, 0, "Saproling"); // untapped 4, tapped 1, total 5
        let mut current = prior.clone();
        current.objects.get_mut(&ObjectId(700)).unwrap().tapped = true; // tap one untapped
        tapped_inert_token(&mut current, 705, 0, "Saproling"); // reproduce TAPPED only
                                                               // untapped 3, tapped 3, total 6: total grows, untapped drains.
        assert!(
            !fodder_cover(&prior, &current),
            "a draining loop (untapped 4→3) must REJECT even though total grows (B1)"
        );
        // Reach-guard: untapped-preserving growth on an equivalent base COVERS.
        let (p, c) = fodder_cover_base();
        assert!(
            fodder_cover(&p, &c),
            "reach-guard: untapped-preserving fodder growth COVERS"
        );
    }

    /// F-stable (engine drift): tap the stable ENGINE object (800, non-fodder) in
    /// current. First branch: `board_covers_modulo_fodder`'s stable-partition
    /// `objects_content_eq`. Revert-failing: dropping that stable check flips this to
    /// COVER — nothing else sees the engine's tap state (`eq_except_growable` reuses
    /// `GameState::PartialEq`, which compares only `objects.len()`, unchanged here).
    #[test]
    fn fodder_reject_stable_engine_drift() {
        let (prior, mut current) = fodder_cover_base();
        current.objects.get_mut(&ObjectId(800)).unwrap().tapped = true;
        assert!(
            !fodder_cover(&prior, &current),
            "a stable-engine (non-fodder) drift must REJECT (stable objects_content_eq)"
        );
        // Reach-guard: without the engine drift, the same growth COVERS.
        let (p, c) = fodder_cover_base();
        assert!(fodder_cover(&p, &c), "reach-guard: no engine drift ⇒ COVER");
    }

    /// F-B7 (grown carries ability): the reproduced token (705) has a keyword, so it
    /// is fodder-by-content (keywords are not compared by `object_content_eq`) but not
    /// churn-inert. First branch: `grown_objects_are_inert`. Revert-failing: dropping
    /// that conjunct covers non-inert growth.
    #[test]
    fn fodder_reject_grown_not_inert() {
        use crate::types::keywords::Keyword;
        let (prior, mut current) = fodder_cover_base();
        current.objects.get_mut(&ObjectId(705)).unwrap().keywords = vec![Keyword::Flying];
        assert!(
            !fodder_cover(&prior, &current),
            "a non-inert grown fodder member must REJECT (grown_objects_are_inert)"
        );
        // Reach-guard: an inert reproduced token COVERS.
        let (p, c) = fodder_cover_base();
        assert!(
            fodder_cover(&p, &c),
            "reach-guard: inert fodder growth ⇒ COVER"
        );
    }

    // =======================================================================
    // PR-7 Phase 4d-i — BLOCKER-2 structural driving-resource sign-check
    // (`driving_resources_non_decreasing`). Two RAW (un-projected) synthetic
    // GameStates; controller = P0. Each REJECT names its branch; each sibling
    // pass proves the veto is not over-broad.
    // =======================================================================

    fn sign_check(prior: &GameState, current: &GameState) -> bool {
        driving_resources_non_decreasing(prior, current, PlayerId(0))
    }

    /// S+ (positive reach-guard for every S- below): no consumable decreases.
    #[test]
    fn sign_check_all_non_decreasing_passes() {
        let mut prior = GameState::new_two_player(7);
        prior.players[0].energy = 3;
        let current = prior.clone();
        assert!(
            sign_check(&prior, &current),
            "no consumable decrease (energy 3→3, all else equal) ⇒ pass"
        );
    }

    /// S-energy ↓. First branch: (a) scalar zip. Revert-failing: deleting the scalar
    /// veto covers an energy-consuming recast loop.
    #[test]
    fn sign_check_energy_decrease_rejects() {
        let mut prior = GameState::new_two_player(7);
        prior.players[0].energy = 3;
        let mut current = prior.clone();
        current.players[0].energy = 2;
        assert!(
            !sign_check(&prior, &current),
            "energy 3→2 must REJECT (branch a scalar zip)"
        );
    }

    /// S-poison ↓. First branch: (a) scalar zip.
    #[test]
    fn sign_check_poison_decrease_rejects() {
        let mut prior = GameState::new_two_player(7);
        prior.players[0].poison_counters = 2;
        let mut current = prior.clone();
        current.players[0].poison_counters = 1;
        assert!(
            !sign_check(&prior, &current),
            "poison 2→1 must REJECT (branch a scalar zip)"
        );
    }

    /// S-playercounter ↓ (per-kind) — the structural-vs-hand-list discriminator.
    /// First branch: (b) per-kind player_counters union. Revert-failing: an
    /// energy-only / scalar-only fix leaves `player_counters` unchecked ⇒ covers.
    #[test]
    fn sign_check_player_counter_decrease_rejects() {
        use crate::types::player::PlayerCounterKind;
        let mut prior = GameState::new_two_player(7);
        prior.players[0]
            .player_counters
            .insert(PlayerCounterKind::Experience, 2);
        let mut current = prior.clone();
        current.players[0]
            .player_counters
            .insert(PlayerCounterKind::Experience, 1);
        assert!(
            !sign_check(&prior, &current),
            "experience counter 2→1 must REJECT (branch b per-kind)"
        );
    }

    /// S-objectcounter ↓ (per-kind, controller). First branch: (c) per-kind object
    /// totals. Revert-failing: deleting branch (c) covers a +1/+1-consuming loop.
    #[test]
    fn sign_check_object_counter_decrease_rejects() {
        let mut prior = GameState::new_two_player(7);
        let oid = inert_token(&mut prior, 500, 0, "Bear");
        prior
            .objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);
        let mut current = prior.clone();
        current
            .objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        assert!(
            !sign_check(&prior, &current),
            "a controller +1/+1 counter 2→1 must REJECT (branch c per-kind object total)"
        );
    }

    /// S monotone-history OK (sibling): `life_gained_this_turn` 0→2 must PASS. Proves
    /// the blanket veto DIRECTION (`cur < pri`, not `cur > pri`) — a mis-signed veto
    /// would false-reject the fodder class.
    #[test]
    fn sign_check_monotone_history_increase_passes() {
        let mut prior = GameState::new_two_player(7);
        prior.players[0].life_gained_this_turn = 0;
        let mut current = prior.clone();
        current.players[0].life_gained_this_turn = 2;
        assert!(
            sign_check(&prior, &current),
            "life_gained_this_turn 0→2 (monotone up) must PASS (blanket ≥ veto direction)"
        );
    }

    /// S damage_marked NOT vetoed (sibling): a controller permanent heals 2→0. Proves
    /// `damage_marked` is excluded from the monotone object-counter veto (a decrease
    /// is a beneficial heal, not a resource depletion).
    #[test]
    fn sign_check_damage_marked_heal_not_vetoed() {
        let mut prior = GameState::new_two_player(7);
        let oid = inert_token(&mut prior, 500, 0, "Bear");
        prior.objects.get_mut(&oid).unwrap().damage_marked = 2;
        let mut current = prior.clone();
        current.objects.get_mut(&oid).unwrap().damage_marked = 0;
        assert!(
            sign_check(&prior, &current),
            "damage_marked 2→0 (heal) must NOT be vetoed (not a monotone counter)"
        );
    }

    /// S object-counter on OPPONENT ↓ (sibling): P1 permanent loses a +1/+1 while
    /// controller is P0. Proves branch (c)'s `o.controller != controller` scoping —
    /// an opponent's depletion is not the controller's resource.
    #[test]
    fn sign_check_opponent_object_counter_decrease_not_vetoed() {
        let mut prior = GameState::new_two_player(7);
        let oid = inert_token(&mut prior, 500, 1, "Bear"); // controller 1 = opponent
        prior
            .objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);
        let mut current = prior.clone();
        current
            .objects
            .get_mut(&oid)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        assert!(
            sign_check(&prior, &current),
            "an OPPONENT's +1/+1 2→1 must NOT be vetoed (controller-scoped)"
        );
    }

    /// `_projected_player_axes_is_total` (compiler-total guard): `Player::default()`
    /// has empty `player_counters` ⇒ 6 scalar axes. Breaks if a projected scalar is
    /// added to `project_out_player_consumables` without a matching `vec![]` entry.
    /// Mirror of `_gamestate_partition_is_total`'s convention.
    #[test]
    fn _projected_player_axes_is_total() {
        assert_eq!(projected_player_axes(&Player::default()).len(), 6);
    }

    /// carry a (`_projected_player_maps_is_total`, compiler-total guard): `Player::default()`
    /// has exactly ONE map-typed projected consumable (`player_counters`). Breaks the build if
    /// a second projected map consumable is added to `project_out_player_consumables` without a
    /// matching `projected_player_maps` entry — the structural tie that keeps
    /// `driving_resources_non_decreasing`'s per-kind map veto (branch b) from silently missing
    /// it. Mirror of `_projected_player_axes_is_total`.
    #[test]
    fn _projected_player_maps_is_total() {
        assert_eq!(projected_player_maps(&Player::default()).len(), 1);
    }

    /// carry b (CR 704.5g damage_marked-INCREASE veto). A controller-side marked-damage
    /// INCREASE (2→3 on the controller's own permanent) REJECTS — a self-terminating loop.
    /// First branch: `driving_resources_non_decreasing` branch (d). Revert-failing: deleting
    /// branch (d) flips this to pass (a lethal-accruing board-growth loop would offer).
    #[test]
    fn sign_check_damage_marked_increase_rejects() {
        let mut prior = GameState::new_two_player(7);
        let oid = inert_token(&mut prior, 600, 0, "Engine"); // controller 0
        prior.objects.get_mut(&oid).unwrap().damage_marked = 2;
        let mut current = prior.clone();
        current.objects.get_mut(&oid).unwrap().damage_marked = 3;
        assert!(
            !sign_check(&prior, &current),
            "a controller-side damage_marked INCREASE (2→3) must REJECT (CR 704.5g, branch d)"
        );
        // Reach-guard + orthogonality with 4d-i's `sign_check_damage_marked_heal_not_vetoed`:
        // a DECREASE (heal) still PASSES — the increase-veto is the opposite polarity.
        let mut healed = prior.clone();
        healed.objects.get_mut(&oid).unwrap().damage_marked = 0;
        assert!(
            sign_check(&prior, &healed),
            "reach-guard: a damage_marked DECREASE (2→0 heal) must still PASS"
        );
    }

    /// carry b controller-scoping: an OPPONENT's damage_marked increase is NOT vetoed (the
    /// veto guards the CONTROLLER's own self-termination only).
    #[test]
    fn sign_check_opponent_damage_marked_increase_not_vetoed() {
        let mut prior = GameState::new_two_player(7);
        let oid = inert_token(&mut prior, 610, 1, "Bear"); // controller 1 = opponent
        prior.objects.get_mut(&oid).unwrap().damage_marked = 1;
        let mut current = prior.clone();
        current.objects.get_mut(&oid).unwrap().damage_marked = 4;
        assert!(
            sign_check(&prior, &current),
            "an OPPONENT's damage_marked increase must NOT be vetoed (controller-scoped)"
        );
    }

    fn recast_ctx(uses_buyback: bool) -> crate::types::game_state::LoopActionContext {
        use crate::types::game_state::BuybackUsage;
        crate::types::game_state::LoopActionContext {
            card_id: CardId(4242),
            controller: PlayerId(0),
            action: crate::types::game_state::LoopAction::Recast {
                from_zone: Zone::Hand,
                uses_buyback: if uses_buyback {
                    BuybackUsage::Used
                } else {
                    BuybackUsage::NotUsed
                },
            },
            convoke: Some(crate::types::game_state::ConvokeMode::Convoke),
            pins: Vec::new(),
        }
    }

    /// N7 (F1 two-sided `last_loop_action_sequence` classify — COVER path via `eq_except_growable`).
    /// (a) two object-cover-equal frames with EQUAL contexts still CERTIFY (no false-negative);
    /// (b) the same frames with a MUTATED context (`uses_buyback` flipped) REJECT (no
    /// false-positive — a heterogeneous recast is caught). Revert-failing: removing the
    /// `a.last_loop_action_sequence == b.last_loop_action_sequence` conjunct in `eq_except_growable` flips
    /// (b) to COVER while (a) stays COVER ⇒ this test's (b) assertion fails. (a) is the paired
    /// positive reach-guard for (b). Non-vacuous: the custom `impl PartialEq for GameState`
    /// EXCLUDES the field, so this conjunct is the SOLE discriminator.
    #[test]
    fn fodder_cover_last_loop_action_sequence_two_sided() {
        // (a) equal contexts ⇒ still covers.
        let (mut prior, mut current) = fodder_cover_base();
        prior.last_loop_action_sequence = vec![recast_ctx(true)];
        current.last_loop_action_sequence = vec![recast_ctx(true)];
        assert!(
            fodder_cover(&prior, &current),
            "(a) equal last_loop_action_sequence ⇒ object-growth cover still CERTIFIES"
        );
        // (b) mutated context (uses_buyback true→false) ⇒ rejects.
        let (mut p2, mut c2) = fodder_cover_base();
        p2.last_loop_action_sequence = vec![recast_ctx(true)];
        c2.last_loop_action_sequence = vec![recast_ctx(false)];
        assert!(
            !fodder_cover(&p2, &c2),
            "(b) a heterogeneous recast (uses_buyback flipped) must REJECT (F1 COMPARED conjunct)"
        );
    }

    /// N7 (equal path via `loop_states_equal_modulo_resources`). The same two-sided classify on
    /// the constant-depth equality gate (the materializer-boundary first disjunct). In-test
    /// invariance note: `ConvokeMode` is a unit-variant enum carrying zero per-iteration data
    /// and `card_id` is a `CardId` (not an `ObjectId`), so a homogeneous loop's contexts are
    /// byte-equal iteration-to-iteration ⇒ COMPARING is safe (no false-negative on a real loop).
    #[test]
    fn loop_states_equal_last_loop_action_sequence_two_sided() {
        let mut a = GameState::new_two_player(7);
        inert_token(&mut a, 900, 0, "Engine");
        let mut b = a.clone();
        // (a) equal contexts ⇒ equal.
        a.last_loop_action_sequence = vec![recast_ctx(true)];
        b.last_loop_action_sequence = vec![recast_ctx(true)];
        assert!(
            loop_states_equal_modulo_resources(&a, &b),
            "equal last_loop_action_sequence ⇒ loop_states_equal_modulo_resources holds"
        );
        // (b) mutated context ⇒ unequal.
        b.last_loop_action_sequence = vec![recast_ctx(false)];
        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "a mutated last_loop_action_sequence (uses_buyback flipped) ⇒ NOT equal (F1 conjunct)"
        );
    }

    fn activate_ctx(ability_index: usize) -> crate::types::game_state::LoopActionContext {
        crate::types::game_state::LoopActionContext {
            card_id: CardId(4242),
            controller: PlayerId(0),
            action: crate::types::game_state::LoopAction::Activate {
                source_id: crate::types::identifiers::ObjectId(77),
                ability_index,
            },
            convoke: None,
            pins: Vec::new(),
        }
    }

    /// P1-7: an ACTIVATION loop whose captured action differs across cycles (a different
    /// `ability_index` — a heterogeneous cycle) must NOT cover. Mirrors the recast two-sided
    /// classify on the `Activate` shape: (a) equal contexts still certify (paired positive
    /// reach-guard); (b) two contexts with different `ability_index` REJECT. Revert-failing:
    /// removing the `a.last_loop_action_sequence == b.last_loop_action_sequence` conjunct in
    /// `eq_except_growable` flips (b) to COVER. Non-vacuous: `impl PartialEq for GameState`
    /// EXCLUDES the field, so this conjunct is the SOLE discriminator.
    #[test]
    fn fodder_cover_heterogeneous_activation_context_rejects() {
        // (a) equal Activate contexts ⇒ still covers.
        let (mut prior, mut current) = fodder_cover_base();
        prior.last_loop_action_sequence = vec![activate_ctx(0)];
        current.last_loop_action_sequence = vec![activate_ctx(0)];
        assert!(
            fodder_cover(&prior, &current),
            "(a) equal Activate contexts ⇒ object-growth cover still CERTIFIES"
        );
        // (b) different ability_index (heterogeneous activation) ⇒ rejects.
        let (mut p2, mut c2) = fodder_cover_base();
        p2.last_loop_action_sequence = vec![activate_ctx(0)];
        c2.last_loop_action_sequence = vec![activate_ctx(1)];
        assert!(
            !fodder_cover(&p2, &c2),
            "(b) a heterogeneous activation (ability_index 0→1) must REJECT (F1 COMPARED conjunct)"
        );
    }

    // ─────── PR-7 v4 (CR 732.2a): persistent-axis collapse routing + δ + partition ───────

    /// CR 732.2a: `counter_growth_is_observed` / `life_growth_is_observed` ROUTE an accepted loop —
    /// false ⇒ batched N×δ (sound only when that axis is unobserved), true ⇒ the discrete N-cycle
    /// driver. The firewall is AXIS-SPECIFIC: a life observer must NOT veto a batched counter gain
    /// and vice-versa (an incidental board observer of one axis never mis-routes a disjoint-axis
    /// loop). Matched pairs: a benign board is UNOBSERVED on both axes; adding a per-event observer
    /// of ONE class (Heliod-like `LifeGained` / `CounterAdded` trigger, or a `GainLife`/`AddCounter`
    /// replacement) FLIPS ONLY that axis. This is the CORRECTNESS gate — the batched apply fires a
    /// lump observer once, not N×.
    ///
    /// REVERT-PROBE (discriminating): delete the per-event trigger scan (block 2) ⇒ the
    /// `LifeGained` / `CounterAdded` rows flip to false; delete the replacement scan (block 3) ⇒
    /// the `GainLife` / `AddCounter` rows flip to false. Each observer row is reach-guarded by the
    /// benign-false row (proves the fixtures otherwise pass the firewall) AND by the CROSS-axis
    /// false assertion (proves the flip is axis-scoped, not a coarse OR).
    #[test]
    fn persistent_axis_growth_is_observed_routes_on_observer() {
        use crate::types::ability::{ReplacementDefinition, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        // Reach-guard: a battlefield permanent with a BENIGN (non-life/non-counter) trigger is
        // UNOBSERVED on both axes — the batched fast path is taken.
        let mut benign = GameState::new_two_player(7);
        let id = bf_object(&mut benign, 100);
        benign.objects.get_mut(&id).unwrap().trigger_definitions =
            vec![TriggerDefinition::new(TriggerMode::ChangesZone)].into();
        assert!(
            !counter_growth_is_observed(&benign) && !life_growth_is_observed(&benign),
            "a benign ChangesZone trigger observes neither axis (batched path)"
        );

        // Returns (counter_observed, life_observed) so each row asserts the flipped axis AND the
        // untouched cross-axis stays false.
        let observed_with = |set: fn(&mut GameObject)| {
            let mut state = GameState::new_two_player(7);
            let id = bf_object(&mut state, 100);
            set(state.objects.get_mut(&id).unwrap());
            (
                counter_growth_is_observed(&state),
                life_growth_is_observed(&state),
            )
        };

        // (life trigger) Heliod-like "whenever you gain life …" ⇒ LIFE observed, COUNTER not.
        assert_eq!(
            observed_with(|o| o.trigger_definitions =
                vec![TriggerDefinition::new(TriggerMode::LifeGained)].into()),
            (false, true),
            "a LifeGained trigger (Heliod) observes ONLY the life axis"
        );
        // (counter trigger) "whenever a +1/+1 counter is put …" ⇒ COUNTER observed, LIFE not.
        assert_eq!(
            observed_with(|o| o.trigger_definitions =
                vec![TriggerDefinition::new(TriggerMode::CounterAdded)].into()),
            (true, false),
            "a CounterAdded trigger observes ONLY the counter axis"
        );
        // (life replacement) Rhox-like life-gain replacement ⇒ LIFE observed, COUNTER not.
        assert_eq!(
            observed_with(|o| o.replacement_definitions =
                vec![ReplacementDefinition::new(ReplacementEvent::GainLife)].into()),
            (false, true),
            "a GainLife replacement (Rhox) observes ONLY the life axis"
        );
        // (counter replacement) Corpsejack-like counter-placement doubler ⇒ COUNTER observed, LIFE not.
        assert_eq!(
            observed_with(|o| o.replacement_definitions =
                vec![ReplacementDefinition::new(ReplacementEvent::AddCounter)].into()),
            (true, false),
            "an AddCounter replacement (Corpsejack) observes ONLY the counter axis"
        );
    }

    /// CR 732.2a: `counter_is_beneficial_materializable` is the wildcard-free batched-collapse
    /// partition — Generic / +1/+1 / loyalty / defense are materializable; every harmful /
    /// duration / SBA-gating counter is NOT. REVERT-PROBE: flip the `{Plus1Plus1, Loyalty,
    /// Defense}` arms to false ⇒ the beneficial rows flip (the probe-proven +1/+1 / loyalty /
    /// defense gap re-opens).
    #[test]
    fn counter_is_beneficial_materializable_partition() {
        use crate::types::keywords::KeywordKind;
        for ct in [
            CounterType::Generic("charge".to_string()),
            CounterType::Plus1Plus1,
            CounterType::Loyalty,
            CounterType::Defense,
        ] {
            assert!(
                counter_is_beneficial_materializable(&ct),
                "{ct:?} is a beneficial-materializable counter"
            );
        }
        for ct in [
            CounterType::Minus1Minus1,
            CounterType::PowerToughness {
                power: 1,
                toughness: 0,
            },
            CounterType::Stun,
            CounterType::Lore,
            CounterType::Time,
            CounterType::Fade,
            CounterType::Age,
            CounterType::Shield,
            CounterType::Finality,
            CounterType::Keyword(KeywordKind::Flying),
        ] {
            assert!(
                !counter_is_beneficial_materializable(&ct),
                "{ct:?} is NOT a beneficial-materializable counter"
            );
        }
    }

    /// CR 122.1 + CR 119.3: the batched δ capture — `grown_beneficial_counter_deltas` returns the
    /// per-object beneficial counter growth, `grown_life_deltas` the per-player life gain, each as
    /// the exact per-cycle δ (multiplied by N at the boundary). Only GROWTH (a > b / gain > 0) is
    /// returned; a shrink/loss is a distinct SBA axis, never a batched gain.
    #[test]
    fn beneficial_counter_and_life_deltas_capture_growth_only() {
        let mut prior = GameState::new_two_player(7);
        let cid = bf_object(&mut prior, 200);
        prior
            .objects
            .get_mut(&cid)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 3);
        let mut current = prior.clone();
        current
            .objects
            .get_mut(&cid)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 5); // +2
        current.players[0].life += 4;

        assert_eq!(
            grown_beneficial_counter_deltas(&prior, &current),
            vec![(cid, CounterType::Plus1Plus1, 2)],
            "captures the +2 per-cycle +1/+1 growth"
        );
        assert_eq!(
            grown_life_deltas(&prior, &current),
            vec![(current.players[0].id, 4)],
            "captures the +4 per-cycle life gain"
        );

        // Reach-guard: a life LOSS is not a gain axis (empty δ).
        let mut shrink = prior.clone();
        shrink.players[0].life -= 2;
        assert!(
            grown_life_deltas(&prior, &shrink).is_empty(),
            "a life LOSS yields no batched gain δ"
        );
    }

    /// A battlefield permanent carrying ONE `TriggerMode::Phase` trigger whose step
    /// (`Phase::End`) the state is NOT in — the "phase-gated observer" population.
    /// CR 500.1: phases and steps proceed in a fixed order, so a window
    /// that provably never leaves `PreCombatMain` never reaches this trigger's step.
    /// That is exactly the population a populated `LoopWindowScope::phase_invariant`
    /// proof can change the answer on, which is why the identity row asserts here.
    fn phase_gated_observer_board(condition: crate::types::ability::TriggerCondition) -> GameState {
        use crate::types::ability::TriggerDefinition;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(7);
        state.phase = Phase::PreCombatMain;
        let id = bf_object(&mut state, 100);
        state.objects.get_mut(&id).unwrap().trigger_definitions =
            vec![TriggerDefinition::new(TriggerMode::Phase)
                .phase(Phase::End)
                .condition(condition)]
            .into();
        state
    }

    /// Phase 1a (Seam A). Each of the three CR 732.2a window predicates keeps its
    /// 2-arg/1-arg name as a **1-line wrapper** delegating to a `_scoped` sibling with
    /// [`LoopWindowScope::unproven`], so pre-change neutrality is STRUCTURAL
    /// (`f(a,b) ≡ f_scoped(a,b, unproven())`) rather than something each caller has
    /// to re-establish. This row pins that identity over five populations, including
    /// the phase-gated observer board named in `phase_gated_observer_board`.
    ///
    /// NON-VACUITY (trap 7 — the instrument must be able to return both values):
    /// every predicate is asserted at a population where it answers `true` AND at one
    /// where it answers `false`, and the row asserts the collected answer vectors
    /// directly. A constant `_scoped` body — the failure a bare `a == b` identity
    /// check cannot see — fails the vector assertions.
    ///
    /// REVERT-PROBE (live at this phase): stop a wrapper delegating (restore the old
    /// inline body, or have it pass anything other than `unproven()`) ⇒ the matching
    /// arm's `assert_eq!` fails. Since the growing-class firewall now READS
    /// `phase_invariant` / `sole_driver`, "make `unproven()` populate a field" is a live
    /// probe too: the phase-gated observer board below is precisely the population a
    /// populated `phase_invariant` changes the answer on, so a non-`None` `unproven()`
    /// breaks the identity here rather than silently.
    #[test]
    fn scoped_wrappers_are_identity() {
        use crate::types::ability::TriggerCondition;

        // (1)/(2) cover pairs: one that covers, one that does not (an extra permanent
        // breaks gate (1)'s board equality) — so the cover predicate is exercised at
        // both answers.
        let (cover_prior, cover_current) = cover_base();
        let (nocover_prior, nocover_current) = {
            let (p, mut c) = cover_base();
            bf_object(&mut c, 900);
            (p, c)
        };

        // (3) a benign board: neither firewall fires.
        let benign = GameState::new_two_player(7);
        // (4) phase-gated SIBLING observer: `ControlsType` is a live board census ⇒ the
        // growing-class firewall vetoes, the projected-resource firewall does not.
        let sibling_observer = phase_gated_observer_board(TriggerCondition::ControlsType {
            filter: TargetFilter::Any,
        });
        // (5) phase-gated PROJECTED observer: "if you gained life this turn" reads a
        // projected player axis ⇒ the projected firewall vetoes.
        let projected_observer =
            phase_gated_observer_board(TriggerCondition::GainedLife { minimum: 1 });

        let cover = |prior: &GameState, current: &GameState| {
            let plain = loop_states_cover_modulo_growth(prior, current);
            assert_eq!(
                plain,
                loop_states_cover_modulo_growth_scoped(
                    prior,
                    current,
                    LoopWindowScope::unproven(),
                    &mut PeriodVerdicts::unproven(current)
                ),
                "loop_states_cover_modulo_growth must be its _scoped sibling at unproven()"
            );
            plain
        };
        let growing = |state: &GameState| {
            let plain = fire_time_conditions_read_growing_class(state, None);
            assert_eq!(
                plain,
                fire_time_conditions_read_growing_class_scoped(
                    state,
                    None,
                    LoopWindowScope::unproven()
                ),
                "fire_time_conditions_read_growing_class must be its _scoped sibling at unproven()"
            );
            plain
        };
        let projected = |state: &GameState| {
            let plain = fire_time_conditions_read_projected_resource(state);
            assert_eq!(
                plain,
                fire_time_conditions_read_projected_resource_scoped(
                    state,
                    LoopWindowScope::unproven()
                ),
                "fire_time_conditions_read_projected_resource must be its _scoped sibling at unproven()"
            );
            plain
        };

        assert_eq!(
            [
                cover(&cover_prior, &cover_current),
                cover(&nocover_prior, &nocover_current)
            ],
            [true, false],
            "the cover predicate must answer BOTH ways across the two pairs — a constant \
             implementation would satisfy identity alone"
        );
        assert_eq!(
            [
                growing(&benign),
                growing(&sibling_observer),
                growing(&projected_observer)
            ],
            [false, true, false],
            "the growing-class firewall vetoes on the sibling observer only"
        );
        assert_eq!(
            [
                projected(&benign),
                projected(&sibling_observer),
                projected(&projected_observer)
            ],
            [false, false, true],
            "the projected-resource firewall vetoes on the projected observer only"
        );
    }

    /// Candidate windows for the Seam A cast proof, each paired with its EXPECTED
    /// `is_forced_cascade_window` membership.
    ///
    /// The `bool` is what makes drift loud in BOTH directions, and it exists because the
    /// caller previously derived its obligation by FILTERING this list through the very
    /// predicate under test: deleting a member then silently shrank the proof obligation
    /// and left the row green (measured — a reviewer's revert probe deleted seven members
    /// and the row still passed). With an expected-membership column, deleting a member
    /// fails its `true` row and adding one of the listed non-members fails its `false`
    /// row. The list is the authority; the predicate is the thing being measured against
    /// it. A member absent from here is still simply never proved — see the `ponytail:`
    /// note on the caller for that residual and its upgrade path.
    ///
    /// `on_board` must be objects that really exist ON THE BATTLEFIELD in the caller's
    /// state and `in_hand` a card that really exists in hand: the turn-based windows
    /// carry object references the per-viewer legal-action enumerator dereferences, and
    /// each reference has a zone the window implies — untap candidates, the exerting /
    /// enlisting attacker and the enlist-eligible creature are battlefield permanents
    /// (CR 502.3 / CR 508.1g), while `DiscardToHandSize` names cards in hand (CR 514.1).
    /// Passing a hand card as an untap candidate measures a window no rules path can
    /// produce.
    ///
    /// Same requirement, one level deeper: a window whose payload the enumerator needs
    /// but the fixture leaves at `Default::default()` produces ZERO actions of any kind,
    /// so "it enumerates no cast" is inert rather than measured. `attacker` is an
    /// OPPOSING battlefield creature the caller has also entered into `state.combat`, so
    /// the CR 509.1 window offers a real block. The caller's per-window reach-guard is
    /// what keeps that requirement enforced instead of documented.
    fn cast_proof_candidate_windows(
        on_board: [ObjectId; 2],
        in_hand: ObjectId,
        attacker: ObjectId,
    ) -> Vec<(&'static str, crate::types::game_state::WaitingFor, bool)> {
        use crate::types::game_state::WaitingFor;
        vec![
            (
                "Priority{active} — CR 704.3 SBA point; NOT exempt, and the positive control",
                WaitingFor::Priority {
                    player: PlayerId(0),
                },
                false,
            ),
            (
                "Priority{non-active} — same, and the sampler's ring-clearing arm",
                WaitingFor::Priority {
                    player: PlayerId(1),
                },
                false,
            ),
            (
                "RedistributeLifeTotals — a window that CAN MOVE LIFE, so never exempt",
                WaitingFor::RedistributeLifeTotals {
                    player: PlayerId(0),
                    options: Vec::new(),
                },
                false,
            ),
            (
                "AssignCombatDamage — turn-based (CR 510.1) but CR 510.2 deals the damage \
                 with no intervening priority, so it MOVES LIFE",
                WaitingFor::AssignCombatDamage {
                    player: PlayerId(0),
                    attacker_id: on_board[0],
                    total_damage: 2,
                    blockers: Vec::new(),
                    assignment_modes: Vec::new(),
                    trample: None,
                    defending_player: PlayerId(1),
                    attack_target: crate::game::combat::default_attack_target(),
                    pw_loyalty: None,
                    pw_controller: None,
                },
                false,
            ),
            (
                "CombatTaxPayment — CR 508.1j / CR 509.1f cost sub-step; a Phyrexian tax \
                 symbol is paid with 2 life (CR 107.4f), so it MOVES LIFE",
                WaitingFor::CombatTaxPayment {
                    player: PlayerId(0),
                    context: crate::types::game_state::CombatTaxContext::Attacking,
                    total_cost: crate::types::mana::ManaCost::Cost {
                        shards: vec![crate::types::mana::ManaCostShard::PhyrexianWhite],
                        generic: 0,
                    },
                    per_creature: Vec::new(),
                    pending: crate::types::game_state::CombatTaxPending::Attack {
                        attacks: Vec::new(),
                        bands: Vec::new(),
                    },
                },
                false,
            ),
            (
                "OrderTriggers (CR 603.3b)",
                WaitingFor::OrderTriggers {
                    player: PlayerId(0),
                    // TWO summaries, matching the two-trigger group the caller puts in
                    // `state.pending_trigger_order`: `order_triggers_candidates` is keyed
                    // on this length and yields nothing at length 0, and
                    // `handle_order_triggers` rejects any order whose length disagrees
                    // with the pending group. CR 603.3b needs a real choice — with ONE
                    // trigger `begin_trigger_ordering` auto-orders the group
                    // (`g.triggers.len() <= 1 => g.ordered = true`) and
                    // `build_next_order_triggers_prompt` only ever returns an UNORDERED
                    // group, so no rules path opens this window over a singleton and
                    // `order: [0]` is the only legal answer rather than an ordering.
                    // The two members must also differ, or the order-independence check
                    // auto-orders them too; each `description` mirrors the group's
                    // `PendingTrigger.description`, which is what the real builder copies
                    // into the summary.
                    triggers: vec![
                        crate::types::game_state::PendingTriggerSummary {
                            source_id: on_board[0],
                            source_name: "Test Bear 0".to_string(),
                            description: "you gain 1 life".to_string(),
                        },
                        crate::types::game_state::PendingTriggerSummary {
                            source_id: on_board[1],
                            source_name: "Test Bear 1".to_string(),
                            description: "you gain 2 life".to_string(),
                        },
                    ],
                },
                true,
            ),
            (
                "TriggerTargetSelection (CR 603.3d)",
                WaitingFor::TriggerTargetSelection {
                    player: PlayerId(0),
                    trigger_controller: None,
                    trigger_event: None,
                    trigger_events: Vec::new(),
                    target_slots: Vec::new(),
                    mode_labels: Vec::new(),
                    target_constraints: Vec::new(),
                    // CR 603.3d: one legal target for the current slot. The enumerator
                    // for this window maps `current_legal_targets` directly to
                    // `ChooseTarget`, so an empty progress makes the window offer nothing
                    // at all and the cast-zero below unreadable.
                    selection: crate::types::game_state::TargetSelectionProgress {
                        current_legal_targets: vec![TargetRef::Object(on_board[0])],
                        ..Default::default()
                    },
                    source_id: None,
                    description: None,
                },
                true,
            ),
            (
                "OptionalEffectChoice (CR 603.5 + CR 608.2d)",
                WaitingFor::OptionalEffectChoice {
                    player: PlayerId(0),
                    source_id: on_board[0],
                    description: None,
                    may_trigger_key: None,
                },
                true,
            ),
            (
                "CommanderZoneChoice (CR 903.9a)",
                WaitingFor::CommanderZoneChoice {
                    player: PlayerId(0),
                    commander_id: ObjectId(2),
                    current_zone: Zone::Graveyard,
                },
                true,
            ),
            (
                "ChooseLegend (CR 704.5j)",
                WaitingFor::ChooseLegend {
                    player: PlayerId(0),
                    legend_name: "Delianfel, Prayerful Herald".to_string(),
                    candidates: on_board.to_vec(),
                },
                true,
            ),
            (
                "BattleProtectorChoice (CR 310.11 + CR 704.5w / CR 704.5x)",
                WaitingFor::BattleProtectorChoice {
                    player: PlayerId(0),
                    battle_id: ObjectId(5),
                    candidates: vec![PlayerId(1)],
                },
                true,
            ),
            // CR 703.1 turn-based members. CR 117.3a puts every one of them strictly
            // before the active player receives priority, so CR 117.1a / CR 305.1 bar
            // a cast or land play at each just as they do at the SBA members above.
            (
                "UntapChoice (CR 502.3 + CR 117.3a)",
                WaitingFor::UntapChoice {
                    player: PlayerId(0),
                    // CR 502.3 untaps PERMANENTS: the candidates must be on the
                    // battlefield, not a card in hand.
                    candidates: on_board.to_vec(),
                    chosen_not_to_untap: Vec::new(),
                },
                true,
            ),
            (
                "ChooseUntapSubset (CR 502.3)",
                WaitingFor::ChooseUntapSubset {
                    player: PlayerId(0),
                    group: on_board.to_vec(),
                    // CR 502.3 cap. `max: 1` over a 2-permanent group keeps the
                    // variant's `group.len() > max` invariant AND admits a real
                    // non-empty choice — with `max: 0` the only legal selection is the
                    // empty one, so "this window enumerates no cast" would be a
                    // degenerate zero rather than a measured one.
                    max: 1,
                },
                true,
            ),
            (
                "DeclareAttackers (CR 508.1)",
                WaitingFor::DeclareAttackers {
                    player: PlayerId(0),
                    valid_attacker_ids: on_board.to_vec(),
                    // CR 506.2: in a two-player game the NONACTIVE player is the defending
                    // player, and only that player (plus their planeswalkers and the
                    // battles they protect) may be attacked. `default_attack_target()` is
                    // `Player(PlayerId(0))`, i.e. P0's own creatures attacking P0, which
                    // the simulation filter rejects for every non-empty proposal. The
                    // guard below then passes on the decline alone (measured: the window
                    // offered `[DeclareAttackers { attacks: [], bands: [] }]`). The
                    // opposing seat is what makes it offer a GENUINE attack.
                    valid_attack_targets: vec![crate::game::combat::AttackTarget::Player(
                        PlayerId(1),
                    )],
                    valid_attack_targets_by_attacker: None,
                    attacker_constraints: Default::default(),
                },
                true,
            ),
            (
                "ExertChoice (CR 508.1g + CR 701.43d)",
                WaitingFor::ExertChoice {
                    player: PlayerId(0),
                    // CR 701.43d exerts an ATTACKING permanent.
                    attacker: on_board[0],
                    remaining: Vec::new(),
                },
                true,
            ),
            (
                "EnlistChoice (CR 508.1g + CR 702.154b)",
                WaitingFor::EnlistChoice {
                    player: PlayerId(0),
                    attacker: on_board[0],
                    // CR 702.154a taps another untapped creature you control — a
                    // battlefield permanent, and a DIFFERENT one from the attacker.
                    eligible: vec![on_board[1]],
                    remaining: Vec::new(),
                },
                true,
            ),
            (
                "DeclareBlockers (CR 509.1)",
                WaitingFor::DeclareBlockers {
                    player: PlayerId(0),
                    valid_blocker_ids: on_board.to_vec(),
                    // CR 509.1a: a real "this creature may block that attacker" pairing.
                    // `blocker_actions` enumerates block proposals strictly from this
                    // map, so an empty map leaves only the decline (the empty
                    // declaration) — measured: with `state.combat` present but this map
                    // empty the guard below passes on the decline alone. Populating it is
                    // what makes the window offer a GENUINE block, which is what the
                    // cast-zero is supposed to be measured against.
                    valid_block_targets: on_board
                        .iter()
                        .map(|&blocker| (blocker, vec![attacker]))
                        .collect(),
                    block_requirements: Default::default(),
                    blocker_constraints: Default::default(),
                },
                true,
            ),
            (
                "DiscardToHandSize (CR 514.1 + CR 514.3)",
                WaitingFor::DiscardToHandSize {
                    player: PlayerId(0),
                    count: 1,
                    // CR 514.1 discards from HAND — the one window whose object
                    // reference is correctly a hand card.
                    cards: vec![in_hand],
                },
                true,
            ),
        ]
    }

    /// Seam A's `cast_card_ids: Some(&[])` proof, pinned as a row.
    ///
    /// CR 117.1a (a spell is cast only with priority) and CR 305.1 (a land is played
    /// only with priority) say no cast or land-play can happen at a window where
    /// nobody holds priority. This row measures that claim against the engine's own
    /// legal-action enumerator instead of trusting it: on ONE board it enumerates the
    /// deliberate class (`CastSpell` / `PlayLand` / `ActivateAbility`) at a `Priority`
    /// window and at every window `is_forced_cascade_window` currently exempts.
    ///
    /// The exempt set is DERIVED from a candidate list that includes non-members, rather
    /// than hardcoded. That is what keeps the revert-probe live: widening the predicate
    /// widens what this row has to prove. Measured — with a hardcoded exempt-only list,
    /// adding `Priority` to the class left this row green, because the legal-action
    /// enumerator never consults the predicate.
    ///
    /// FAILS LOUDLY ON CLASS DRIFT IN BOTH DIRECTIONS. Deriving the obligation by
    /// FILTERING the candidate list through `is_forced_cascade_window` — the predicate
    /// under test — was itself a hole: deleting a member just shrank the loop, and a
    /// reviewer's revert probe deleting seven members left this row GREEN. Each candidate
    /// now carries its EXPECTED membership and that expectation is asserted before the
    /// cast proof runs, so DELETING a member fails its `true` row and ADDING one of the
    /// enumerated non-members (`Priority` either seat, `RedistributeLifeTotals`,
    /// `AssignCombatDamage`, `CombatTaxPayment`) fails its `false` row.
    ///
    /// ponytail: a brand-new `WaitingFor` variant added to the predicate but to no list
    /// is still silent. Closing that needs an exhaustive 127-arm `WaitingFor` destructure;
    /// deliberately not built, because the load-bearing non-members are enumerated here
    /// and `is_forced_cascade_window`'s FAIL-CLOSED fall-through makes a forgotten variant
    /// a conservative miss rather than a soundness hole. Upgrade path if that changes:
    /// mirror `types::game_state::_gamestate_partition_is_total`'s no-`..` destructure
    /// over `WaitingFor` so the build breaks when a variant is added.
    ///
    /// That mechanism did its job when the class was widened to the CR 703.1 turn-based
    /// actions: the seven new members (CR 502.3 untap, CR 508.1 / CR 508.1g declare
    /// attackers + exert/enlist, CR 509.1 declare blockers, CR 514.1 cleanup discard)
    /// were added to the candidate list and re-measured, and none of them enumerates a
    /// `CastSpell` / `PlayLand` / `ActivateAbility`. So `cast_card_ids: Some(&[])` still
    /// holds for a window retained across a turn boundary: untapping, declaring,
    /// exerting/enlisting and discarding to hand size are not casts, and CR 117.3a
    /// grants nobody the priority CR 117.1a / CR 305.1 require.
    ///
    /// NON-VACUITY (trap 7 — a zero from an instrument that cannot return non-zero):
    /// the `Priority` arm runs FIRST and is asserted NON-EMPTY on the same board, so
    /// the zeros below are proved zeros, not an inert enumerator. The exempt set is
    /// also asserted non-empty, so "no exempt window admits a cast" cannot pass by the
    /// class being empty.
    ///
    /// ⚠️ SCOPE LIMIT (stated, not implied): this row enumerates the legal actions
    /// available **at** a window. It therefore cannot see the `apply_action` bypass
    /// class — the handful of `GameAction`s that early-return before the ring clear
    /// (`ReorderHand`, `Concede`, `Debug`, `GrantDebugPermission`,
    /// `RevokeDebugPermission`, `CancelAutoPass`, `SetPhaseStops`,
    /// `SetPriorityPassingMode`). That class is discharged separately by enumeration:
    /// none of those actions casts a spell or plays a land, so the proof is unaffected.
    ///
    /// REVERT-PROBE: add `WaitingFor::Priority { .. }` to `is_forced_cascade_window`'s
    /// `matches!` ⇒ a `Priority` window becomes "exempt", casts become admissible
    /// inside a retained window, and the `Some(&[])` proof this row pins is false.
    #[test]
    fn no_exempt_window_admits_a_cast() {
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        // A land in hand makes the deliberate class REACHABLE on this board (CR 305.1 + CR 305.2:
        // main phase, empty stack, the active player holds priority, land drop unused).
        // It is also the ONLY correct object for `DiscardToHandSize` (CR 514.1 discards
        // from hand) — every other window below names a battlefield permanent.
        let in_hand = crate::game::zones::create_object(
            &mut state,
            CardId(701),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&in_hand)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        // Two real battlefield creatures. CR 502.3 untap candidates, the CR 508.1g
        // exerting attacker and its CR 702.154a enlist-eligible partner are all
        // permanents; passing the hand card for those built windows no rules path can
        // produce, and the per-viewer enumerator dereferences every one of them.
        let on_board = [0u64, 1].map(|i| {
            let id = crate::game::zones::create_object(
                &mut state,
                CardId(710 + i),
                PlayerId(0),
                format!("Test Bear {i}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            id
        });

        // CR 509.1a: a real attacking creature CONTROLLED BY THE OPPONENT and
        // entered into `state.combat`, so the CR 509.1 window below is answerable. The
        // blocker-action enumerator runs every proposal through the engine's own
        // `handle_declare_blockers`, which errors out with "No combat state (attackers
        // not declared)" when `state.combat` is `None` — every candidate is then filtered
        // away and the window offers nothing at all.
        let attacker = crate::game::zones::create_object(
            &mut state,
            CardId(730),
            PlayerId(1),
            "Test Ogre".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
        state.combat = Some(crate::game::combat::CombatState {
            attackers: vec![crate::game::combat::AttackerInfo::attacking_player(
                attacker,
                PlayerId(0),
            )],
            ..Default::default()
        });

        // CR 603.3b: one unordered group, matching the two-summary `OrderTriggers`
        // window. `handle_order_triggers` reads the group (not the window) for the
        // permutation length and rejects the submission outright without it, so the
        // window's candidates would be filtered out and its zero rendered inert.
        //
        // TWO members, and DIFFERENT ones. `begin_trigger_ordering` auto-orders any
        // group that is a singleton or `group_is_order_independent`, and only an
        // unordered group ever becomes a prompt — so a one-trigger group, or two
        // triggers with identical normalized abilities, is a window no rules path can
        // open. Distinct life amounts make the group order-dependent by the engine's
        // own conservative identity check, which is the reachable shape. Both stay
        // inert: no targets, no modes, no resolution choice.
        let inert_life_trigger = |source_id, value, description: &str| {
            // `single` (not a struct literal) supplies the CR 603.7 firing identity:
            // `TriggerFiring::Ordinary`. A literal would leave the field's `#[default]`
            // `UnknownLegacy`, which is reserved for persisted records whose install
            // receipt cannot be reconstructed — never for a freshly built trigger.
            crate::game::triggers::PendingTriggerContext::single(
                crate::game::triggers::PendingTrigger {
                    source_id,
                    controller: PlayerId(0),
                    condition: None,
                    ability: Box::new(ResolvedAbility::new(
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value },
                            player: TargetFilter::Controller,
                        },
                        vec![],
                        source_id,
                        PlayerId(0),
                    )),
                    timestamp: 0,
                    target_constraints: Vec::new(),
                    distribute: None,
                    trigger_event: None,
                    modal: None,
                    mode_abilities: Vec::new(),
                    // The real prompt builder COPIES this into the summary
                    // (`description.clone().unwrap_or_default()`), so a `None` here
                    // under a described summary is a state the engine cannot produce.
                    description: Some(description.to_string()),
                    may_trigger_origin: None,
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            )
        };
        state.pending_trigger_order = Some(crate::types::game_state::PendingTriggerOrder {
            groups: vec![crate::types::game_state::TriggerOrderGroup {
                controller: PlayerId(0),
                triggers: vec![
                    inert_life_trigger(on_board[0], 1, "you gain 1 life"),
                    inert_life_trigger(on_board[1], 2, "you gain 2 life"),
                ],
                ordered: false,
            }],
            resume_after_ordering: None,
        });

        let deliberate = |s: &GameState| -> Vec<GameAction> {
            crate::ai_support::legal_actions(s)
                .into_iter()
                .filter(|a| {
                    matches!(
                        a,
                        GameAction::CastSpell { .. }
                            | GameAction::PlayLand { .. }
                            | GameAction::ActivateAbility { .. }
                    )
                })
                .collect()
        };

        // POSITIVE CONTROL, asserted before any zero is read.
        let at_priority = deliberate(&state);
        assert!(
            !at_priority.is_empty(),
            "reach-guard: the enumerator must return a deliberate action at a Priority \
             window on this board, else every zero below is an inert instrument"
        );

        // CLASS-DRIFT GATE, run before the cast proof: every candidate's membership must
        // be what the list says it is. A deleted member reds its `true` row here; an
        // added non-member reds its `false` row.
        let candidates = cast_proof_candidate_windows(on_board, in_hand, attacker);
        for (why, window, expected_member) in &candidates {
            assert_eq!(
                window.is_forced_cascade_window(),
                *expected_member,
                "CLASS DRIFT — `is_forced_cascade_window` disagrees with the candidate \
                 table on {why}. Expected member = {expected_member}."
            );
        }
        let (members, non_members): (usize, usize) = candidates.iter().fold(
            (0, 0),
            |(m, n), (_, _, e)| if *e { (m + 1, n) } else { (m, n + 1) },
        );
        assert!(
            members > 0 && non_members > 0,
            "reach-guard: both halves of the table must be populated — a one-sided table \
             is satisfiable by a constant predicate; got {members} members / \
             {non_members} non-members"
        );

        for (why, window, expected_member) in candidates {
            if !expected_member {
                continue;
            }
            state.waiting_for = window;
            // PER-WINDOW REACH-GUARD. The zero below is only evidence if the enumerator
            // is live AT THIS WINDOW. A member whose fixture is under-populated (a
            // `Default::default()` where the enumerator needs real data) yields zero
            // deliberate actions because it yields zero actions AT ALL — an inert
            // instrument, not a measured absence. Measured on the pre-guard fixtures,
            // three members were exactly that: `OrderTriggers` (no `pending_trigger_order`
            // group ⇒ no valid permutation), `TriggerTargetSelection` (empty
            // `current_legal_targets`) and `DeclareBlockers` (`state.combat: None` ⇒ every
            // proposal rejected by the simulation filter).
            assert!(
                !crate::ai_support::legal_actions(&state).is_empty(),
                "{why} must offer at least one legal answer, else the zero below is inert"
            );
            let found = deliberate(&state);
            assert!(
                found.is_empty(),
                "{why} holds no priority (CR 117.1a / CR 305.1), so it must admit no \
                 CastSpell/PlayLand/ActivateAbility; got {found:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // CR 704 elimination bound (§4.2)
    // -----------------------------------------------------------------------

    /// `n` living players, seat `i` at `lives[i]`. Poison and library stay at their
    /// constructor defaults unless a case sets them.
    fn bound_board(lives: &[i32]) -> GameState {
        let mut state = GameState::new(
            crate::types::format::FormatConfig::free_for_all(),
            lives.len() as u8,
            7,
        );
        for (p, &life) in state.players.iter_mut().zip(lives) {
            p.life = life;
        }
        state
    }

    /// A per-period delta carrying `losses[i]` life loss on seat `i` (0 = no term).
    fn life_loss_delta(losses: &[(u8, i64)]) -> ResourceVector {
        let mut v = ResourceVector::default();
        for &(seat, magnitude) in losses {
            v.life.insert(PlayerId(seat), -magnitude);
        }
        v
    }

    fn slot(index: u8) -> DecisionSlot {
        DecisionSlot {
            source: crate::types::game_state::YieldTarget::AllCopies {
                card_id: CardId(u64::from(index) + 900),
                trigger_description: None,
            },
            index,
        }
    }

    fn slot_magnitudes(magnitudes: &[i64]) -> BTreeMap<DecisionSlot, i64> {
        magnitudes
            .iter()
            .enumerate()
            .map(|(i, &m)| (slot(i as u8), m))
            .collect()
    }

    /// PR-7 Phase 5b (PA-2A(e)) — CR 704.5a: the MAX-vs-SUM fork in `victim_slot`'s magnitude
    /// derivation, whose wrong answer surfaces in playtesting as a wrong elimination bound
    /// rather than as a failure.
    ///
    /// WHY IT NEEDS ITS OWN ROW. ⚠ THE REASON THAT STOOD HERE — *"`victim_slot` is EMPTY on
    /// every trajectory that offers today … all publish `points == 0` … no fixture reaches the
    /// fork"* — IS FALSIFIED, and is replaced rather than softened: once the answer-beat
    /// sampling site in `apply_action` announces the entries a FORCED pre-priority window puts
    /// on the stack, a CR 608.2b `Targets` declaration is announced like any other, so on the
    /// F4 boards `points` carries Torch's `Targets` point and `victim_slot` is NON-EMPTY. This
    /// value is therefore no longer collected into an empty `Vec` and dropped — it reaches
    /// `elimination_bounds` in production, `r1_the_bounded_offer_fires_on_the_real_f4_dump`
    /// re-derives the published bound with a non-zero declared term, and
    /// `b5f_the_declared_term_can_suppress_an_otherwise_legal_offer` measures it flipping a live
    /// offer to `NoNarrowedLegalCount`.
    ///
    /// What those real-dump rows do NOT cover is THIS fork. Both take the magnitude off the
    /// offer's own published `per_cycle.victim_slot`, so they track whatever the derivation
    /// returns instead of discriminating between derivations — swap `max` for `sum` and their
    /// expectations move with it. The max-vs-sum discrimination below is still this row's alone,
    /// and that, not an absent production consumer, is why it stays.
    ///
    /// O4 DERIVE conformance — all THREE legs, not one:
    /// 1. **DERIVED, never compared to a literal.** `m` is bound from the return value and
    ///    asserted only against structural invariants of the input map. No expected magnitude
    ///    is written in arm ⓐ, and the function is CALLED, never re-derived.
    /// 2. **NON-ZERO POPULATION + a fork reach-guard.** `losses` must be non-empty (else every
    ///    ∀ below is vacuously true over an empty seat set) and must hold ≥2 STRICTLY POSITIVE
    ///    entries — which is what makes the sum strictly exceed every single seat's loss. A
    ///    single-seat fixture would make max and sum COINCIDE and the row would pass under
    ///    both derivations, i.e. the degenerate-fixture trap this leg exists to catch.
    /// 3. **POSITIVE CONTROL on the same instrument.** Arms ⓑ and ⓒ assert the OPPOSITE
    ///    outcome (`0`) on the same function, so no constant implementation passes: ⓐ alone
    ///    would be satisfied by anything that always returns a large number, and ⓑ/ⓒ alone by
    ///    anything that always returns zero.
    ///
    /// MAX-vs-SUM DISCRIMINATION, structural rather than numeric. `m` is asserted to be
    /// (i) an UPPER BOUND on every seat's clamped loss and (ii) ATTAINED by a seat that
    /// actually LOST life. Together those two are the definition of `max` with no number
    /// named. Switching the derivation to `sum` keeps (i) and breaks (ii) — no single seat
    /// suffers the total of an asymmetric map (it reports 6; the seats lose 1, 2 and 3).
    /// `first`/`last`/`min` break (i), because arm ⓐ's map is keyed so the first entry is the
    /// proposer's GAIN and clamps to 0. `abs()` breaks (ii) in ⓐ (it reports the proposer's 5,
    /// which no seat lost) and breaks ⓑ outright (it reports 7 where the contract says 0).
    /// Arms ⓑ/ⓒ do NOT kill `sum` — a gain-only map sums to 0 either way; killing `sum` is
    /// arm ⓐ's job alone, which is why ⓐ's asymmetry reach-guard is load-bearing.
    #[test]
    fn worst_seat_life_loss_is_the_max_seat_never_the_sum() {
        // ⓐ asymmetric multi-seat losses PLUS a proposer GAIN larger than any of them.
        let mut v = life_loss_delta(&[(1, 1), (2, 3), (3, 2)]);
        v.life.insert(pid(0), 5);

        let m = v.worst_seat_life_loss();

        // O4(2): non-zero population, and the fixture really does separate max from sum.
        let losses: Vec<i64> = v.life.values().map(|&n| (-n).max(0)).collect();
        assert!(
            losses.iter().filter(|&&l| l > 0).count() >= 2,
            "reach-guard: with fewer than two seats LOSING life the sum equals the max and \
             this row passes under either derivation, proving nothing; got {losses:?}"
        );

        // (i) UPPER BOUND — kills `first`, `last`, `min`, and any non-largest per-seat pick.
        assert!(
            v.life.values().all(|&n| (-n).max(0) <= m),
            "CR 704.5a: a slot aimed at ANY one seat must be charged at least what that seat \
             loses per period, else the bound overstates the legal repetition count and the \
             drive can cross a threshold inside the proposal. m = {m}, losses = {losses:?}"
        );
        // (ii) ATTAINED BY A LOSER — kills `sum` (no seat suffers the total) and kills
        //      `abs()`/gain-inclusive forms (the proposer's +5 is not a loss anyone suffers).
        assert!(
            v.life.iter().any(|(_, &n)| n < 0 && -n == m),
            "the magnitude must be a loss some single seat actually took: `sum` reports a \
             total no seat suffers, and a gains-inclusive derivation reports the proposer's \
             own gain. m = {m}, life = {:?}",
            v.life
        );

        // ⓑ POSITIVE CONTROL — the refusing value IS reachable on a NON-EMPTY map, which is
        // what proves the `(-n).max(0)` clamp is doing the work rather than `unwrap_or(0)`.
        let mut gains_only = ResourceVector::default();
        gains_only.life.insert(pid(0), 7);
        gains_only.life.insert(pid(1), 2);
        assert!(
            !gains_only.life.is_empty(),
            "reach-guard: an EMPTY map returns 0 through `unwrap_or`, a different arm; this \
             control is about the clamp"
        );
        assert_eq!(
            gains_only.worst_seat_life_loss(),
            0,
            "a period in which nobody LOSES life charges nothing — 0 is the contract's \
             no-repetition sentinel, not a fixture number"
        );

        // ⓒ the empty arm, the other way to reach 0 (`max()` yields None).
        assert_eq!(
            ResourceVector::default().worst_seat_life_loss(),
            0,
            "a delta with no life term charges nothing"
        );
    }

    /// CR 704.5a / CR 704.5c / CR 104.3c + CR 121.4 + CR 732.2a: the bound's conventions,
    /// case by case. Every case names the WRONG implementation it kills, so this row is a
    /// battery of discriminators rather than one assertion repeated.
    ///
    /// P-A: the four real fixture bounds (dump B/C/D/F4) are deliberately NOT asserted here.
    /// They are shipped-state values while a real `max_iterations` is computed at the OFFER
    /// beat, dozens of beats later, where the lives differ — a literal measured in a
    /// different state than the one under test. This row asserts the PURE FUNCTION against
    /// hand-supplied lives, which is exactly what a unit row is for; every fixture row
    /// computes its expectation in-test from the offer-beat state.
    #[test]
    fn elimination_bounds_conventions() {
        let no_slots: BTreeMap<DecisionSlot, i64> = BTreeMap::new();

        // (a) life 40, Δ2 ⇒ 19. Kills `floor(life / Δ)` (= 20): at 20 cycles the victim is
        //     at exactly 0 and CR 704.5a has already removed them mid-proposal.
        //     THE ONLY CASE THAT KILLS `floor(life/Δ)` — never drop it.
        assert_eq!(
            life_loss_delta(&[(1, 2)]).elimination_bounds(&bound_board(&[40, 40]), &[], &no_slots),
            19
        );
        // (b) life 39, Δ2 ⇒ 19. Kills `ceil`: 38/2 = 19 exactly, so a ceiling would say 20.
        assert_eq!(
            life_loss_delta(&[(1, 2)]).elimination_bounds(&bound_board(&[40, 39]), &[], &no_slots),
            19
        );
        // (c) poison 0, Δ5 ⇒ 1. Kills `(10 - poison) / Δ` (= 2): CR 704.5c loses at TEN, so
        //     the headroom is 9, and 2 cycles would already have delivered 10.
        {
            let mut v = ResourceVector::default();
            v.poison.insert(PlayerId(1), 5);
            assert_eq!(
                v.elimination_bounds(&bound_board(&[40, 40]), &[], &no_slots),
                1
            );
        }
        // (d) library 8, Δ2 ⇒ 4. Kills `(L - 1) / Δ` (= 3): CR 104.3c/CR 121.4 lose on the
        //     DRAW FROM EMPTY, not on reaching one card, so all 8 cards may legally go.
        {
            let mut state = bound_board(&[40, 40]);
            state.players[1].library = (0..8).map(|i| ObjectId(1000 + i)).collect();
            let mut v = ResourceVector::default();
            v.library_delta.insert(PlayerId(1), -2);
            assert_eq!(v.elimination_bounds(&state, &[], &no_slots), 4);
        }
        // (e) two living at 40 and 12, Δ1 each ⇒ 11. Kills max-instead-of-min.
        assert_eq!(
            life_loss_delta(&[(0, 1), (1, 1)]).elimination_bounds(
                &bound_board(&[40, 12]),
                &[],
                &no_slots
            ),
            11
        );
        // (f) life 5000, Δ1 ⇒ 1000. Kills a missing clamp to MAX_SHORTCUT_CYCLES.
        assert_eq!(
            life_loss_delta(&[(1, 1)]).elimination_bounds(
                &bound_board(&[40, 5000]),
                &[],
                &no_slots
            ),
            crate::game::engine::MAX_SHORTCUT_CYCLES
        );
        // (g) CR 800.4a: an ELIMINATED seat at life 1 must not lower N — PAIRED with the
        //     same seat un-eliminated, which DOES (trap 7: the zero has a non-zero control).
        {
            let mut alive = bound_board(&[40, 1, 40]);
            let delta = life_loss_delta(&[(1, 1), (2, 1)]);
            assert_eq!(
                delta.elimination_bounds(&alive, &[], &no_slots),
                0,
                "control: while that seat is IN the game it pins the bound to 0"
            );
            alive.players[1].is_eliminated = true;
            assert_eq!(
                delta.elimination_bounds(&alive, &[], &no_slots),
                39,
                "an eliminated seat has left the game and constrains nothing"
            );
        }
        // (h) the PROPOSER at life 3 losing 1/cycle ⇒ N <= 2. Kills the deleted
        //     `p == proposer => unbounded` special case: `net_progress_for` reads only the
        //     proposer's mana and life, so it cannot see this at all.
        assert!(
            life_loss_delta(&[(0, 1)]).elimination_bounds(&bound_board(&[3, 40]), &[], &no_slots)
                <= 2
        );
        // (i) the PROPOSER gaining 3 poison/cycle from 0 ⇒ N <= 3. Same defect on the axis
        //     `net_progress_for` is entirely blind to.
        {
            let mut v = ResourceVector::default();
            v.poison.insert(PlayerId(0), 3);
            assert!(v.elimination_bounds(&bound_board(&[40, 40]), &[], &no_slots) <= 3);
        }
        // (j) observed drain on P3 only, lives P1/P2/P3 = 12/13/28, ONE published slot of
        //     magnitude 1 whose legal targets are every opponent ⇒ 11. Kills the
        //     observed-victim-only bound (which returns 27, P3's own headroom): the
        //     declaration may aim the slot at P1 instead. Paired with the untargeted twin.
        {
            let board = bound_board(&[69, 12, 13, 28]);
            let delta = life_loss_delta(&[(3, 1)]);
            let victims = [PlayerId(1), PlayerId(2), PlayerId(3)];
            assert_eq!(
                delta.elimination_bounds(&board, &victims, &slot_magnitudes(&[1])),
                11
            );
            assert_eq!(
                delta.elimination_bounds(&board, &[], &no_slots),
                27,
                "with NO declarable victims only the observed victim constrains the bound"
            );
        }
        // (k) TWO published slots, each magnitude 1, both able to name any opponent ⇒ each
        //     declarable victim's magnitude is 2 ⇒ N == 5. Kills a per-slot (non-aggregated)
        //     bound, which returns 11 and would let a both-slots-on-P1 declaration kill P1
        //     at cycle 6 — inside the proposal.
        {
            let board = bound_board(&[69, 12, 13, 28]);
            let victims = [PlayerId(1), PlayerId(2), PlayerId(3)];
            assert_eq!(
                ResourceVector::default().elimination_bounds(
                    &board,
                    &victims,
                    &slot_magnitudes(&[1, 1])
                ),
                5
            );
        }
        // (l) a 12-life seat at Δ1 ⇒ N == 11, and cycle TWELVE is the killing cycle. The
        //     off-by-one stated as an arithmetic identity, not a comment.
        {
            let board = bound_board(&[40, 12]);
            let n = life_loss_delta(&[(1, 1)]).elimination_bounds(&board, &[], &no_slots);
            assert_eq!(n, 11);
            assert_eq!(
                board.players[1].life as i64 - (i64::from(n) + 1),
                0,
                "cycle N+1 = 12 is the one that reaches 0 life (CR 704.5a)"
            );
        }
        // (m) the dump-C shape: ONE slot of magnitude 1 over every opponent, lives
        //     77/20/20/16, and an OBSERVED loss of 1 on P3 — the same drain, measured twice.
        //     ⇒ N == 7 under the clamped-additive operator. This is the DOUBLE-COUNT case:
        //     `observed` and `S` measure one drain, so charging `0.max(1) + 1 == 2` to P3
        //     over-charges and returns 7 where `max` returned 15. Accepted — it errs toward
        //     REFUSAL, and this repo's convention is fail-closed.
        //     Its untargeted twin stays at 15, so the pair now DISCRIMINATES (7 vs 15) where
        //     under `max` both read 15 — strictly stronger than before.
        //     REVERT-PROBE: restore `observed_life_loss.max(declared_life_magnitude)` ⇒ this
        //     assertion flips 7 → 15 ⇒ FAILS.
        {
            let board = bound_board(&[77, 20, 20, 16]);
            let delta = life_loss_delta(&[(3, 1)]);
            let victims = [PlayerId(1), PlayerId(2), PlayerId(3)];
            assert_eq!(
                delta.elimination_bounds(&board, &victims, &slot_magnitudes(&[1])),
                7,
                "the slot magnitude and the observed loss may be the SAME drain, but this \
                 signature cannot prove it, so both are charged: `0.max(1) + 1 == 2` over \
                 P3's headroom of 15 gives 7"
            );
            assert_eq!(
                delta.elimination_bounds(&board, &[], &no_slots),
                15,
                "untargeted twin: with no published slot the victim arm is never taken, so \
                 the board still bounds at 15 — this is what makes the pair discriminating"
            );
        }
        // (n) lives in its OWN #[test] below — see
        //     `elimination_bounds_mixed_loss_charges_both_terms`. Case (m) above shares
        //     its revert-probe (the same `max` restoration) and panics FIRST, which made
        //     (n)'s documented probe unreachable while they sat in one test fn.
        // (o) NET-GAIN victim — the `.max(0)` clamp's own discriminator. P1 GAINS 2 life
        //     per period (`life_loss_delta` with a NEGATIVE loss), so
        //     `observed_life_loss = -2`, while ONE published slot of magnitude 1 can be
        //     re-aimed at them. The declared slot still constrains: charged magnitude is
        //     `max(-2, 0) + 1 == 1` ⇒ `(10 - 1) / 1 == 9`.
        //
        //     WHY THIS ROW EXISTS: without `.max(0)` the charge is `-2 + 1 == -1`, so
        //     `elimination_bounds`' `narrow` closure never fires for P1 (its guard is
        //     `magnitude > 0`) and the bound stays at MAX_SHORTCUT_CYCLES — the life axis
        //     silently DISARMED on exactly the input that needs it. Asserting the cap here
        //     would lock that fail-open in behind a green test.
        //     REVERT-PROBE: delete `.max(0)` from `elimination_bounds`' `life_magnitude`
        //     operator ⇒ this assertion flips 9 → MAX_SHORTCUT_CYCLES ⇒ FAILS.
        //
        //     NOT bounded by the clamp, disclosed: intra-cycle dips. `self.life` is a
        //     per-period NET delta, so a period draining 5 and lifelinking 7 also reports
        //     `observed = -2` while dipping below `life - 5` mid-cycle. That blindness is a
        //     property of the INPUT and is identical under `max`.
        {
            let board = bound_board(&[40, 10]);
            let delta = life_loss_delta(&[(1, -2)]);
            let victims = [PlayerId(1)];
            // REACH-GUARD (kept from the in-flight row): no P0 term exists, so the value
            // below cannot be the cap-or-not for an unrelated seat's reason.
            assert!(!delta.life.contains_key(&PlayerId(0)));
            assert_eq!(
                delta.elimination_bounds(&board, &victims, &slot_magnitudes(&[1])),
                9,
                "a NET-GAIN victim is still bounded by the re-aimable slot: the observed \
                 term is clamped to 0 and cannot credit against the declared magnitude"
            );
        }
    }

    /// Case (n) of the `elimination_bounds` battery, in its OWN `#[test]` so its
    /// revert-probe is independently REACHABLE: case (m) shares the probe (restore
    /// `observed_life_loss.max(declared_life_magnitude)`) and panics first at 15 vs 7,
    /// so (n)'s assertion never executed under its own stated probe while they were
    /// one test fn.
    ///
    /// MIXED-LOSS regression. The observed drain and the published slot are DIFFERENT
    /// losses (an untargeted 1 plus a re-aimable 1), so P1's true per-period loss is 2
    /// against a headroom of 1 ⇒ NO legal repetition exists. `max` returned 1 here,
    /// offering one iteration that takes P1 from 2 to 0 — an in-proposal elimination
    /// (CR 704.5a), exactly the conditional action CR 732.2a forbids. This is the row
    /// that proves the operator swap is a soundness fix and not a re-labelling.
    ///
    /// REVERT-PROBE: restore `observed_life_loss.max(declared_life_magnitude)` ⇒ the
    /// subject assertion flips 0 → 1 ⇒ FAILS (and the positive control above it still
    /// passes, isolating the flip to the operator).
    #[test]
    fn elimination_bounds_mixed_loss_charges_both_terms() {
        let no_slots: BTreeMap<DecisionSlot, i64> = BTreeMap::new();
        let board = bound_board(&[40, 2]);
        let delta = life_loss_delta(&[(1, 1)]);
        let victims = [PlayerId(1)];
        // PAIRED POSITIVE CONTROL, first: the same board with NO published slot bounds
        // at 1, so the instrument provably returns non-zero here and the 0 below is a
        // VERDICT rather than a dead path.
        assert_eq!(
            delta.elimination_bounds(&board, &[], &no_slots),
            1,
            "positive control: with no published slot the observed drain of 1 over P1's \
             headroom of 1 permits exactly one repetition"
        );
        assert_eq!(
            delta.elimination_bounds(&board, &victims, &slot_magnitudes(&[1])),
            0,
            "MIXED LOSS: an untargeted drain of 1 AND a re-aimable slot of magnitude 1 \
             cost P1 2 per period against a headroom of 1, so no legal repetition \
             exists; `max` returned 1 and permitted an in-proposal elimination"
        );
    }

    /// A conditioned SELF-cost-modifying static (CR 601.2f) on a card sitting in
    /// `zone`, whose condition reads a PROJECTED player resource (life gained this
    /// turn). This is dump-D's Mortality Spear shape: a `ModifyCost` whose `affected`
    /// is `SelfRef`, visible from a never-cast-from zone.
    fn conditioned_self_cost_static_board(zone: Zone, card_id: u64) -> GameState {
        use crate::types::ability::{
            Comparator, PlayerScope, QuantityExpr, QuantityRef, StaticCondition, StaticDefinition,
            TargetFilter,
        };
        use crate::types::mana::ManaCost;
        use crate::types::statics::{CostModifyMode, StaticMode};

        let mut state = GameState::new_two_player(7);
        state.phase = Phase::PreCombatMain;
        let oid = ObjectId(500);
        let mut object = crate::game::game_object::GameObject::new(
            oid,
            CardId(card_id),
            PlayerId(0),
            "Conditioned Cost Static".to_string(),
            zone,
        );
        object.static_definitions = vec![StaticDefinition::new(StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::NoCost,
            spell_filter: None,
            dynamic_count: None,
        })
        .affected(TargetFilter::SelfRef)
        .condition(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        })
        .active_zones(vec![
            Zone::Hand,
            Zone::Stack,
            Zone::Command,
            Zone::Graveyard,
            Zone::Exile,
            Zone::Library,
            Zone::Battlefield,
        ])]
        .into();
        state.objects.insert(oid, object);
        if zone == Zone::Battlefield {
            state.battlefield.push_back(oid);
        }
        state
    }

    /// X4-1 — CR 601.2f. A conditioned SELF-cost modifier on a card the window
    /// provably never casts cannot modify any cost paid inside the window, so its
    /// condition's projected read is not an observation of the loop. Asserted across
    /// FOUR never-cast-from zones, each with its own positive control: the UNSCOPED
    /// call (`cast_card_ids: None`, no proof) still vetoes in all four.
    ///
    /// REVERT-PROBES:
    /// * delete the `continue` ⇒ all four scoped assertions FAIL.
    /// * drop the `ModifyCost` conjunct ⇒ the `Continuous` sibling below is wrongly
    ///   relieved ⇒ FAILS.
    /// * drop the `Some(TargetFilter::SelfRef)` conjunct ⇒ the affects-others sibling
    ///   below is wrongly relieved ⇒ FAILS.
    #[test]
    fn a_conditioned_cost_static_in_a_zone_the_window_never_casts_from_does_not_observe() {
        use crate::types::ability::TargetFilter;
        use crate::types::statics::StaticMode;

        // A card id the window's driving sequence does NOT contain.
        let never_cast = [CardId(999)];

        for zone in [Zone::Library, Zone::Hand, Zone::Graveyard, Zone::Exile] {
            let state = conditioned_self_cost_static_board(zone, 500);

            // POSITIVE CONTROL for this zone: with NO proof the firewall still vetoes.
            assert!(
                fire_time_conditions_read_projected_resource(&state),
                "X4-1 control ({zone:?}): `cast_card_ids: None` is NO PROOF, so the \
                 conservative veto must be preserved"
            );

            let scope = LoopWindowScope {
                phase_invariant: None,
                sole_driver: None,
                pinned: None,
                cast_card_ids: Some(&never_cast),
                period: None,
            };
            assert!(
                !fire_time_conditions_read_projected_resource_scoped(&state, scope),
                "X4-1 ({zone:?}): CR 601.2f — the window provably never casts this card, \
                 so its self-cost modifier cannot modify any cost paid inside the window"
            );
        }

        // NON-BLANKET siblings, both in the SAME never-cast-from zone with the SAME
        // proof: only a `ModifyCost` + `SelfRef` static may be relieved.
        let mut not_modify_cost = conditioned_self_cost_static_board(Zone::Library, 500);
        {
            let obj = not_modify_cost.objects.get_mut(&ObjectId(500)).unwrap();
            let mut defs: Vec<_> = obj.static_definitions.iter_all().cloned().collect();
            defs[0].mode = StaticMode::Continuous;
            obj.static_definitions = defs.into();
        }
        let scope = LoopWindowScope {
            phase_invariant: None,
            sole_driver: None,
            pinned: None,
            cast_card_ids: Some(&never_cast),
            period: None,
        };
        assert!(
            fire_time_conditions_read_projected_resource_scoped(&not_modify_cost, scope),
            "X4-1: a NON-`ModifyCost` static with the same condition is NOT a cost \
             modifier, so CR 601.2f's argument does not apply — keep vetoing"
        );

        let mut affects_others = conditioned_self_cost_static_board(Zone::Library, 500);
        {
            let obj = affects_others.objects.get_mut(&ObjectId(500)).unwrap();
            let mut defs: Vec<_> = obj.static_definitions.iter_all().cloned().collect();
            defs[0].affected = Some(TargetFilter::Any);
            obj.static_definitions = defs.into();
        }
        assert!(
            fire_time_conditions_read_projected_resource_scoped(&affects_others, scope),
            "X4-1: a cost modifier affecting OTHER objects can modify a cost paid in the \
             window even though its own card is never cast — keep vetoing"
        );
    }

    /// X4-2 — the matched negative that kills the lazy-but-unsound X4. The SAME static
    /// on a card whose id IS in the window's cast set keeps vetoing: the window does
    /// cast it, so its self-cost modifier does apply inside the window.
    ///
    /// REVERT-PROBE: replace the guard with a bare `ModifyCost ⇒ continue` ⇒ FAILS.
    #[test]
    fn a_cost_static_on_a_card_the_loop_recasts_still_vetoes() {
        let state = conditioned_self_cost_static_board(Zone::Hand, 500);
        let recast = [CardId(500)];
        let scope = LoopWindowScope {
            phase_invariant: None,
            sole_driver: None,
            pinned: None,
            cast_card_ids: Some(&recast),
            period: None,
        };
        assert!(
            fire_time_conditions_read_projected_resource_scoped(&state, scope),
            "X4-2: CR 601.2f — the window DOES cast this card, so its conditioned \
             self-cost modifier is read inside the window and must keep vetoing"
        );

        // PAIRED POSITIVE (same board, one variable — the cast set): a different id is
        // relieved, so the assertion above is not a constant.
        let other = [CardId(501)];
        let relieved_scope = LoopWindowScope {
            phase_invariant: None,
            sole_driver: None,
            pinned: None,
            cast_card_ids: Some(&other),
            period: None,
        };
        assert!(
            !fire_time_conditions_read_projected_resource_scoped(&state, relieved_scope),
            "X4-2 paired positive: the identical board with the card OUT of the cast set \
             IS relieved — the only variable is membership"
        );
    }

    /// X4-5 — THE `:1038` BINDING EXPRESSION, pinned through the PRODUCTION entry point.
    ///
    /// X4-4 tests [`window_cast_card_ids`] directly and X4-1 uses a hand-built scope, so
    /// neither pins the premise *"conjunct (5) derives `cast_card_ids` from
    /// `window_cast_card_ids(current)`, fail-closed"*. Measured: writing
    /// `Some(cast_ids.as_deref().unwrap_or(&[]))` at that binding re-opens the fail-open
    /// and every other X4 row still passes. This row closes that gap: it drives
    /// [`loop_states_cover_modulo_growth`] — the real 2-arg production predicate, which
    /// `loop_check.rs` calls with NO non-empty-sequence precondition — over a covering
    /// frame pair carrying a library-visible conditioned self-cost static.
    ///
    /// MATCHED PAIR, one variable (the recorded driving sequence):
    /// * half A — EMPTY sequence ⇒ no proof ⇒ the guard is fail-closed ⇒ conjunct (5)
    ///   rejects the cover.
    /// * half B — a one-entry sequence naming a DIFFERENT card ⇒ proof ⇒ relieved ⇒ the
    ///   cover holds.
    ///
    /// REVERT-PROBES, both measured to flip half A:
    /// * bind `Some(cast_ids.as_deref().unwrap_or(&[]))` instead of `cast_ids.as_deref()`.
    /// * make `window_cast_card_ids` return `Some(ids)` unconditionally.
    #[test]
    fn empty_sequence_keeps_the_projected_cost_veto_through_the_production_cover() {
        use crate::types::ability::{
            Comparator, PlayerScope, QuantityExpr, QuantityRef, StaticCondition, StaticDefinition,
            TargetFilter,
        };
        use crate::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};
        use crate::types::mana::ManaCost;
        use crate::types::statics::{CostModifyMode, StaticMode};

        const STATIC_CARD: CardId = CardId(90);
        const DRIVER_CARD: CardId = CardId(64);

        // A library-resident conditioned SELF-cost static, added identically to BOTH
        // frames so it cannot perturb the board-equality conjuncts (1)-(4).
        let add_static = |state: &mut GameState| {
            let oid = ObjectId(700);
            let mut object = crate::game::game_object::GameObject::new(
                oid,
                STATIC_CARD,
                PlayerId(0),
                "Library Cost Static".to_string(),
                Zone::Library,
            );
            object.static_definitions = vec![StaticDefinition::new(StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount: ManaCost::NoCost,
                spell_filter: None,
                dynamic_count: None,
            })
            .affected(TargetFilter::SelfRef)
            .condition(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
            .active_zones(vec![Zone::Library, Zone::Hand, Zone::Stack])]
            .into();
            state.objects.insert(oid, object);
        };

        // REACH-GUARD: the untouched pair covers, so any `false` below is caused by the
        // static and not by an upstream conjunct.
        let (bare_prior, bare_current) = cover_base();
        assert!(
            loop_states_cover_modulo_growth(&bare_prior, &bare_current),
            "reach-guard: the base frame pair must COVER, else conjuncts (1)-(4) dominate"
        );

        // ── half A: empty driving sequence ⇒ NO PROOF ⇒ the veto survives ──
        let (mut prior, mut current) = cover_base();
        add_static(&mut prior);
        add_static(&mut current);
        assert!(
            current.last_loop_action_sequence.is_empty(),
            "half A precondition: no recorded driving sequence"
        );
        assert!(
            !loop_states_cover_modulo_growth(&prior, &current),
            "half A: an EMPTY `last_loop_action_sequence` proves NOTHING about what the \
             window casts, so the conditioned self-cost static must keep its veto and \
             conjunct (5) must reject. `Some(&[])` here would assert `this window casts \
             nothing` and relieve every such static — the forbidden direction."
        );

        // ── half B: a real one-entry sequence naming a DIFFERENT card ⇒ relieved ──
        let ctx = LoopActionContext {
            card_id: DRIVER_CARD,
            controller: PlayerId(0),
            action: LoopAction::Recast {
                from_zone: Zone::Hand,
                uses_buyback: BuybackUsage::Used,
            },
            convoke: None,
            pins: Vec::new(),
        };
        prior.last_loop_action_sequence = vec![ctx.clone()];
        current.last_loop_action_sequence = vec![ctx];
        assert_ne!(DRIVER_CARD, STATIC_CARD);
        assert!(
            loop_states_cover_modulo_growth(&prior, &current),
            "half B: with the cast set PROVEN and the static's card outside it, CR 601.2f \
             says the modifier cannot apply inside the window ⇒ the cover holds"
        );
    }

    /// X4-4 — [`window_cast_card_ids`]'s emptiness contract, called DIRECTLY so no cover
    /// conjunct can dominate it. An empty `last_loop_action_sequence` means NO RECORDED
    /// PROOF, not "this window casts nothing": `Some(vec![])` would assert the latter
    /// and relieve EVERY conditioned self-cost static.
    ///
    /// REVERT-PROBE: replace `if ids.is_empty() { None } else { Some(ids) }` with a bare
    /// `Some(ids)` ⇒ assertion (1) FAILS while (2) still passes ⇒ the probe is isolated
    /// to the emptiness test.
    ///
    /// ⛔ WHAT THIS ROW DOES NOT CLAIM: it does not assert "and the X4-1 static still
    /// vetoes". That half is carried by X4-1's own UNSCOPED arm
    /// (`LoopWindowScope::unproven()` has `cast_card_ids: None`, measured `true` on all
    /// four zones). The end-to-end property is the COMPOSITION of two directly-tested
    /// seams — X4-4 (`empty ⇒ None`) and X4-1 (`None ⇒ veto`) — and is stated as a
    /// composition, not asserted as a third row.
    #[test]
    fn empty_loop_action_sequence_proves_nothing_about_casting() {
        use crate::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

        let mut state = GameState::new_two_player(7);
        assert!(state.last_loop_action_sequence.is_empty());
        assert_eq!(
            window_cast_card_ids(&state, None),
            None,
            "(1) an empty driving sequence is NO PROOF — `Some(vec![])` would assert \
             `this window casts nothing` and relieve every conditioned self-cost static"
        );

        // (2) PAIRED POSITIVE. `action` is not load-bearing here (the derivation reads
        // only `card_id`); `Recast` is the cheapest to construct.
        state.last_loop_action_sequence = vec![LoopActionContext {
            card_id: CardId(64),
            controller: PlayerId(0),
            action: LoopAction::Recast {
                from_zone: Zone::Hand,
                uses_buyback: BuybackUsage::Used,
            },
            convoke: None,
            pins: Vec::new(),
        }];
        assert_eq!(
            window_cast_card_ids(&state, None),
            Some(vec![CardId(64)]),
            "(2) a one-entry sequence yields exactly that card id"
        );
    }

    /// X4-5 — [`window_cast_card_ids`]'s PROPOSER SCOPING (CR 732.2a), the sibling contract to
    /// X4-4's emptiness one, called DIRECTLY for the same anti-domination reason.
    ///
    /// A recorded period is evidence about the seat that recorded it. Once the bounded mint's
    /// step (1b) went seat-relative, a certification could be taken with a FOREIGN period sitting
    /// in state — and an unscoped read would then let an OPPONENT'S choice of which card to
    /// activate decide which conditioned self-cost static gets relieved for THIS proposer.
    ///
    /// THREE-WAY AND EACH ARM IS LOAD-BEARING, so no constant implementation passes:
    /// * `None` (the proposer-less 2-arg entry) ⇒ unscoped, byte-identical to pre-fix. Dropping
    ///   the `Option` guard — the UNCONDITIONAL-MATCH form `if state.loop_period_controller() !=
    ///   proposer { return None; }` — refuses the unbound container and FAILS (1); this is the arm
    ///   that protects `loop_check`'s object-growth detection covers. (MEASURED, and it corrects
    ///   this row's own earlier claim: the `is_some`-instead-of-`is_some_and` swap does NOT fail
    ///   (1) — with `proposer == None` it never returns early — it fails (2), by refusing the
    ///   seat that DID record the period.)
    /// * `Some(owner)` ⇒ proof. An always-`None` implementation FAILS (2), as does the `is_some`
    ///   swap above.
    /// * `Some(other)` ⇒ no proof. The pre-fix unscoped implementation FAILS (3).
    ///
    /// (4) pins the fail-closed homogeneity clause: a two-seat run is nobody's period, so it is
    /// proof for NEITHER seat — an implementation testing only `seq[0].controller` FAILS it.
    #[test]
    fn a_foreign_driving_period_proves_nothing_about_this_proposers_casting() {
        use crate::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

        let owner = PlayerId(0);
        let other = PlayerId(1);
        let step = |controller: PlayerId, card_id: CardId| LoopActionContext {
            card_id,
            controller,
            action: LoopAction::Recast {
                from_zone: Zone::Hand,
                uses_buyback: BuybackUsage::Used,
            },
            convoke: None,
            pins: Vec::new(),
        };

        let mut state = GameState::new_two_player(7);
        state.last_loop_action_sequence = vec![step(owner, CardId(64))];

        assert_eq!(
            window_cast_card_ids(&state, None),
            Some(vec![CardId(64)]),
            "(1) an UNBOUND container (the proposer-less 2-arg entry `loop_check` uses) reads \
             the period unscoped — `is_some_and`, not `is_some`, or the object-growth detection \
             covers lose their relief"
        );
        assert_eq!(
            window_cast_card_ids(&state, Some(owner)),
            Some(vec![CardId(64)]),
            "(2) the seat that RECORDED the period is proved by it"
        );
        assert_eq!(
            window_cast_card_ids(&state, Some(other)),
            None,
            "(3) CR 732.2a: another seat's independent activation describes no sequence THIS \
             proposer takes, so it is no proof about this window's cast set — relieving on it \
             would hand an opponent the choice of which soundness relief applies"
        );

        // (4) the fail-closed homogeneity clause: nobody's period.
        state.last_loop_action_sequence = vec![step(owner, CardId(64)), step(other, CardId(90))];
        assert_eq!(
            (
                window_cast_card_ids(&state, Some(owner)),
                window_cast_card_ids(&state, Some(other)),
            ),
            (None, None),
            "(4) a heterogeneous run belongs to no seat, so it proves nothing for EITHER — \
             reading only `seq[0].controller` would wrongly prove it for the first"
        );
    }

    /// X4-3 — the REAL 4-player Dina/Conqueror capture (`dina_conqueror_4p.json.gz`),
    /// loaded through the production restore chokepoint
    /// `PersistedGameState::into_game_state`. It carries dump-D obj 90 **Mortality
    /// Spear** in P0's LIBRARY: a conditioned `ModifyCost` static whose `affected` is
    /// `SelfRef` and whose `active_zones` make it visible from the library — exactly
    /// X4's subject, on a board nobody synthesized.
    ///
    /// MEASURED on this board (which is what makes the flip attributable): the Spear's
    /// static is the **ONLY** projected-resource-reading fire-time surface in the entire
    /// dump — 1 static, 0 trigger conditions — so the unscoped `true` is caused by it
    /// alone and the scoped `false` cannot come from anything else.
    ///
    /// ⛔ NO OFFER CLAIM IS MADE HERE. 2b's deliverable-visible acceptance is that it
    /// changes nothing observable (an empty `combo-verify` rowdiff); this row asserts the
    /// SEAM, not a shortcut offer.
    ///
    /// REVERT-PROBE: delete X4's `continue` in
    /// `fire_time_conditions_read_projected_resource_scoped` block (iii-static) ⇒ the
    /// scoped half returns `true` ⇒ FAILS. Both directions are probed in this one row:
    /// the unscoped call is the positive control for the scoped call.
    #[test]
    fn dina_untargeted_drain_4p_cover_is_not_vetoed_by_a_library_cost_static() {
        use crate::types::ability::TargetFilter;
        use crate::types::statics::StaticMode;
        use std::io::Read;

        let gz = include_bytes!("../../tests/fixtures/dina_conqueror_4p.json.gz");
        let mut json = String::new();
        flate2::read::GzDecoder::new(&gz[..])
            .read_to_string(&mut json)
            .expect("fixture .json.gz must inflate to UTF-8 JSON");
        let envelope: serde_json::Value =
            serde_json::from_str(&json).expect("dump envelope parses as JSON");
        // Cross the dump through the PRODUCTION decoder rather than a bare `GameState`
        // decode wrapped in `Raw`: `PersistedGameState`'s own `Deserialize` runs
        // `reject_legacy_raw_prompt_authority` and `decode_persisted_resolution_state`
        // first, so this row exercises the chokepoint the server's `from_persisted` and
        // WASM's `decode_restored_game_state` actually funnel through.
        // `.expect(..)`, not `?`: `into_game_state` returns `GameState`, not `Result`.
        let state = serde_json::from_value::<crate::types::game_state::PersistedGameState>(
            envelope["gameState"].clone(),
        )
        .expect("gameState deserializes through the production decoder")
        .into_game_state();

        // ── reach-guards: the X4 subject really is present, in a never-cast-from zone ──
        let spear = state
            .objects
            .get(&ObjectId(90))
            .expect("dump-D obj 90 is present");
        assert_eq!(spear.name, "Mortality Spear");
        assert_eq!(
            spear.zone,
            Zone::Library,
            "the subject is visible from a zone the window never casts from"
        );
        let subjects: Vec<_> = state
            .objects
            .values()
            .filter(|o| {
                o.static_definitions.iter_all().any(|d| {
                    matches!(d.mode, StaticMode::ModifyCost { .. })
                        && matches!(d.affected, Some(TargetFilter::SelfRef))
                        && d.condition.is_some()
                })
            })
            .map(|o| (o.id, o.name.clone(), o.zone))
            .collect();
        assert_eq!(
            subjects.len(),
            1,
            "ATTRIBUTION reach-guard: the dump must carry EXACTLY ONE conditioned \
             self-cost static, else the flip below is not attributable to it; got \
             {subjects:?}"
        );

        // ── POSITIVE CONTROL: with no proof, the real board vetoes ──
        assert!(
            fire_time_conditions_read_projected_resource(&state),
            "X4-3 control: `cast_card_ids: None` is NO PROOF, so the real 4p board must \
             keep its conservative veto"
        );

        // ── the window provably casts something else (any id but the Spear's) ──
        let spear_card = spear.card_id;
        let cast = [CardId(spear_card.0 + 1)];
        assert!(!cast.contains(&spear_card));
        let scope = LoopWindowScope {
            phase_invariant: None,
            sole_driver: None,
            pinned: None,
            cast_card_ids: Some(&cast),
            period: None,
        };
        assert!(
            !fire_time_conditions_read_projected_resource_scoped(&state, scope),
            "X4-3: CR 601.2f — the window provably never casts Mortality Spear, so its \
             library-visible self-cost modifier cannot modify any cost paid inside the \
             window and must not veto the cover. \
             ⛔ PRE-REGISTERED FAILURE BRANCH: if this fails, name the NEXT rejecting \
             surface (the measurement above says the Spear is the only one) and its call \
             count in the PR body, and STOP — do not widen the guard."
        );

        // ── non-blanket: the SAME board with the Spear IN the cast set keeps vetoing ──
        let recast = [spear_card];
        let recast_scope = LoopWindowScope {
            phase_invariant: None,
            sole_driver: None,
            pinned: None,
            cast_card_ids: Some(&recast),
            period: None,
        };
        assert!(
            fire_time_conditions_read_projected_resource_scoped(&state, recast_scope),
            "X4-3 matched negative: a window that DOES cast the Spear keeps its veto — \
             the only variable is cast-set membership"
        );
    }

    /// A Saproling creature token, the fodder class 2c's rows exclude or match.
    fn saproling_class_member(state: &mut GameState) -> ObjectId {
        let oid = ObjectId(800);
        let mut object = crate::game::game_object::GameObject::new(
            oid,
            CardId(0),
            PlayerId(0),
            "Saproling".to_string(),
            Zone::Battlefield,
        );
        object.card_types.core_types = vec![CoreType::Creature];
        object.card_types.subtypes = vec!["Saproling".to_string()];
        object.color = vec![crate::types::mana::ManaColor::Green];
        object.is_token = true;
        state.objects.insert(oid, object);
        state.battlefield.push_back(oid);
        oid
    }

    /// The ability source the ledger read belongs to (the observer permanent).
    fn ledger_observer_source(state: &mut GameState) -> ObjectId {
        let oid = ObjectId(801);
        let mut object = crate::game::game_object::GameObject::new(
            oid,
            CardId(801),
            PlayerId(0),
            "BBFU10 Bystander".to_string(),
            Zone::Battlefield,
        );
        object.card_types.core_types = vec![CoreType::Creature];
        state.objects.insert(oid, object);
        state.battlefield.push_back(oid);
        oid
    }

    /// Parse `oracle` and hand back the first trigger's `execute` body — the exact
    /// `AbilityDefinition` block (1) scans.
    fn trigger_execute_from_oracle(oracle: &str) -> crate::types::ability::AbilityDefinition {
        let parsed = crate::parser::parse_oracle_text(
            oracle,
            "BBFU10 Bystander",
            &[],
            &["Creature".to_string()],
            &[],
        );
        parsed
            .triggers
            .first()
            .and_then(|t| t.execute.as_deref())
            .cloned()
            .expect("the constructed oracle must parse a trigger execute body")
    }

    /// K4-N3 + NW-2 — the CR 608.2i + CR 608.2j exclusion predicate, SEVEN arms, both polarities on
    /// every axis. Each `false` arm is paired with a `true` arm in the same row, so a
    /// constant implementation fails at least one.
    ///
    /// REVERT-PROBES, one per conjunct (each named with the arm it flips):
    /// * (ii) disable conjunct (c) ⇒ verbatim Park Heights Pegasus is wrongly relieved ⇒
    ///   (ii) FAILS. (a) is measured to PASS for Pegasus, so (c) is the only conjunct
    ///   carrying its refusal.
    /// * (iii) drop conjunct (0) ⇒ FAILS. This is NW-2: the scan destructures
    ///   `activation_restrictions: _` (ability_scan.rs:4238), so conjunct (a) returns
    ///   `false` and the predicate would wrongly return `true` with a class-MATCHING
    ///   `ActivationRestriction::RequiresCondition` on the very def being relieved.
    /// * (iv) replace conjunct (b)'s `_ => false` with `_ => true` ⇒ FAILS.
    /// * (v) drop conjunct (a) ⇒ FAILS.
    /// * (vi) flip the matcher's `FilterProp` fail-closed `_ => false`
    ///   (restrictions.rs:515) to `_ => true` ⇒ the `FaceDown` filter now matches the
    ///   record ⇒ relief is refused ⇒ FAILS.
    /// * (vii) swap conjunct (c)'s call to `matches_target_filter`, or drop
    ///   `Some(source.id)` ⇒ the verdict diverges from the resolver's ⇒ FAILS.
    #[test]
    fn ledger_exclusion_is_precise_and_fail_closed() {
        use crate::types::ability::{
            AbilityCondition, Comparator, FilterProp, PlayerScope, QuantityExpr, QuantityRef,
            TargetFilter, TypeFilter, TypedFilter,
        };

        let mut state = GameState::new_two_player(7);
        state.phase = Phase::PreCombatMain;
        let member = saproling_class_member(&mut state);
        let source_id = ledger_observer_source(&mut state);
        let source = state.objects[&source_id].clone();

        let ledger_condition = |filter: TargetFilter| AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::BattlefieldEntriesThisTurn {
                    player: PlayerScope::Controller,
                    filter,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 2 },
        };
        let typed = |t: TypeFilter, props: Vec<FilterProp>| {
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![t],
                controller: None,
                properties: props,
            })
        };

        // The fixture-C shape: a ledger read in `execute.condition` whose body is a plain
        // fixed draw, so `condition` is the def's ONLY sibling read.
        const FIXTURE_C: &str = "Whenever this creature deals damage to a player, draw a card if you had two or more artifacts enter the battlefield under your control this turn.";
        let mut exec_artifact = trigger_execute_from_oracle(FIXTURE_C);
        // Reach-guard: the parsed shape is the one conjunct (b) matches.
        assert!(
            matches!(
                exec_artifact.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::BattlefieldEntriesThisTurn { .. }
                    },
                    rhs: QuantityExpr::Fixed { .. },
                    ..
                })
            ),
            "reach-guard: fixture C must parse into the single-level shape conjunct (b) \
             accepts, else every arm below tests conjunct (b)'s `_` arm instead; got {:?}",
            exec_artifact.condition
        );

        // ── (i) TRUE — an Artifact ledger filter provably cannot count a Saproling ──
        assert!(
            execute_ledger_condition_provably_excludes_class(
                &exec_artifact,
                &state,
                member,
                &source
            ),
            "(i) CR 608.2j: `Typed{{Artifact}}` cannot count a creature token, so the \
             read's value is invariant across the loop's growth"
        );

        // ── (ii) FALSE — verbatim Park Heights Pegasus GENUINELY matches ──
        let db = crate::test_support::shared_card_db();
        let pegasus = db
            .face_index
            .get("park heights pegasus")
            .expect("Park Heights Pegasus is in the integration card fixtures");
        assert_eq!(pegasus.triggers.len(), 1, "(ii) reach-guard: one trigger");
        let pegasus_exec = pegasus.triggers[0]
            .execute
            .as_deref()
            .expect("(ii) reach-guard: the trigger carries an execute body")
            .clone();
        assert!(
            !execute_ledger_condition_provably_excludes_class(
                &pegasus_exec,
                &state,
                member,
                &source
            ),
            "(ii) the printed card's `Typed{{Creature}}` ledger filter DOES count a \
             Saproling creature token, so relief must be REFUSED — conjunct (c) is the \
             only conjunct carrying this refusal"
        );

        // ── (iii) NW-2: FALSE when the def carries an activation restriction ──
        // The firewall never reads that field, so this must be a PROGRAMMATIC fixture:
        // measured, 0 trigger `execute` bodies in the card pool carry one (positive
        // control: 3195 on `abilities[]`), so no parser path can build it.
        let mut restricted = exec_artifact.clone();
        restricted
            .activation_restrictions
            .push(ActivationRestriction::RequiresCondition {
                condition: Some(crate::types::ability::ParsedCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::BattlefieldEntriesThisTurn {
                            player: PlayerScope::Controller,
                            filter: TargetFilter::Typed(TypedFilter::creature()),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 2 },
                }),
            });
        assert!(
            !execute_ledger_condition_provably_excludes_class(&restricted, &state, member, &source),
            "(iii) NW-2: the two defs differ in EXACTLY that one field — the scan is blind \
             to it (`activation_restrictions: _`), so conjunct (0) is the only closure for \
             a class-MATCHING activation restriction on the def being relieved"
        );

        // ── (iv) FALSE when the condition is a COMPOUND (conjunct b's `_` arm) ──
        let mut compound = exec_artifact.clone();
        compound.condition = Some(AbilityCondition::And {
            conditions: vec![ledger_condition(typed(TypeFilter::Artifact, vec![]))],
        });
        assert!(
            !execute_ledger_condition_provably_excludes_class(&compound, &state, member, &source),
            "(iv) conjunct (b) is single-level with `_ => false`: an `And`/`Or`/`Not` \
             wrapper keeps the veto rather than recursing without a totality obligation"
        );

        // ── (v) FALSE when a SECOND sibling read hides in the effect body (conjunct a) ──
        const FIXTURE_TWO_READS: &str = "Whenever this creature deals damage to a player, draw a card for each creature you control if you had two or more artifacts enter the battlefield under your control this turn.";
        let two_reads = trigger_execute_from_oracle(FIXTURE_TWO_READS);
        assert!(
            !execute_ledger_condition_provably_excludes_class(&two_reads, &state, member, &source),
            "(v) conjunct (a): with the `condition` cleared the def STILL reads the board, \
             so `condition` is not its sole sibling source and no exclusion proof about \
             `condition` alone can license relief"
        );

        // ── (vi) TRUE for an UNEVALUABLE filter — invariance under growth ──
        // `FilterProp::FaceDown` is live (1/60, tunnel tipster) and outside
        // `ledger_filter_is_evaluable`'s allow-list. The matcher answers `false` for
        // every record, so each new class member adds 0 TO THE TALLY WHATEVER THE
        // TALLY'S VALUE IS — which is all soundness needs. Do NOT restate this as "the
        // tally is a constant 0": under `Or` an unsupported leaf yields a SILENT PARTIAL
        // COUNT instead (restrictions.rs:519-526), and `Or` is live 4/60.
        exec_artifact.condition = Some(ledger_condition(typed(
            TypeFilter::Creature,
            vec![FilterProp::FaceDown],
        )));
        assert!(
            execute_ledger_condition_provably_excludes_class(
                &exec_artifact,
                &state,
                member,
                &source
            ),
            "(vi) an unanswerable filter is relieved because relief is CORRECT here: the \
             same matcher the resolver asks answers `false` for the new member, so the \
             tally is invariant under growth"
        );

        // ── (vii) ARG-EQUIVALENCE PIN: the predicate's verdict IS the resolver's ──
        let creature_filter = typed(TypeFilter::Creature, vec![]);
        exec_artifact.condition = Some(ledger_condition(creature_filter.clone()));
        let record =
            crate::game::restrictions::battlefield_entry_record_for(&state.objects[&member]);
        let resolver_shaped = !crate::game::restrictions::battlefield_entry_matches_filter(
            &record,
            &creature_filter,
            source.controller,
            &state.all_creature_types,
            Some(source.id),
        );
        assert_eq!(
            execute_ledger_condition_provably_excludes_class(
                &exec_artifact,
                &state,
                member,
                &source
            ),
            resolver_shaped,
            "(vii) ⛔ ARG-EQUIVALENCE PIN: conjunct (c) must ask the SAME matcher the \
             CR 608.2i resolver asks (`QuantityRef::BattlefieldEntriesThisTurn`), with \
             the ability CONTROLLER for `player` and `Some(source.id)` for the `Another` \
             exclusion. Swapping in `matches_target_filter`, or dropping `source.id`, \
             makes the two verdicts diverge and this arm fails."
        );
        assert!(
            !resolver_shaped,
            "(vii) reach-guard: the resolver-shaped call must answer MATCH for a creature \
             filter vs a creature token, else the equality above is vacuously true on two \
             `true`s"
        );

        // ── (viii) ARG-EQUIVALENCE PIN, the `Some(source.id)` ARGUMENT specifically ──
        // `FilterProp::Another` is `source_id.is_some_and(|s| record.object_id != s)`.
        // The class member is NOT the ability source, so with the source id supplied the
        // matcher answers MATCH and relief must be REFUSED. Dropping `Some(source.id)` to
        // `None` makes `Another` answer `false`, the filter stops matching, and relief is
        // wrongly GRANTED — so this arm flips to FAIL on exactly that one-argument change,
        // which arms (i)-(vii) cannot see (none of their filters carries a `FilterProp`).
        exec_artifact.condition = Some(ledger_condition(typed(
            TypeFilter::Creature,
            vec![FilterProp::Another],
        )));
        assert_ne!(
            member, source.id,
            "(viii) reach-guard: the class member must NOT be the ability source, else \
             `Another` excludes it for the wrong reason"
        );
        assert!(
            !execute_ledger_condition_provably_excludes_class(
                &exec_artifact,
                &state,
                member,
                &source
            ),
            "(viii) with `Some(source.id)` supplied, `Typed{{Creature,[Another]}}` MATCHES \
             the class member (it is another object), so relief must be refused. Dropping \
             that argument silently changes the verdict — the ARG-EQUIVALENCE PIN."
        );

        // ── (ix) conjunct (b)'s `rhs: Fixed` REQUIREMENT, pinned ──
        // The shape match reads `lhs` and conjunct (c) only interrogates the lhs filter, so
        // an rhs-position board read would go completely unexamined. Requiring `rhs: Fixed`
        // is what forecloses that: a comparison whose rhs is itself a `QuantityRef` falls to
        // conjunct (b)'s `_` arm and KEEPS the veto. Dropping the requirement flips this
        // arm — no other arm carries a non-`Fixed` rhs, and conjunct (a) cannot catch it
        // (the clone-and-rescan clears the whole `condition`, rhs included).
        exec_artifact.condition = Some(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::BattlefieldEntriesThisTurn {
                    player: PlayerScope::Controller,
                    filter: typed(TypeFilter::Artifact, vec![]),
                },
            },
            comparator: Comparator::LE,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: typed(TypeFilter::Creature, vec![]),
                },
            },
        });
        assert!(
            !execute_ledger_condition_provably_excludes_class(
                &exec_artifact,
                &state,
                member,
                &source
            ),
            "(ix) an rhs-position board read is never interrogated by conjunct (c), so \
             conjunct (b)'s `rhs: Fixed` requirement must keep the veto"
        );
    }

    /// ITEM B-1 — relief requires the ledger filter to provably exclude **EVERY** member
    /// of the growing class, not one representative (CR 603.6a). The one-representative
    /// test was unsound in the ACCEPTING direction: fodder equivalence
    /// (`object_content_eq`) does NOT compare `card_types`, so two members of one class
    /// can differ on exactly the axis a `Typed{Artifact}` ledger filter reads.
    ///
    /// FIXTURE ORDERING IS LOAD-BEARING. The EXCLUDING member is `ObjectId(800)` (the
    /// Saproling creature token) and the divergent NON-excluding member is `ObjectId(802)`
    /// (an artifact token), so `800` is the min by `ObjectId` AND the untapped-first
    /// collapse key's winner. The deleted production collapse
    /// (`min_by_key(|id| (tapped, *id))`) therefore picks the EXCLUDING member, which is
    /// what makes the revert-probe flip on every run rather than half of them.
    ///
    /// REVERT-PROBE (deterministic): replace
    /// `!members.is_empty() && members.iter().all(f)` in the ledger gate with the
    /// single-representative collapse this edit removes —
    /// `members.iter().min_by_key(|id| (state.objects[id].tapped, **id)).is_some_and(f)` —
    /// ⇒ only `ObjectId(800)` is consulted, it excludes, relief is granted, the veto
    /// disappears ⇒ this assertion FAILS. (`members.iter().min().is_some_and(f)` is
    /// equivalent here because both members are untapped, asserted below.)
    #[test]
    fn ledger_exclusion_requires_every_class_member() {
        let mut state = GameState::new_two_player(7);
        state.phase = Phase::PreCombatMain;

        // The representative the old collapse would have chosen: a CREATURE token, which a
        // `Typed{Artifact}` ledger filter provably cannot count.
        let excluding = saproling_class_member(&mut state); // ObjectId(800)

        // A second member of the SAME fodder class that diverges on `core_types` — a
        // field `object_content_eq` does not compare — and which the SAME filter DOES
        // count.
        let divergent = ObjectId(802);
        {
            let mut object = crate::game::game_object::GameObject::new(
                divergent,
                CardId(0),
                PlayerId(0),
                "Saproling".to_string(),
                Zone::Battlefield,
            );
            object.card_types.core_types = vec![CoreType::Artifact];
            object.color = vec![crate::types::mana::ManaColor::Green];
            object.is_token = true;
            state.objects.insert(divergent, object);
            state.battlefield.push_back(divergent);
        }

        let source_id = ledger_observer_source(&mut state);
        let source = state.objects[&source_id].clone();
        const FIXTURE_C: &str = "Whenever this creature deals damage to a player, draw a card if you had two or more artifacts enter the battlefield under your control this turn.";
        let exec_artifact = trigger_execute_from_oracle(FIXTURE_C);
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .execute(exec_artifact.clone()),
            );

        // ── REACH-GUARDS, all before any outcome assertion ──
        assert!(
            crate::game::ability_scan::ability_definition_reads_sibling_mutable_for_loop(
                &exec_artifact
            ),
            "reach-guard: the execute body must read the sibling axis, else the ledger \
             gate's first conjunct is false and this row proves nothing"
        );
        assert!(
            excluding < divergent,
            "reach-guard: the EXCLUDING member must be the min by ObjectId, so the reverted \
             single-representative collapse provably picks it"
        );
        assert!(
            !state.objects[&excluding].tapped && !state.objects[&divergent].tapped,
            "reach-guard: both members untapped, so the collapse key's `tapped` component \
             is inert and `min()` and `min_by_key(tapped, id)` agree"
        );
        assert_ne!(
            state.objects[&excluding].card_types.core_types,
            state.objects[&divergent].card_types.core_types,
            "reach-guard: the two members must DIVERGE on the axis the filter reads — that \
             divergence is the whole premise (`object_content_eq` does not compare it)"
        );
        // The representative ALONE really does exclude, so this row isolates the
        // QUANTIFIER and not the predicate.
        assert!(
            execute_ledger_condition_provably_excludes_class(
                &exec_artifact,
                &state,
                excluding,
                &source
            ),
            "reach-guard: the representative alone DOES exclude — otherwise the veto below \
             would be attributable to the predicate rather than to the quantifier"
        );
        // ...and the divergent member alone does NOT.
        assert!(
            !execute_ledger_condition_provably_excludes_class(
                &exec_artifact,
                &state,
                divergent,
                &source
            ),
            "reach-guard: the divergent member is genuinely NOT excluded — an artifact IS \
             counted by a `Typed{{Artifact}}` ledger filter"
        );

        // ── MATCHED POSITIVE CONTROL: the one-member class IS relieved ──
        let single = HashSet::from([excluding]);
        assert!(
            !fire_time_conditions_read_growing_class(&state, Some(&single)),
            "control: a proven class of JUST the excluding member is relieved, so the \
             subject's veto below is attributable to the second member alone"
        );

        // ── SUBJECT: adding the divergent member must restore the veto ──
        let both = HashSet::from([excluding, divergent]);
        assert!(
            fire_time_conditions_read_growing_class(&state, Some(&both)),
            "CR 603.6a: relief requires the filter to provably exclude EVERY member; the \
             second member is an artifact the `Typed{{Artifact}}` ledger read DOES count, \
             so the observer genuinely observes the loop and the veto must survive"
        );
    }

    /// FIREWALL block-(1) EMPTY-SET vacuity guard, TWO fixtures — one per gate (the
    /// ETB-entry-matcher gate and the battlefield-entry-ledger gate), so a firing arm
    /// is ATTRIBUTABLE to the gate it names.
    ///
    /// WHY TWO FIXTURES (this supersedes a single-fixture design that could not attribute):
    /// both gates are probed by the same call shape, so on a fixture carrying BOTH an
    /// ETB-gate-eligible matcher and a ledger-gate-eligible execute body either probe drives
    /// the call to `false`, arm 1 panics first, and arm 2 never runs. Arm 1 must therefore be
    /// INSENSITIVE to the ledger probe, and the only way to be insensitive to a guard inside
    /// `if let Some(exec) = def.execute` is to carry `execute: None`. Splitting the two
    /// surfaces across two objects of ONE state does not work either: the intervening-if
    /// veto is an unconditional `return true` whenever its object is reached, so such a
    /// state is DETERMINISTICALLY GREEN under the ledger probe on every visit order —
    /// non-discriminating, not nondeterministic.
    ///
    /// The def-kind test (`matches!(def.mode, ChangesZone | ChangesZoneAll)`) is the `.all()`
    /// closure's BODY, and `Iterator::all` returns `true` on an empty set WITHOUT invoking
    /// the closure — which is why an empty set must never reach either quantifier, and why a
    /// ledger-shaped def is NOT immune to the ETB probe.
    #[test]
    fn empty_class_member_set_does_not_relieve() {
        // "another nontoken Wizard you control" — triple-disjoint from a P0 Saproling token.
        let disjoint = TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Wizard".to_string())
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::NonToken, FilterProp::Another]),
        );

        // ── FIXTURE 1: ETB gate. Board cloned from
        // `etb_observer_gate_skips_only_provably_disjoint_observer`, whose DISJOINT +
        // `Some(member)` arm already proves this matcher EXCLUDES this member.
        let mut etb_state = GameState::new_two_player(7);
        let etb_member = inert_token(&mut etb_state, 900, 0, "Saproling");
        {
            let o = etb_state.objects.get_mut(&etb_member).unwrap();
            o.card_types.core_types = vec![CoreType::Creature];
            o.card_types.subtypes = vec!["Saproling".to_string()];
            o.is_token = true;
        }
        let etb_observer = inert_token(&mut etb_state, 910, 1, "Eminence Observer");
        let etb_condition = TriggerCondition::ControlsType {
            filter: TargetFilter::Typed(TypedFilter::creature()),
        };
        etb_state
            .objects
            .get_mut(&etb_observer)
            .unwrap()
            .trigger_definitions
            .push(
                // NO `.execute(..)`: `TriggerDefinition::new` leaves `execute: None`, so
                // block (1)'s `if let Some(exec) = def.execute` is never entered and the
                // LEDGER guard cannot influence this fixture. That is the attribution property.
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .valid_card(disjoint.clone())
                    .condition(etb_condition.clone()),
            );

        // ── FIXTURE 2: ledger gate. Board + execute body lifted from
        // `ledger_exclusion_is_precise_and_fail_closed` arm (i), which already
        // measures this exact body as EXCLUDING ObjectId(800).
        const LEDGER_ARTIFACT_ORACLE: &str = "Whenever this creature deals damage to a player, draw a card if you had two or more artifacts enter the battlefield under your control this turn.";
        let mut ledger_state = GameState::new_two_player(7);
        ledger_state.phase = Phase::PreCombatMain;
        let ledger_member = saproling_class_member(&mut ledger_state); // ObjectId(800)
        let ledger_observer = ledger_observer_source(&mut ledger_state); // ObjectId(801)
        let exec_artifact = trigger_execute_from_oracle(LEDGER_ARTIFACT_ORACLE);
        ledger_state
            .objects
            .get_mut(&ledger_observer)
            .unwrap()
            .trigger_definitions
            .push(
                // NO `.valid_card(..)`. IN UNMUTATED CODE this means the ETB gate cannot
                // `continue` past this def: the non-empty guard passes, so the closure runs,
                // and `etb_observer_provably_excludes_class` requires `def.valid_card
                // .is_some()`. NOTE THE SCOPE — that conjunct is the `.all()` closure's BODY,
                // and under the ETB probe `all()` on an empty set returns `true` WITHOUT
                // invoking it, so `continue` DOES fire there. Arm 2's attribution does not
                // rest on immunity to the ETB probe; it rests on ARM ORDER (arm 1 fires
                // first, with the ETB message).
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .destination(Zone::Battlefield)
                    .execute(exec_artifact.clone()),
            );

        // ── REACH-GUARDS, all before any outcome assertion ────────────────────────────
        // (1) each fixture's veto surface is one the firewall's scan actually SEES
        //     (subsumes the `Effect::Unimplemented => Axes::NONE` vacuity).
        assert!(
            crate::game::ability_scan::trigger_condition_reads_sibling_mutable(&etb_condition),
            "reach-guard: fixture 1's intervening-if must read the sibling axis, else the \
             intervening-if veto never fires and arm 1 proves nothing"
        );
        assert!(
            crate::game::ability_scan::ability_definition_reads_sibling_mutable_for_loop(
                &exec_artifact
            ),
            "reach-guard: fixture 2's execute body must read the sibling axis, else the ledger \
             gate's first conjunct is false and arm 2 proves nothing"
        );
        // (2) MATCHED CONTROLS — with a NON-EMPTY proven class each gate RELIEVES, so the
        //     empty-set vetoes below are attributable to `!is_empty()` and nothing else.
        let etb_class = std::collections::HashSet::from([etb_member]);
        let ledger_class = std::collections::HashSet::from([ledger_member]);
        assert!(
            !fire_time_conditions_read_growing_class(&etb_state, Some(&etb_class)),
            "control: a PROVEN one-member class lets the ETB gate skip this provably \
             disjoint observer"
        );
        assert!(
            !fire_time_conditions_read_growing_class(&ledger_state, Some(&ledger_class)),
            "control: a PROVEN one-member class lets the ledger gate exclude this \
             Artifact-filtered read"
        );

        // ── ARM 1 (B-2a) — block (1) ETB gate ─────────────────────────────────────────
        assert!(
            fire_time_conditions_read_growing_class(&etb_state, Some(&HashSet::new())),
            "BLOCK-(1) ETB GATE: an EMPTY class set proves nothing, so \
             `members.iter().all(..)` must not be vacuously true — deleting \
             `!members.is_empty() &&` from the ETB gate makes it `continue` past every \
             trigger def regardless of its `TriggerMode`, because the def-kind test lives \
             inside the closure and `all()` never calls it on an empty set. This fixture \
             carries `execute: None`, so the LEDGER guard cannot affect it: if THIS message \
             appears, the ETB guard is the one that was removed"
        );
        // ── ARM 2 (B-2b) — block (1) ledger gate ──────────────────────────────────────
        assert!(
            fire_time_conditions_read_growing_class(&ledger_state, Some(&HashSet::new())),
            "BLOCK-(1) LEDGER GATE: same vacuity, other site — deleting \
             `!members.is_empty() &&` from the ledger gate makes the inner `all()` vacuously \
             true, `is_some_and` true, which negates to `false` and drops the veto. \
             ATTRIBUTION rests on ARM ORDER, not on immunity: under the ETB probe arm 1 \
             above fires FIRST with the ETB message, so this message can only appear when \
             the ledger guard is the one that was removed. (In UNMUTATED code this fixture \
             also cannot be skipped by the ETB gate — it carries no `valid_card`, which \
             `etb_observer_provably_excludes_class` requires — but that is a property of the \
             unmutated closure body, which an empty set short-circuits past.)"
        );
    }

    /// G6-1 — ROUTER BYTE-IDENTITY. `counter_growth_is_observed` (`:2923`) and
    /// `life_growth_is_observed` (`:2946`) are ROUTERS, not suppressors: a `true` there
    /// selects the O(N) discrete driver and the offer still forms. They keep the 2-arg
    /// wrappers (`LoopWindowScope::unproven()`), so the phase-unreachability narrowing
    /// must NOT reach them — a `{Phase, End}` observer scanned at `PreCombatMain` still
    /// reports OBSERVED at both routers even though the identically-shaped observer IS
    /// relieved at the two suppressing covers (rows X2-1 / X2-2).
    ///
    /// REVERT-PROBE: switch either router to its `_scoped` sibling with a populated
    /// `phase_invariant` ⇒ the matching assertion flips to `false` ⇒ FAILS.
    #[test]
    fn observedness_callers_literal_expectation() {
        use crate::types::ability::TriggerCondition;

        // A SIBLING (growing-class) observer gated on a step the state is not in.
        let sibling = phase_gated_observer_board(TriggerCondition::ControlsType {
            filter: TargetFilter::Any,
        });
        assert_eq!(sibling.phase, Phase::PreCombatMain);
        assert!(
            counter_growth_is_observed(&sibling),
            "G6-1: the counter router must stay byte-identical — a phase-unreachable \
             observer is still OBSERVED here, because routing true only picks the \
             discrete driver (it never suppresses the offer)"
        );

        // A PROJECTED (life) observer gated on the same unreachable step.
        let projected = phase_gated_observer_board(TriggerCondition::GainedLife { minimum: 1 });
        assert!(
            life_growth_is_observed(&projected),
            "G6-1: the life router must stay byte-identical for the same reason"
        );

        // PAIRED NEGATIVE (so the instrument provably returns both answers): a board
        // with no observer at all reports NOT observed at both routers.
        let benign = GameState::new_two_player(7);
        assert!(!counter_growth_is_observed(&benign));
        assert!(!life_growth_is_observed(&benign));
    }

    /// X1-3 — [`window_scope_from_cover_frames`] is FAIL-CLOSED on every conjunct, and
    /// each `None` assertion is PAIRED with the `Some` it degenerates from, so the
    /// instrument provably returns both answers on both axes.
    ///
    /// REVERT-PROBES, one per conjunct:
    /// * drop the all-equal fold over the two sequences (return the first controller) ⇒
    ///   the heterogeneous `sole_driver == None` assertion FAILS.
    /// * drop the both-frames requirement (read only `pa`) ⇒ the one-empty-sequence
    ///   `sole_driver == None` assertion FAILS.
    /// * drop the `extra_phases` conjunct (CR 500.8) ⇒ the `phase_invariant == None`
    ///   assertion FAILS while the turn/phase ones still pass.
    /// * drop the turn-number conjunct ⇒ the differing-turn assertion FAILS.
    #[test]
    fn window_scope_is_fail_closed_on_a_heterogeneous_window() {
        use crate::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

        fn ctx(controller: u8) -> LoopActionContext {
            LoopActionContext {
                card_id: CardId(64),
                controller: PlayerId(controller),
                action: LoopAction::Recast {
                    from_zone: Zone::Hand,
                    uses_buyback: BuybackUsage::Used,
                },
                convoke: None,
                pins: Vec::new(),
            }
        }

        // Baseline frame pair: same turn, same step-granular phase, no extra phases,
        // both sequences driven by P0.
        let base = || {
            let mut s = GameState::new_two_player(7);
            s.turn_number = 13;
            s.phase = Phase::PreCombatMain;
            s.last_loop_action_sequence = vec![ctx(0)];
            s
        };

        // ── `sole_driver` — CR 117.1 ──
        let (pa, pb) = (base(), base());
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb, None, None).sole_driver,
            Some(PlayerId(0)),
            "PAIRED POSITIVE: a homogeneous single-driver window proves CR 117.1's premise"
        );

        // (s2) heterogeneous ACROSS the two frames — the case a `pa`-only read would
        // mint `Some(P0)` for, which is the relieving direction #4603 forbids.
        let mut pb_other = base();
        pb_other.last_loop_action_sequence = vec![ctx(1)];
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb_other, None, None).sole_driver,
            None,
            "(s2) a two-controller window proves nothing about who holds priority"
        );

        // (s2) heterogeneous WITHIN one frame.
        let mut pa_mixed = base();
        pa_mixed.last_loop_action_sequence = vec![ctx(0), ctx(1)];
        assert_eq!(
            window_scope_from_cover_frames(&pa_mixed, &pb, None, None).sole_driver,
            None,
            "(s2) an interleaved sequence is fail-closed"
        );

        // (s1) an EMPTY sequence proves nothing — not "nobody drove this".
        let mut pb_empty = base();
        pb_empty.last_loop_action_sequence.clear();
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb_empty, None, None).sole_driver,
            None,
            "(s1) an empty driving sequence is NO PROOF, so it cannot relieve anything"
        );

        // ── `phase_invariant` — CR 500.1 / CR 506.1 / CR 500.8 ──
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb, None, None).phase_invariant,
            Some(Phase::PreCombatMain),
            "PAIRED POSITIVE: agreeing frames with no extra phase prove the window's phase"
        );

        // (p3) CR 500.8: a queued extra phase can duplicate the SAME phase inside one
        // turn, so "equal phase" no longer implies "never left it".
        let mut pb_extra = base();
        pb_extra
            .extra_phases
            .push(crate::types::game_state::ExtraPhase {
                anchor: Phase::PreCombatMain,
                phase: Phase::PreCombatMain,
                attacker_restriction: None,
                attacker_restriction_source: None,
            });
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb_extra, None, None).phase_invariant,
            None,
            "(p3) CR 500.8: a pending extra phase breaks `equal phase ⇒ never left it`"
        );

        // (p1) different turns.
        let mut pb_turn = base();
        pb_turn.turn_number = 14;
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb_turn, None, None).phase_invariant,
            None,
            "(p1) frames from different turns bound nothing about one window's phase"
        );

        // (p2) different step-granular phases.
        let mut pb_phase = base();
        pb_phase.phase = Phase::PostCombatMain;
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb_phase, None, None).phase_invariant,
            None,
            "(p2) a window that crosses a phase boundary is not phase-invariant"
        );
    }
    /// CR 732.2a — `ring_delta_signature`'s "seen TWICE" contract, at the building-block
    /// level: five arms over synthetically-built rings, so every input shape the function can
    /// meet is exercised rather than whichever one a fixture happens to produce.
    ///
    /// `k` is an OUTPUT here, exactly as O4 requires of production: no period constant exists
    /// in the function, and the only numbers in this row are the periods THIS TEST
    /// CONSTRUCTED. Comparing a derived period against a constructed one is the discriminator
    /// for "smallest repeating period"; it is not fixture-brittleness, because the input is
    /// built two lines above the assertion.
    ///
    /// PRODUCTION-PATH COMPANIONS, both in `tests/integration/loop_shortcut.rs`:
    /// `bounded_offer_on_a_within_turn_draw_drain_is_basis_b`, where the ENGINE writes a
    /// basis-B offer whose published `frames_per_period != 1` on a within-turn draw↔drain
    /// cascade; and `dina_untargeted_drain_4p_offers_at_three_live_opponents`, measured to
    /// certify through this function with a derived `k == 1` on a REAL 4-player dump (see that
    /// row's doc for the two measurements, and for why a `k == 1` basis-B offer is
    /// indistinguishable from a basis-A one in the published payload).
    ///
    /// RETRACTION, kept on the record: this doc previously named
    /// `fantastic_four_bounded_loop_4p` as that companion, "a DERIVED `k == 2` and a bound of
    /// 35". Both numbers reproduce, and the reading was wrong — measured, F4's δ carries
    /// `library -1` for ALL FOUR players and its certifying frames sit at turns
    /// `[5, 9, 9, 13, 13]`, so the "period" is one 4-player TURN CYCLE (CR 703.4d: only the
    /// active player draws in a draw step) and the "35" is 35 turn cycles, not 35 loop
    /// iterations. That certificate is the same artifact as the drawgo false positive, which
    /// is why `ring_delta_signature` now refuses it outright.
    ///
    /// REVERT-PROBES, each flipping a DIFFERENT arm so none dominates another:
    /// * accept a period seen ONCE (search `k` up to `frames - 1` and compare only one
    ///   window) ⇒ arm ⓐ's `None` at 2 frames becomes `Some` ⇒ FAILS.
    /// * drop the zero-delta refusal ⇒ arm ⓓ returns `Some` ⇒ FAILS.
    /// * scan the OLDEST `2k` deltas instead of the most recent ⇒ arm ⓔ (a ring whose old
    ///   stretch repeats and whose recent stretch does not) returns `Some` ⇒ FAILS.
    /// * take the LARGEST repeating `k` instead of the smallest ⇒ arm ⓑ returns `k = 2` for a
    ///   constant-delta ring ⇒ FAILS.
    /// * delete the CR 703.1 turn-position conjunct ⇒ arm ⓕ returns `Some` ⇒ FAILS.
    #[test]
    fn ring_delta_signature_certifies_only_a_period_seen_twice() {
        /// A ring whose frames carry the given life totals for P1, in order. Everything else
        /// is held identical, so the frame-deltas are exactly the successive differences.
        fn ring_of(lives: &[i32]) -> GameState {
            let mut state = GameState::new_two_player(0);
            for &life in lives {
                let mut frame = GameState::new_two_player(0);
                frame.players[1].life = life;
                // Both halves built exactly as `record_loop_detect_sample` builds them, so
                // the fixture cannot diverge from production's construction.
                state.loop_detect_ring.push_back(std::sync::Arc::new(
                    crate::types::LoopDetectSample {
                        normalized: frame.normalize_for_loop(),
                        live: frame.loop_detect_live_sample(),
                    },
                ));
            }
            state
        }

        // ⓐ REFUSING: 2 frames is one delta — a period seen ONCE. `2k + 1 = 3` is the
        //   threshold at the smallest possible `k`, and it is an EXPRESSION, never a literal
        //   in the function.
        let short = ring_of(&[40, 39]);
        assert_eq!(
            short.loop_detect_ring.len(),
            2,
            "reach-guard on the built ring"
        );
        assert_eq!(
            ring_delta_signature(&short),
            None,
            "a period observed once is a coincidence; certifying it would let an offer be \
             minted off a single frame pair"
        );

        // ⓑ POSITIVE CONTROL, same shape one frame longer: the instrument provably returns
        //   `Some`, so ⓐ is not a function that always refuses.
        let steady = ring_of(&[40, 39, 38]);
        let (k, delta) = ring_delta_signature(&steady)
            .expect("three frames observe a 1-frame period TWICE, which is the contract");
        assert_eq!(
            k, 1,
            "the SMALLEST repeating period, derived — a constant per-frame delta has period 1"
        );
        // Bound to a local rather than inlined: `2k + 1` is the CONTRACT expression (2k deltas
        // ⇒ the period was observed twice), and clippy's `int_plus_one` would otherwise push it
        // to a `> 2k` that no longer reads as the rule.
        let frames_needed = 2 * k as usize + 1;
        assert!(
            steady.loop_detect_ring.len() >= frames_needed,
            "the structural invariant every certified period satisfies: 2k+1 frames"
        );
        assert_ne!(
            delta,
            ResourceVector::default(),
            "a certified period moves some resource"
        );
        assert_eq!(
            delta.life.get(&PlayerId(1)).copied(),
            Some(-1),
            "and the published delta is the one measured across ONE period"
        );

        // ⓒ a 2-frame period, seen twice: 5 frames. The derived `k` must be the constructed
        //   one, and the delta must span the WHOLE period, not one frame of it.
        let period_two = ring_of(&[40, 39, 36, 35, 32]);
        let (k2, delta2) = ring_delta_signature(&period_two)
            .expect("(-1, -3) repeated twice over 5 frames is a period seen twice");
        assert_eq!(
            k2, 2,
            "derived from the ring, and equal to the period this test BUILT two lines above"
        );
        assert_eq!(
            delta2.life.get(&PlayerId(1)).copied(),
            Some(-4),
            "one whole period is -1 + -3, not either half"
        );

        // ⓓ a ring that repeats perfectly but moves NOTHING: no CR 704 threshold to bound, so
        //   no signature. Without this the offer's bound would be the safety cap.
        let flat = ring_of(&[40, 40, 40, 40, 40]);
        assert_eq!(
            ring_delta_signature(&flat),
            None,
            "a zero-delta cycle states no threshold; every multiple of a zero period is zero \
             too, which is why this refuses outright rather than searching on"
        );

        // ⓕ the CR 703.1 turn-position conjunct, at the building-block level: the SAME ring as
        //   ⓑ, with only the newest frame's turn number moved on. Nothing about the deltas
        //   changes (`ResourceVector::snapshot` never reads `turn_number` or `phase`), so a
        //   `None` here is attributable to the conjunct alone.
        let mut turn_crossing = ring_of(&[40, 39, 38]);
        {
            let last = turn_crossing
                .loop_detect_ring
                .back_mut()
                .expect("the ring was just built with three frames");
            // `.normalized` is the half the subject reads (`ring_delta_signature` →
            // `window_scope_from_cover_frames`). Retargeting this to `.live` makes the
            // subject see no turn crossing ⇒ `Some` ⇒ the `assert_eq!(.., None, ..)` below
            // FAILS LOUDLY. That is the arm working, not a reason to weaken the assertion.
            std::sync::Arc::make_mut(last).normalized.turn_number += 1;
        }
        assert_eq!(
            ring_delta_signature(&turn_crossing),
            None,
            "a period paved by a turn boundary is the game advancing, not a CR 732.2a loop — \
             the board-blind basis must refuse it (the ⓑ ring differs from this one in \
             `turn_number` and nothing else)"
        );

        // ⓔ the OLD stretch repeats, the RECENT one does not. A scan anchored at the oldest
        //   deltas would certify a period the loop is no longer running.
        let stale = ring_of(&[40, 39, 38, 37, 30]);
        assert_eq!(
            ring_delta_signature(&stale),
            None,
            "the certified period must be the one the loop is running NOW: the most recent \
             2k deltas are (-1, -7) at k=1 and (-1,-1),(-1,-7) at k=2, neither of which repeats"
        );
    }

    /// CR 703.1 / CR 732.2a — the PRODUCTION-DRIVEN half of the turn-position conjunct: on a
    /// board with NO loop at all, the game's own turn structure is exactly periodic in the
    /// resource axes basis B reads, and this row asserts the board-blind basis derives no
    /// signature from it at any beat.
    ///
    /// HOME. This lives in `resource.rs`'s unit module and NOT in
    /// `tests/integration/loop_shortcut.rs` because `ring_delta_signature` is `pub(crate)`:
    /// an integration test is a separate crate and cannot name it. The only alternative — a
    /// re-derivation of the period search inside the test — is prohibited: it would be a
    /// second copy of the very algorithm this row exists to pin, and it would stay green
    /// while production drifted. What is copied here is a test HARNESS (the drawgo fixture
    /// builder from `drawgo_ring_spans_turns_but_never_offers` and a beat driver), never the
    /// thing under test, which is CALLED.
    ///
    /// FIXTURE, loop-free by construction: P0 has an upkeep ticker, a drain cleric and a
    /// "may draw" scribe. Each of P0's upkeeps runs a FINITE 3-deep cascade — nothing
    /// re-triggers the ticker — yet the per-turn shape is drain-like, which is exactly what a
    /// board-blind periodicity test mistakes for a loop. MEASURED, on this tree: with the
    /// conjunct absent the engine minted offers on this board (the sibling integration row
    /// `drawgo_ring_spans_turns_but_never_offers` failed on its `offer_at.is_none()`
    /// assertion); with it, that row is green and a seam probe over 400 beats counts ZERO
    /// engine offers.
    ///
    /// THE FLATTEN ARM discharges two obligations at once, on the SAME trajectory and the
    /// SAME ring, with exactly ONE axis neutralized:
    /// * NON-ZERO POPULATION — a `∀ beats: is_none()` over an empty beat set is vacuously
    ///   true, so the row must prove its quantifier ranged over beats where a signature was
    ///   actually derivable. `flattened_some > 0` is that proof.
    /// * SAME-TRAJECTORY POSITIVE CONTROL — the instrument is shown returning the
    ///   non-refusing value on drawgo's own data, so the `None`s above are a measured refusal
    ///   rather than an inert instrument.
    /// * ATTRIBUTION — `ResourceVector::snapshot` reads life / library / poison / energy /
    ///   mana / battlefield counters / `combat_phases_started_this_turn` / `extra_phases`, and
    ///   never `turn_number` or `phase`, so δ and the derived `k` are unchanged by the
    ///   flattening and the `None` → `Some` flip is attributable to the turn-position
    ///   conjunct alone.
    ///
    /// MEASURED on this tree: 253 of the 300 driven beats hold >= 3 frames, and the flatten
    /// arm derives a signature at **225** of them, over 22 turns.
    ///
    /// REVERT-PROBE (must FLIP): delete the CR 703.1 conjunct from `ring_delta_signature` ⇒
    /// the `is_none()` assertion fires on the first signature-bearing beat.
    #[test]
    fn drawgo_turn_structure_yields_no_basis_b_signature() {
        use crate::game::scenario::GameScenario;
        use crate::types::actions::GameAction;
        use crate::types::game_state::{LoopDetectionMode, WaitingFor};

        /// One beat of the shared dump drive policy (`tests/integration/loop_shortcut.rs`'s
        /// `dump_drive_one_beat`): at `Priority` always pass — the mandatory triggers resolve
        /// and re-trigger, which IS the loop when there is one — and otherwise take the first
        /// legal non-terminal action.
        fn drive_one_beat(state: &mut GameState) -> Result<(), String> {
            let actor = state
                .waiting_for
                .acting_player()
                .into_iter()
                .chain(state.players.iter().map(|p| p.id))
                .find_map(|p| {
                    let (actions, _costs, _grouped) =
                        crate::ai_support::legal_actions_for_viewer(state, p);
                    (!actions.is_empty()).then_some((p, actions))
                });
            let Some((who, actions)) = actor else {
                return Err(format!("no legal actor at {:?}", state.waiting_for));
            };
            let forbidden =
                |a: &GameAction| matches!(a, GameAction::Concede { .. } | GameAction::Debug(_));
            let chosen = if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                actions
                    .iter()
                    .find(|a| matches!(a, GameAction::PassPriority))
            } else {
                actions
                    .iter()
                    .find(|a| !matches!(a, GameAction::PassPriority) && !forbidden(a))
                    .or_else(|| actions.iter().find(|a| !forbidden(a)))
            };
            let Some(action) = chosen.cloned() else {
                return Err(format!("empty action list at {:?}", state.waiting_for));
            };
            crate::game::engine::apply(state, who, action.clone())
                .map(|_| ())
                .map_err(|e| format!("apply err ({action:?}): {e:?}"))
        }

        let mut scenario = GameScenario::new_n_player(2, 7);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(PlayerId(0), 20);
        scenario.with_life(PlayerId(1), 20);
        scenario.add_creature_from_oracle(
            PlayerId(0),
            "Test Upkeep Ticker",
            2,
            2,
            "At the beginning of your upkeep, you gain 1 life.",
        );
        scenario.add_creature_from_oracle(
            PlayerId(0),
            "Test Drain Cleric",
            2,
            2,
            "Whenever you gain life, each opponent loses 1 life.",
        );
        scenario.add_creature_from_oracle(
            PlayerId(0),
            "Test May Scribe",
            2,
            2,
            "Whenever an opponent loses life, you may draw a card.",
        );
        // CR 504.1: both players draw every turn, so the libraries must outlast the drive — a
        // deck-out would end the game and silently truncate every assertion below.
        let names: Vec<String> = (0..60).map(|i| format!("Filler {i}")).collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        scenario.with_library_top(PlayerId(0), &refs);
        scenario.with_library_top(PlayerId(1), &refs);
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        let mut state = runner.state().clone();

        assert!(
            state.loop_detection.samples(),
            "reach-guard: a non-sampling mode never populates the ring, which would make the \
             per-beat `is_none()` below vacuous; got {:?}",
            state.loop_detection
        );
        assert_eq!(
            state.loop_detect_ring.len(),
            0,
            "reach-guard: every frame the assertions below read was accumulated by THIS drive"
        );

        let mut long_ring_beats = 0usize;
        let mut flattened_some = 0usize;
        let mut turns_seen: Vec<u32> = Vec::new();
        for beat in 0..300usize {
            if !turns_seen.contains(&state.turn_number) {
                turns_seen.push(state.turn_number);
            }
            // 2k + 1 at the smallest derivable k: below three frames the refusal is the
            // short-ring one, which says nothing about turn position.
            if state.loop_detect_ring.len() >= 3 {
                long_ring_beats += 1;
                assert_eq!(
                    ring_delta_signature(&state),
                    None,
                    "beat {beat} (turn {}, {} frames): this board has no loop — each upkeep \
                     runs a FINITE cascade and nothing re-triggers the ticker — so the \
                     board-blind basis must derive no period from the game's own turn \
                     structure",
                    state.turn_number,
                    state.loop_detect_ring.len()
                );

                // Same trajectory, same ring, ONE axis neutralized.
                let mut flat = state.clone();
                for frame in flat.loop_detect_ring.iter_mut() {
                    // `.normalized` is the half the subject reads. Writing `.live` instead
                    // leaves the axis un-flattened ⇒ `flattened_some == 0` ⇒ the shipped
                    // `assert!(flattened_some > 0, ..)` below FAILS LOUDLY.
                    let f = &mut std::sync::Arc::make_mut(frame).normalized;
                    f.turn_number = 1;
                    f.phase = Phase::Upkeep;
                }
                if ring_delta_signature(&flat).is_some() {
                    flattened_some += 1;
                }
            }
            if drive_one_beat(&mut state).is_err() {
                break;
            }
        }

        assert!(
            turns_seen.len() >= 3,
            "reach-guard: the drive must cross at least two full turn boundaries for a \
             turn-position claim to mean anything; saw turns {turns_seen:?}"
        );
        assert!(
            long_ring_beats > 0,
            "reach-guard: the ∀ above ranged over ZERO beats holding 2k+1 frames, so it was \
             vacuously true"
        );
        assert!(
            flattened_some > 0,
            "O4(2) + O4(3): with `turn_number` and `phase` flattened on a clone of THIS \
             trajectory's own ring — and nothing else changed — a signature must appear at \
             some beat. It appeared at {flattened_some} of {long_ring_beats} long-ring beats. \
             A zero here would mean the `None`s above are attributable to a short ring or a \
             non-repeating delta rather than to the CR 703.1 conjunct, and the row would not \
             be sound"
        );
    }

    /// CR 732.2a — `PeriodicDelta` rides `WaitingFor::LoopShortcut` over the wire, so the
    /// whole payload must survive `serde_json`. TWO map-key hazards, both real: a
    /// `ResourceVector.counters` key is the `(CounterClass, ObjectClass)` TUPLE, and a
    /// `BTreeMap` keyed by `DecisionSlot` (a struct) would be the same failure — which is
    /// why `victim_slot` is a `Vec` of pairs and not a map at all.
    ///
    /// The runtime symptom of getting this wrong is NOT a soft failure:
    /// `crates/engine-wasm/src/lib.rs`'s serializer `panic!`s on the error, i.e. a browser
    /// crash.
    ///
    /// ARM (ii) ALONE WOULD PASS AGAINST A BROKEN MAP — an empty map serializes fine
    /// whatever its key type. Arm (i) is what discriminates, and both are asserted here.
    ///
    /// ⚠ **THIS ROW IS NOT SUFFICIENT ON ITS OWN, and its green was once read as if it
    /// were.** It uses `to_string`/`from_str` throughout, and that combination was measured
    /// `Ok` even against an UNADAPTED `PlayerId`-keyed map. The production persistence path
    /// degrades to `serde_json::Value` + `from_value` inside `PersistedGameState`, where
    /// serde's `Content` buffering stringifies map keys and a `PlayerId` key breaks.
    /// `a_populated_per_cycle_proposal_survives_the_production_persistence_boundary` is the
    /// row that covers that; keep BOTH, they discriminate different failures.
    ///
    /// REVERT-PROBES: drop `#[serde(with = "map_key_pairs")]` from
    /// `ResourceVector.counters` ⇒ arm (i) and the payload arm both fail with "key must be a
    /// string"; change `PeriodicDelta.victim_slot` to a `BTreeMap<DecisionSlot, i64>` ⇒ same.
    /// MUST-NOT-FLIP: a `LoopShortcut` payload with `per_cycle: None` stays byte-identical
    /// (`skip_serializing_if`), asserted in the third block.
    #[test]
    fn periodic_delta_survives_the_serde_json_wire() {
        use crate::analysis::decision_template::{DecisionSlot, ShortcutDecisionSchema};
        use crate::analysis::loop_check::{LoopCertificate, WinKind};
        use crate::types::game_state::{WaitingFor, YieldTarget};

        let slot = DecisionSlot {
            source: YieldTarget::ThisObject {
                source_id: ObjectId(403),
                incarnation: Some(7),
                trigger_description: None,
            },
            index: 0,
        };

        // (i) POPULATED — a non-empty tuple-keyed `counters` map is the discriminating input.
        let mut delta = ResourceVector::default();
        delta.life.insert(PlayerId(1), -3);
        delta.life.insert(PlayerId(0), 3);
        delta
            .counters
            .insert((CounterClass::Plus1Plus1, ObjectClass::Creature), 2);
        delta
            .counters
            .insert((CounterClass::Poison, ObjectClass::Player), 1);
        delta.generic_triggers.insert(TriggerKind::Proliferate, 4);
        assert!(
            !delta.counters.is_empty() && !delta.life.is_empty(),
            "reach-guard: arm (i) is only discriminating while `counters` is NON-EMPTY — an \
             empty map round-trips whatever the key type"
        );
        let populated = PeriodicDelta {
            frames_per_period: 2,
            delta,
            victim_slot: vec![(slot.clone(), 1)],
        };
        let json = serde_json::to_string(&populated)
            .expect("a populated PeriodicDelta must serialize (engine-wasm PANICS otherwise)");
        assert_eq!(
            serde_json::from_str::<PeriodicDelta>(&json).expect("and round-trip"),
            populated
        );

        // (ii) EMPTY — the degenerate arm, kept only so the pair is visible.
        let empty = PeriodicDelta::default();
        let empty_json = serde_json::to_string(&empty).expect("an empty PeriodicDelta too");
        assert_eq!(
            serde_json::from_str::<PeriodicDelta>(&empty_json).expect("and round-trip"),
            empty
        );

        // The ACTUAL wire payload: the `WaitingFor` variant that carries it.
        let cert = LoopCertificate {
            unbounded: vec![],
            win_kind: WinKind::LethalDamage,
            mandatory: false,
            residual_board_delta: BoardDelta::default(),
            per_cycle: Some(populated),
        };
        let offer = WaitingFor::LoopShortcut {
            proposer: PlayerId(0),
            predicted_winner: None,
            certificate: cert.clone(),
            schema: ShortcutDecisionSchema::default(),
            declaration: None,
        };
        let offer_json =
            serde_json::to_string(&offer).expect("the LoopShortcut payload carrying it must too");
        assert_eq!(
            serde_json::from_str::<WaitingFor>(&offer_json).expect("and round-trip"),
            offer
        );

        // MUST-NOT-FLIP: `skip_serializing_if` keeps the shipped payload byte-identical —
        // `per_cycle` appears nowhere in the JSON of an offer that states none.
        let shipped = WaitingFor::LoopShortcut {
            proposer: PlayerId(0),
            predicted_winner: None,
            certificate: LoopCertificate {
                per_cycle: None,
                ..cert
            },
            schema: ShortcutDecisionSchema::default(),
            declaration: None,
        };
        let shipped_json = serde_json::to_string(&shipped).expect("serializes");
        assert!(
            !shipped_json.contains("per_cycle"),
            "an offer stating no per-period signature must be byte-identical to BASE; got \
             {shipped_json}"
        );
    }

    /// CR 616.1 — an EMPTY derivation must not discharge the replacement obligation.
    ///
    /// `resolution_events_are_discharged` answers `FreeUnlessReplacements(events)` with
    /// `!events.iter().any(..)`, and `any()` over an empty slice is `false`, so `!any(..)`
    /// is `true`: an empty vector certified the entry having inspected NOTHING. The only
    /// thing standing against that was a `debug_assert!`, which compiles out of release —
    /// so the fail-open case was live in exactly the build that ships, and was untestable
    /// besides (a `debug_assert!` aborts the build tests run in).
    ///
    /// MATCHED PAIR:
    /// * EMPTY ⇒ `false` (refuse). This is the arm the fix adds.
    /// * NON-EMPTY, no applicable replacement ⇒ `true` (discharge). Without it the row
    ///   would pass against a predicate that simply returned `false` always.
    ///
    /// `MayPrompt ⇒ false` is asserted alongside so all three arms of the match are pinned.
    ///
    /// REVERT-PROBE (run, recorded): delete the `if events.is_empty() { return false; }`
    /// arm ⇒ the EMPTY case FLIPS TO `true` and this row FAILS, while the non-empty arms
    /// stay green.
    #[test]
    fn an_empty_derivation_does_not_vacuously_discharge_the_cr_616_1_obligation() {
        use crate::game::resolution_prompt::ResolutionChoiceFreedom;

        let board = GameState::new_two_player(7);

        assert!(
            !resolution_events_are_discharged(
                &board,
                ResolutionChoiceFreedom::FreeUnlessReplacements(Vec::new())
            ),
            "an EMPTY derivation proves nothing about CR 616.1 replacements and must \
             REFUSE — `!any()` over an empty slice is `true`, which is the fail-open \
             direction this predicate exists to prevent"
        );

        // POSITIVE CONTROL — a real event on a board with no applicable replacement still
        // discharges, so the refusal above is about EMPTINESS and not a blanket `false`.
        let non_empty = vec![crate::types::proposed_event::ProposedEvent::LifeLoss {
            player_id: PlayerId(0),
            amount: 1,
            applied: Default::default(),
        }];
        assert!(
            crate::game::replacement::proposed_event_prompt_cause(
                &board,
                &non_empty[0],
                crate::game::replacement::replacement_registry(),
            )
            .is_empty(),
            "reach-guard: the control event must have NO applicable replacement on this \
             board, or it would refuse for the wrong reason"
        );
        assert!(
            resolution_events_are_discharged(
                &board,
                ResolutionChoiceFreedom::FreeUnlessReplacements(non_empty)
            ),
            "a NON-EMPTY derivation with no applicable replacement must still discharge"
        );

        // The third arm, pinned so the match stays exhaustively covered.
        assert!(
            !resolution_events_are_discharged(&board, ResolutionChoiceFreedom::MayPrompt),
            "MayPrompt is never discharged"
        );
    }

    /// CR 732.2a — the PRODUCTION persistence path for a proposal that carries a per-cycle
    /// signature. The row above is NOT a substitute and its green was FALSE CONFIDENCE: it
    /// round-trips with `to_string`/`from_str`, and that combination was measured `Ok` even
    /// against the unadapted map. The failure needs the enclosing shape:
    ///
    /// * `WaitingFor` is `#[serde(tag = "type", content = "data")]`, so serde buffers the
    ///   payload through its private `Content`, which stringifies every map KEY;
    /// * `PlayerId` is `#[serde(transparent)]` over `u8`, so it then reads a string where it
    ///   wants an integer;
    /// * `PersistedGameState::deserialize` routes EVERY decode through `serde_json::Value`
    ///   and `from_value` — including the production WASM restore, whose outer call is
    ///   `from_str::<PersistedGameState>`. So `from_str` at the boundary does NOT save it.
    ///
    /// This row therefore drives `to_value`/`from_value` and the real
    /// `PersistedGameState` boundary, with a POPULATED `PlayerId`-keyed map, which is the
    /// only combination that discriminates.
    ///
    /// REVERT-PROBE (run, recorded): delete `#[serde(with = "map_key_pairs")]` from
    /// `ResourceVector::life` ⇒ arms (i) and (ii) FAIL with
    /// `invalid type: string "0", expected u8`, the exact text
    /// `tests/integration/loop_shortcut.rs` had recorded as a standing limitation.
    /// REACH-GUARD: the map is asserted non-empty before the round trip — an empty map
    /// round-trips whatever its key type, so a populated one is what makes this row real.
    #[test]
    fn a_populated_per_cycle_proposal_survives_the_production_persistence_boundary() {
        use crate::analysis::decision_template::IterationCount;
        use crate::analysis::loop_check::{ShortcutProposal, WinKind};
        use crate::types::game_state::{PersistedGameState, WaitingFor};

        let mut delta = ResourceVector::default();
        delta.life.insert(PlayerId(0), 3);
        delta.life.insert(PlayerId(1), -3);
        delta.damage_dealt.insert(PlayerId(1), 3);
        delta.library_delta.insert(PlayerId(0), -1);
        delta.poison.insert(PlayerId(1), 1);
        delta
            .counters
            .insert((CounterClass::Plus1Plus1, ObjectClass::Creature), 2);
        delta.generic_triggers.insert(TriggerKind::Proliferate, 4);
        assert!(
            !delta.life.is_empty()
                && !delta.damage_dealt.is_empty()
                && !delta.library_delta.is_empty()
                && !delta.poison.is_empty(),
            "reach-guard: all four `PlayerId`-keyed maps must be NON-EMPTY, or this row \
             passes against a broken key type"
        );
        let proposal = ShortcutProposal {
            proposer: PlayerId(0),
            predicted_winner: Some(PlayerId(0)),
            count: IterationCount::UntilLethal,
            unbounded: vec![],
            win_kind: WinKind::LethalDamage,
            template: None,
            per_cycle: Some(PeriodicDelta {
                frames_per_period: 2,
                delta,
                victim_slot: vec![],
            }),
        };
        let wait = WaitingFor::RespondToShortcut {
            player: PlayerId(1),
            remaining_players: vec![],
            proposal: proposal.clone(),
        };

        // (i) The precise mechanism: adjacently-tagged `WaitingFor` through `Value`.
        let value = serde_json::to_value(&wait).expect("the wait serializes");
        assert_eq!(
            serde_json::from_value::<WaitingFor>(value).expect(
                "a populated per-cycle proposal must survive `from_value` — this is the \
                 combination `Content` key-stringification breaks"
            ),
            wait
        );

        // (ii) The PRODUCTION boundary: whole state through `PersistedGameState`, both the
        // outer API the WASM bridge uses (`from_str`) and the `Value` form it degrades to.
        let mut state = GameState::new_two_player(7);
        state.waiting_for = wait.clone();
        let raw = serde_json::to_value(&state).expect("the state serializes");
        let restored = serde_json::from_value::<PersistedGameState>(raw.clone())
            .expect("decodes through the production persistence boundary")
            .into_game_state();
        assert_eq!(
            restored.waiting_for, wait,
            "the restored wait must carry the SAME per-cycle signature, not a dropped or \
             emptied one"
        );
        let text = serde_json::to_string(&raw).expect("serializes to text");
        let via_str = serde_json::from_str::<PersistedGameState>(&text)
            .expect("and through the WASM bridge's own `from_str::<PersistedGameState>`")
            .into_game_state();
        assert_eq!(via_str.waiting_for, wait);

        // (iii) MUST-NOT-FLIP: a proposal stating no signature stays absent from the wire.
        let none = ShortcutProposal {
            per_cycle: None,
            ..proposal
        };
        let none_json = serde_json::to_string(&none).expect("serializes");
        assert!(
            !none_json.contains("per_cycle"),
            "`skip_serializing_if` must keep a signature-free proposal byte-identical to \
             BASE; got {none_json}"
        );
    }

    // ── PR-7 Phase 5c: the DRAW verdict's paired CR 616.1 obligation ──

    /// A mandatory, non-"up to" `Effect::Draw` trigger — the starved shape the
    /// `FreeUnlessReplacements(DRAW)` arm claims.
    fn draw_entry(id: u64) -> StackEntry {
        churn_entry(
            id,
            0,
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                ObjectId(CHURN_SRC),
                PlayerId(0),
            ),
            None,
        )
    }

    fn with_replacements(entry: StackEntry, defs: &[ReplacementDefinition]) -> GameState {
        let mut state = GameState::new_two_player(7);
        state.stack.push_back(entry);
        // Owned by P0 — the player whose draw these defs are meant to replace
        // (CR 614.1 scopes a def to its controller's events).
        let oid = bf_object_owned_by(&mut state, 900, PlayerId(0));
        let object = state.objects.get_mut(&oid).expect("just inserted");
        for def in defs {
            object.replacement_definitions.push(def.clone());
        }
        state
    }

    fn repl(event: ReplacementEvent, optional: bool) -> ReplacementDefinition {
        let is_draw = matches!(event, ReplacementEvent::Draw);
        let mut def = ReplacementDefinition::new(event);
        if optional {
            def.mode = crate::types::ability::ReplacementMode::Optional { decline: None };
        }
        // CR 121.2: a Draw definition MUST declare whether it modifies the
        // instruction's count or replaces one individual draw. The pipeline
        // debug-asserts on a definition that declares neither; the def-scan this
        // replaces never ran the pipeline, so the fixture could omit it.
        if is_draw {
            def.draw_scope = Some(crate::types::ability::DrawReplacementScope::IndividualDraw);
        }
        // CR 616.1: ordering is a player choice only when it is MATERIAL. Two
        // no-op definitions commute, so a fixture built to exercise the ordering
        // prompt must carry a modification whose composition order matters. The
        // def-scan this replaces counted definitions instead of asking.
        def.quantity_modification =
            Some(crate::types::ability::QuantityModification::Plus { value: 1 });
        def
    }

    /// The board is bound ONCE and handed to both the predicate and the container it is
    /// asked through. That is not a style choice: `PeriodVerdicts::frame_ix` resolves a
    /// frame by POINTER IDENTITY, so building the container from a second, equal-valued
    /// `GameState` would be a frame the container does not hold and the predicate would
    /// fail closed — which is exactly the production guard doing its job, and exactly the
    /// vacuity a copy-pasted expression would introduce here.
    fn specified_on(board: &GameState) -> bool {
        stack_choices_are_all_specified(
            board,
            PlayerId(0),
            &[],
            None,
            &mut PeriodVerdicts::for_period(&[], board, PlayerId(0)),
        )
    }

    /// CR 616.1 + CR 121.1: the draw verdict's environmental obligation is REAL (it can
    /// reject) and CLASS-SCOPED (it rejects only on its own event class).
    ///
    /// Every arm is paired with the control that makes it non-vacuous: the bare board
    /// arm proves a mandatory draw entry reaches and PASSES step (6) at all — without it
    /// the three `false` arms would be indistinguishable from "draws are still refused
    /// upstream" — and the cross-class arms prove the parameterization is load-bearing
    /// rather than a rename of a guard that scans everything.
    #[test]
    fn draw_verdict_obligation_is_real_and_class_scoped() {
        // (i) REACH-GUARD: a bare board with a mandatory draw on the stack PASSES. This
        // is the assertion that flips if `Effect::Draw` goes back to `MayPrompt`.
        assert!(
            specified_on(&with_replacements(draw_entry(10), &[])),
            "a mandatory non-`up to` draw is starved: no replacement environment, no prompt"
        );

        // (ii) CR 702.52a dredge-class: a single OPTIONAL draw replacement prompts.
        assert!(
            !specified_on(&with_replacements(
                draw_entry(10),
                &[repl(ReplacementEvent::Draw, true)]
            )),
            "an optional draw replacement is a genuine CR 608.2d resolution-time choice"
        );

        // (iii) CR 616.1 material ordering: two MANDATORY draw replacements compete.
        // The second one multiplies where the first adds — measured, `Plus` and
        // `Times` are different `CommuteClass`es, so their composition order changes
        // the drawn count and the affected player must order them. Two `Plus` defs
        // COMMUTE and the pipeline correctly opens no prompt for them (measured
        // `replacement_ordering_is_material == false`), which is why this arm cannot
        // be built from two copies of `repl(..)`.
        let material_pair = {
            let mut doubler = repl(ReplacementEvent::Draw, false);
            doubler.quantity_modification =
                Some(crate::types::ability::QuantityModification::Times { factor: 2 });
            [repl(ReplacementEvent::Draw, false), doubler]
        };
        {
            let board = with_replacements(draw_entry(10), &material_pair);
            let drawn_event = crate::types::proposed_event::ProposedEvent::Draw {
                player_id: PlayerId(0),
                count: 1,
                applied: Default::default(),
            };
            let candidates = crate::game::replacement::find_applicable_replacements(
                &board,
                &drawn_event,
                crate::game::replacement::replacement_registry(),
            );
            assert_eq!(
                candidates.len(),
                2,
                "reach-guard: the live candidate authority draws BOTH defs for the \
                 event this entry's resolution proposes"
            );
            assert!(
                crate::game::replacement::replacement_ordering_is_material(
                    &board,
                    &candidates,
                    &drawn_event
                ),
                "reach-guard: those two candidates really are CR 616.1 order-material"
            );
        }
        assert!(
            !specified_on(&with_replacements(draw_entry(10), &material_pair)),
            "CR 616.1: the affected player orders two competing mandatory replacements"
        );

        // (iv) ACCEPT-SIDE control: ONE mandatory, body-less draw replacement (Teferi's
        // Ageless Insight class) resolves deterministically — the guard is not a blanket
        // "any draw replacement rejects".
        assert!(
            specified_on(&with_replacements(
                draw_entry(10),
                &[repl(ReplacementEvent::Draw, false)]
            )),
            "a lone mandatory quantity-mod replacement applies without a prompt"
        );

        // (v) CLASS SCOPING, both directions. A LIFE replacement says nothing about a
        // DRAW-only stack and vice versa; an unparameterized guard would reject both.
        assert!(
            specified_on(&with_replacements(
                draw_entry(10),
                &[repl(ReplacementEvent::GainLife, true)]
            )),
            "an optional LIFE replacement cannot prompt on a draw-only stack"
        );
        assert!(
            specified_on(&with_replacements(
                churn_entry(11, 0, lose_ability(1), None),
                &[repl(ReplacementEvent::Draw, true)]
            )),
            "an optional DRAW replacement cannot prompt on a life-only stack"
        );
        // …and the same optional LIFE replacement DOES reject a life stack, proving the
        // arm above passes because of scoping and not because the def is inert.
        assert!(
            !specified_on(&with_replacements(
                churn_entry(11, 0, lose_ability(1), None),
                &[repl(ReplacementEvent::LoseLife, true)]
            )),
            "positive control: the life guard still rejects its own class"
        );
    }

    // ── §6 R24: the probe resolves on `resolve_top`'s board ──

    /// A drain entry whose `EventContextAmount` resolves against a batched
    /// subject count of `match_count` (CR 603.2c) rather than `churn_entry`'s
    /// fixed `Some(1)` — a distinctive amount is what makes R24(a)'s equality
    /// non-degenerate.
    fn scoped_drain_entry(
        id: u64,
        match_count: Option<u32>,
        condition: Option<TriggerCondition>,
    ) -> StackEntry {
        let mut ability = lose_life_targeting(event_amount(), opp_typed(vec![]));
        ability.targets = vec![TargetRef::Player(PlayerId(1))];
        StackEntry {
            id: ObjectId(id),
            source_id: ObjectId(CHURN_SRC),
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: ObjectId(CHURN_SRC),
                ability: Box::new(ability),
                condition,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: match_count,
                die_result: None,
                provenance: None,
            },
        }
    }

    /// **§6 R24 — THE PROBE RESOLVES ON `resolve_top`'s BOARD: SCOPE BOUND,
    /// ENTRY OFF THE STACK, CR 603.4 RE-CHECKED.**
    ///
    /// Three arms, each keyed to one thing the classifier gets wrong when it is
    /// handed the raw pre-resolution board instead of the one
    /// `resolve_top` hands `resolve_ability_chain`.
    ///
    /// * **(a) THE EVENT-CONTEXT AXIS — the FAIL-OPEN closer.** A
    ///   `LoseLife { amount: Ref(EventContextAmount) }` drain (the Sanguine Bond
    ///   shape) resolves "that many" against the entry's batched subject count
    ///   (CR 603.2c), which only `bind_resolution_scope` lifts. The derived
    ///   `ProposedEvent::LifeLoss.amount` must equal the amount the LIVE
    ///   resolution proposes, and must be non-zero. Without the lift the derived
    ///   amount is 0 — a `> 0`-gated virtual candidate is then never drawn and
    ///   the probe certifies a resolution the live pipeline would prompt on.
    ///   REVERT-PROBE (RUN): hand `probe_resolution` the raw board (drop the
    ///   `bind_resolution_scope` call from `stack_entry_resolution_choice_freedom`)
    ///   ⇒ derived `0` vs live `7` ⇒ FLIPS.
    /// * **(b) CR 603.4.** The same entry with a FALSE intervening-if ⇒
    ///   `bind_resolution_scope` returns `false` ⇒ `MayPrompt`; matched against
    ///   the TRUE twin, which classifies. Direction note, so the arm is not
    ///   over-claimed: skipping the re-check is fail-CLOSED (a superset of
    ///   events draws a superset of candidates), so (b) is a FIDELITY arm —
    ///   (a) is the fail-open closer.
    /// * **(c) AMOUNT-INSENSITIVITY, and the zero arm RE-KEYED ON A
    ///   MEASUREMENT.** On one board with a single in-class (Compleated,
    ///   CR 702.150a) virtual candidate, sweeping the ability's resolved count
    ///   over `{1, 2, 7, 99}` yields an IDENTICAL candidate set — candidate
    ///   selection is amount-insensitive ABOVE zero. The `0` arm does NOT reach
    ///   the zero-payload accounting guard the plan predicted: measured, a
    ///   zero-count resolution proposes no event at all (as do
    ///   `DealDamage { amount: 0 }` and `Draw { count: 0 }`), so the refusal is
    ///   the `is_empty` arm and the plan's stated revert-probe for this arm
    ///   cannot reproduce. Both facts are asserted in place, with the
    ///   guard's own classification pinned separately on the partition. See the
    ///   block comment at the arm.
    ///
    /// REACH-GUARDS on every arm: `bind_resolution_scope` is asserted to have
    /// returned `true` and the probe to have returned `Events(..)` on each
    /// positive arm, so a board that refuses for an unrelated reason fails
    /// LOUDLY instead of passing a negative vacuously.
    #[test]
    fn the_probe_resolves_on_resolve_tops_board_with_scope_bound_and_603_4_rechecked() {
        use crate::game::resolution_prompt::ResolutionChoiceFreedom;
        use crate::types::proposed_event::ProposedEvent;

        // ── (a) the event-context axis ──
        const MATCHES: u32 = 7;
        let mut state = drain_state(2);
        let entry = scoped_drain_entry(20, Some(MATCHES), None);
        state.stack.push_back(entry.clone());

        // Reach-guard: the binding this arm is about actually succeeds.
        let mut board = state.clone();
        board.stack.retain(|e| e.id != entry.id);
        assert!(
            crate::game::stack::bind_resolution_scope(&mut board, &entry, None),
            "reach-guard: no CR 603.4 condition on this entry ⇒ the scope binds"
        );

        let freedom = stack_entry_resolution_choice_freedom(
            &state,
            &entry,
            &mut ProbeBudget::for_test(PROBE_BUDGET),
        );
        let ResolutionChoiceFreedom::FreeUnlessReplacements(derived) = freedom else {
            panic!("reach-guard: the probe must return Events(..) on this board, got {freedom:?}");
        };
        let derived_amount = derived
            .iter()
            .find_map(|event| match event {
                ProposedEvent::LifeLoss {
                    player_id, amount, ..
                } if *player_id == PlayerId(1) => Some(*amount),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no LifeLoss on P1 in the derived set: {derived:?}"));

        let mut live = state.clone();
        let before = live.players[1].life;
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut live, &mut events);
        let live_amount = before - live.players[1].life;
        assert_eq!(
            i64::from(derived_amount),
            i64::from(live_amount),
            "CR 603.2c + CR 608.2k: the derived amount must equal the one the LIVE \
             resolution proposes — an unbound scope resolves EventContextAmount \
             against an absent context"
        );
        assert_eq!(
            derived_amount, MATCHES,
            "non-degeneracy: the amount is the lifted batched subject count, not zero \
             and not a coincidental 1"
        );

        // ── (b) CR 603.4 intervening-if re-check ──
        // `drain_state` builds a standard-format board (20 starting life; the `7`
        // it passes is the RNG seed), so `LifeTotalGE 6` is TRUE and `LifeTotalGE 30`
        // FALSE for the entry's controller.
        for (label, condition, binds) in [
            ("TRUE", TriggerCondition::LifeTotalGE { minimum: 6 }, true),
            (
                "FALSE",
                TriggerCondition::LifeTotalGE { minimum: 30 },
                false,
            ),
        ] {
            let mut s = drain_state(2);
            let e = scoped_drain_entry(21, Some(MATCHES), Some(condition));
            s.stack.push_back(e.clone());

            let mut b = s.clone();
            b.stack.retain(|x| x.id != e.id);
            assert_eq!(
                crate::game::stack::bind_resolution_scope(&mut b, &e, None),
                binds,
                "reach-guard: the CR 603.4 re-check is what decides arm ({label})"
            );

            let verdict = stack_entry_resolution_choice_freedom(
                &s,
                &e,
                &mut ProbeBudget::for_test(PROBE_BUDGET),
            );
            if binds {
                assert!(
                    matches!(verdict, ResolutionChoiceFreedom::FreeUnlessReplacements(_)),
                    "the condition-TRUE twin classifies ({label}); got {verdict:?}"
                );
            } else {
                assert_eq!(
                    verdict,
                    ResolutionChoiceFreedom::MayPrompt,
                    "CR 603.4: a FALSE intervening-if means the live resolution proposes \
                     NOTHING, and an empty derivation is never safe ({label})"
                );
            }
        }

        // ── (c) amount-insensitivity above zero, and the zero-payload guard ──
        let mut counter_state = drain_state(2);
        {
            // CR 702.150a: the Compleated virtual AddCounter candidate is drawn
            // only for a loyalty placement on a source whose Phyrexian life was paid.
            let src = counter_state
                .objects
                .get_mut(&ObjectId(CHURN_SRC))
                .expect("fixture: the churn source exists");
            src.phyrexian_life_paid = 2;
            src.keywords
                .push(crate::types::keywords::Keyword::Compleated);
        }
        let loyalty_ability = |count: i32| {
            ResolvedAbility::new(
                Effect::PutCounter {
                    target: TargetFilter::SelfRef,
                    counter_type: crate::types::counter::CounterType::Loyalty,
                    count: QuantityExpr::Fixed { value: count },
                },
                vec![],
                ObjectId(CHURN_SRC),
                PlayerId(0),
            )
        };
        let counter_entry = |count: i32| {
            let mut e = scoped_drain_entry(22, Some(MATCHES), None);
            let StackEntryKind::TriggeredAbility { ability, .. } = &mut e.kind else {
                unreachable!("scoped_drain_entry builds a TriggeredAbility")
            };
            **ability = loyalty_ability(count);
            e
        };

        let mut candidate_sets = Vec::new();
        for count in [1, 2, 7, 99] {
            let e = counter_entry(count);
            let mut s = counter_state.clone();
            s.stack.push_back(e.clone());
            let verdict = stack_entry_resolution_choice_freedom(
                &s,
                &e,
                &mut ProbeBudget::for_test(PROBE_BUDGET),
            );
            let ResolutionChoiceFreedom::FreeUnlessReplacements(events) = verdict else {
                panic!("reach-guard: count {count} must probe to Events(..), got {verdict:?}");
            };
            let add = events
                .iter()
                .find(|event| matches!(event, ProposedEvent::AddCounter { .. }))
                .unwrap_or_else(|| panic!("count {count} derived no AddCounter: {events:?}"));
            candidate_sets.push(crate::game::replacement::find_applicable_replacements(
                &s,
                add,
                crate::game::replacement::replacement_registry(),
            ));
        }
        assert!(
            !candidate_sets[0].is_empty(),
            "reach-guard: the CR 702.150a Compleated virtual candidate IS drawn above \
             zero — without it the sweep would compare four empty sets"
        );
        assert!(
            candidate_sets.windows(2).all(|pair| pair[0] == pair[1]),
            "CR 614.1a: candidate SELECTION is amount-insensitive above zero; got \
             {candidate_sets:?}"
        );

        // The `0` arm, RE-KEYED ON A MEASUREMENT that contradicts the plan's
        // stated mechanism — recorded here rather than papered over.
        //
        // The plan expects a zero-count `PutCounter` to DERIVE an
        // `AddCounter { count: 0 }` which the zero-payload guard then classifies
        // Unaccounted (arm 4). Measured on this board: the zero-count resolution
        // proposes NOTHING AT ALL — and so do `DealDamage { amount: 0 }` and
        // `Draw { count: 0 }`, the other two zero-payload classes. Every counter/
        // damage/draw resolver short-circuits above the pipeline at zero. So no
        // zero-payload `ProposedEvent` is reachable through the six allow-listed
        // classes, the refusal below is arm 3 (`is_empty`), and the plan's
        // (c) revert-probe (delete the `AddCounter { count: 0 }` guard ⇒ the
        // derivation certifies) CANNOT REPRODUCE — there is no derivation to
        // certify. DIRECTION: fail-CLOSED either way, so this is a coverage fact,
        // not a hole. The guards remain correct defence-in-depth for events
        // proposed by non-allow-listed routes and are pinned directly on the
        // partition in BOTH directions by
        // `resolution_prompt::tests::an_unaccounted_derived_event_is_prompted_in_the_resolver`.
        let zero_entry = counter_entry(0);
        let mut zero_state = counter_state.clone();
        zero_state.stack.push_back(zero_entry.clone());
        let mut zero_board = zero_state.clone();
        zero_board.stack.retain(|e| e.id != zero_entry.id);
        assert!(
            crate::game::stack::bind_resolution_scope(&mut zero_board, &zero_entry, None),
            "reach-guard: the zero arm's refusal is not the CR 603.4 arm"
        );
        let zero_events = crate::game::replacement::record_proposed_events(|| {
            let mut work = zero_board.clone();
            let mut sink = Vec::new();
            let _ = crate::game::effects::resolve_ability_chain(
                &mut work,
                &loyalty_ability(0),
                &mut sink,
                0,
            );
        });
        assert!(
            zero_events.is_empty(),
            "MEASURED, and the reason the arm is re-keyed: a zero count proposes no \
             event at all; recorded {zero_events:?}. If this ever becomes non-empty the \
             accounting arm becomes the reachable one and this arm must be re-keyed \
             back onto it."
        );
        assert!(
            !crate::game::replacement::event_is_accounted(&ProposedEvent::AddCounter {
                placement: crate::types::proposed_event::CounterPlacement::Object {
                    actor: PlayerId(0),
                    object_id: ObjectId(CHURN_SRC),
                    counter_type: crate::types::counter::CounterType::Loyalty,
                },
                count: 0,
                applied: Default::default(),
            }),
            "the CR 702.150a zero-payload guard still classifies the event Unaccounted \
             — it is simply not reachable from an allow-listed resolution"
        );
        assert_eq!(
            stack_entry_resolution_choice_freedom(
                &zero_state,
                &zero_entry,
                &mut ProbeBudget::for_test(PROBE_BUDGET)
            ),
            ResolutionChoiceFreedom::MayPrompt,
            "CR 732.2a: at zero the live pipeline draws no candidate (`count > 0`) and \
             the resolution proposes nothing, so the probe refuses fail-CLOSED rather \
             than certifying an empty derivation"
        );
    }

    /// CR 732.2a: `BUDGET-EXCEEDED ⇒ Prompted` is what keeps the probe's cost a
    /// COVERAGE knob rather than an unbounded player-facing stall, so the budget
    /// must actually stop granting and must LATCH the denial (an exhaustion that
    /// is only inferable from a zero remainder cannot be attributed).
    ///
    /// Both directions asserted so neither can go vacuous: exactly
    /// [`PROBE_BUDGET`] charges are granted with `denied()` still `false`, and
    /// only the charge AFTER that flips it.
    #[test]
    fn probe_budget_grants_exactly_its_cap_then_latches_the_denial() {
        let mut budget = ProbeBudget::for_test(PROBE_BUDGET);
        for i in 0..PROBE_BUDGET {
            assert!(
                budget.try_charge_one(),
                "charge {i} of {PROBE_BUDGET} must be granted"
            );
            assert!(
                !budget.denied(),
                "no denial may be latched while charges are still granted (after {i})"
            );
        }
        assert!(
            !budget.try_charge_one(),
            "the charge past the cap must be refused"
        );
        assert!(
            budget.denied(),
            "the exhaustion fact must be latched, not inferred from the remainder"
        );
        // A zero-cap budget denies its FIRST charge — the shape a lowered cap
        // takes, and the reason exhaustion is fail-closed rather than silent.
        let mut starved = ProbeBudget::for_test(0);
        assert!(!starved.try_charge_one());
        assert!(starved.denied());
    }
    // ───────────────── 5d U2 — the shape-(B) mint's relief-side rows ─────────────────

    /// A 3-seat board with one battlefield source, shared by the two U2 relief rows.
    fn u2_relief_board() -> (GameState, ObjectId) {
        use crate::game::scenario::GameScenario;
        let mut state = GameScenario::new_n_player(3, 7).build().state().clone();
        let src = ObjectId(970);
        let mut obj = crate::game::game_object::GameObject::new(
            src,
            crate::types::identifiers::CardId(0),
            PlayerId(0),
            "U2 Source".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        obj.incarnation = 3;
        state.objects.insert(src, obj);
        (state, src)
    }

    /// Shape (B): a proposer-controlled OPTIONAL, NO-TARGET triggered ability.
    ///
    /// Shape (B) is reached whenever `build_target_slots` yields ZERO slots. There are two
    /// routes to that, and the difference matters HERE and not at the mint: an effect whose
    /// head filter announces nothing (`Draw { target: Controller }`) leaves the residual
    /// classification choice-FREE, while `TargetChoiceTiming::Resolution` (the Braids
    /// per-player-upkeep shape the mint rows use) is itself one of the six `MayPrompt`
    /// reasons — such an entry mints but can never be RELIEVED, so its offer is refused at
    /// conjunct (6). That is the fail-closed direction, and it is why the relief rows below
    /// take the announce-nothing route rather than the resolution-timing one.
    fn u2_shape_b_entry(
        src: ObjectId,
        id: u64,
        effect: crate::types::ability::Effect,
        mutate: impl FnOnce(&mut crate::types::ability::ResolvedAbility),
    ) -> StackEntry {
        let mut ability =
            crate::types::ability::ResolvedAbility::new(effect, vec![], src, PlayerId(0));
        ability.optional = true;
        mutate(&mut ability);
        StackEntry {
            id: ObjectId(id),
            source_id: src,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: src,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        }
    }

    fn u2_draw_effect() -> crate::types::ability::Effect {
        crate::types::ability::Effect::Draw {
            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            target: crate::types::ability::TargetFilter::Controller,
        }
    }

    /// [`u2_scope`] with the proposer as a PARAMETER — the seat whose offer published the pins,
    /// which is not always the seat the consuming container is bound to (R22 conjunct (4)).
    fn u3_scope_for(proposer: PlayerId, slots: &[DecisionSlot]) -> LoopWindowScope<'_> {
        LoopWindowScope {
            phase_invariant: None,
            sole_driver: None,
            pinned: Some(PinnedChoices { proposer, slots }),
            cast_card_ids: None,
            period: None,
        }
    }

    /// The `LoopWindowScope` an offer that published exactly `slots` hands the relief.
    fn u2_scope(slots: &[DecisionSlot]) -> LoopWindowScope<'_> {
        LoopWindowScope {
            phase_invariant: None,
            sole_driver: None,
            pinned: Some(PinnedChoices {
                proposer: PlayerId(0),
                slots,
            }),
            cast_card_ids: None,
            period: None,
        }
    }

    /// R6 — **an optional trigger carrying an additional unpublishable axis still refuses,
    /// and the RELIEF is the layer that refuses it.**
    ///
    /// `ability_resolution_choice_freedom` returns `MayPrompt` for six independent reasons and
    /// the offer publishes a `MayChoice` point for exactly ONE of them (`ability.optional`).
    /// `pinned_may_choice_relief` therefore re-classifies the ability with `optional` cleared:
    /// an `unless_pay` (CR 118.12) keeps coming back `MayPrompt` and gets no relief, because
    /// no published pin specifies it.
    ///
    /// **(a′) THE MATCHED POSITIVE (ROUND-42 M15), byte-identical except the axis.** Without
    /// it this row is a dominated negative: `entry_publishes_pin_slots` returns `None` from
    /// four conjuncts that all sit ABOVE R6's axis (`entry.controller != proposer`, the
    /// `TriggeredAbility` let-else, and the `multi_target`/`distribution`/`target_constraints`
    /// block), so a fixture tripping any of them would refuse for a reason that has nothing to
    /// do with the residual re-classification. The positive proves the fixture is
    /// proposer-controlled, is a triggered ability, carries none of those three, and reaches
    /// the mint — and this row additionally asserts the negative fixture MINTS, so the refusal
    /// is attributable to the relief and to nothing upstream of it.
    ///
    /// SCOPE, so this row is not read as covering the cardinality axis: `unless_pay` and a
    /// modal header are axes the relief catches. A `repeat_for`-driven multi-prompt ability
    /// does NOT fail that way — it can re-classify choice-free while the resolution still
    /// opens N prompts — so it is caught one layer earlier, at the mint, by
    /// `one_published_may_slot_stands_for_exactly_one_cr_603_5_prompt`.
    ///
    /// REVERT-PROBE: make `pinned_may_choice_relief` return the residual without
    /// re-classifying (drop the `without_may_gate` round-trip and return
    /// `FreeUnlessReplacements` unconditionally) ⇒ the `unless_pay` arm is relieved ⇒ FLIPS.
    #[test]
    fn an_unpublishable_residual_axis_is_refused_by_the_relief_not_by_the_mint() {
        use crate::game::engine::entry_publishes_pin_slots;
        use crate::game::resolution_prompt::ResolutionChoiceFreedom;

        let (state, src) = u2_relief_board();

        // ── (a′) matched positive: no residual axis ──
        let clean = u2_shape_b_entry(src, 980, u2_draw_effect(), |_| {});
        let published = entry_publishes_pin_slots(&state, &clean, PlayerId(0))
            .expect("(a′) reach-guard: the clean fixture must reach the mint");
        let may = published
            .may
            .expect("(a′) the clean fixture publishes its CR 603.5 gate");
        assert!(
            published.target.is_none(),
            "(a′) shape (B): no announcement choice, so no target slot"
        );
        let slots = vec![may.clone()];
        // The relief reads the mint and the residual through the ONE door, so the row drives
        // a real container bound to the same proposer the pins carry; `frame_ix` is the only
        // `FrameIx` mint and its `None` would be a refusal, so the `expect` is a reach-guard.
        let mut verdicts = PeriodVerdicts::for_period(&[], &state, PlayerId(0));
        let f = verdicts
            .frame_ix(&state)
            .expect("(a′) reach-guard: the container holds the board the relief is asked about");
        assert!(
            matches!(
                pinned_may_choice_relief(f, &clean, &mut verdicts, u2_scope(&slots)),
                Some(ResolutionChoiceFreedom::FreeUnlessReplacements(_))
            ),
            "(a′) with the may pinned and no other axis, the residual classification is \
             choice-free and the entry is relieved"
        );

        // ── (a) the negative: one CR 118.12 `unless_pay` axis, nothing else changed ──
        let gated = u2_shape_b_entry(src, 981, u2_draw_effect(), |ability| {
            ability.unless_pay = Some(crate::types::ability::UnlessPayModifier {
                cost: crate::types::ability::AbilityCost::Mana {
                    cost: crate::types::mana::ManaCost::Cost {
                        shards: vec![],
                        generic: 2,
                    },
                },
                payer: crate::types::ability::TargetFilter::Controller,
            });
        });
        let gated_published = entry_publishes_pin_slots(&state, &gated, PlayerId(0))
            .expect("(a) reach-guard: the MINT still publishes — it does not read `unless_pay`");
        assert_eq!(
            gated_published.may.as_ref(),
            Some(&may),
            "(a) reach-guard: the same slot is published, so the two arms differ ONLY in the \
             residual axis and the refusal below cannot come from the mint"
        );
        assert!(
            pinned_may_choice_relief(f, &gated, &mut verdicts, u2_scope(&slots)).is_none(),
            "(a) CR 118.12: an `unless_pay` is a SECOND resolution-time choice no published \
             pin specifies, so the residual re-classification still returns `MayPrompt` and \
             the entry gets no relief"
        );
    }

    /// R31 — **the `may` mint's recipient conjunct may read the board, but it can never move
    /// a published offer.**
    ///
    /// Conjunct (a) calls `optional_prompt_player`, whose sole state-touching callee is
    /// `targeting::resolve_effect_player_ref`, reaching eleven distinct `GameState` fields
    /// (`players`, `seat_order`, `format_config`, `objects`, `lki_cache`, `stack`,
    /// `current_trigger_event`, `last_created_token_ids`, `last_revealed_ids`,
    /// `last_zone_changed_ids`, `resolution_stack`). Every one of the three branches that
    /// reach it is gated on an `Effect` that `effect_resolution_choice_freedom` puts in its
    /// fail-closed grouped arm — so conjunct (6) refuses any offer carrying such an entry.
    /// The reads happen; they cannot bear on a published result.
    ///
    /// NO PRODUCTION DELTA: this row pins an ARGUMENT, which is why it needs a revert-probe
    /// that edits code rather than deletes a guard.
    ///
    /// THREE ARMS, and the first two exist so the third cannot pass vacuously:
    /// * **(a) the branch is REACHED** — `Effect::PayCost { payer: Controller }` routes
    ///   through `resolve_effect_player_ref`'s `Controller` arm and returns the proposer ⇒ a
    ///   `may` slot IS published.
    /// * **(a′) matched negative, differing in exactly the payer filter** — `payer: Opponent`
    ///   resolves through `players::is_opponent`/`opponents` to a seat ≠ proposer ⇒ no slot.
    ///   The pair proves the verdict is decided BY the callee's return value, which is why no
    ///   function-level inertness is claimed anywhere.
    /// * **(b) the closure** — the offer is refused anyway, repeated for all three
    ///   state-reading branches (`PayCost`, `Sacrifice`, `SearchLibrary`) so the arm covers
    ///   the closure's whole population rather than one member of it.
    ///
    /// REVERT-PROBE (symbol-anchored, so it survives file moves): in
    /// `game/resolution_prompt.rs`, move `Effect::PayCost { .. }` out of
    /// `effect_resolution_choice_freedom`'s fail-closed grouped arm into a
    /// `FreeUnlessReplacements(vec![])` arm ⇒ **(b) FLIPS to `true`** for the `PayCost` case
    /// while (a)/(a′) stay green — proving the closure rests on the scope filter and not on
    /// the fixture.
    ///
    /// The COMPLETENESS arm ("every minted pair is scanned") ships in U3: its probe names
    /// `touch.announced`, which does not exist until the announcement loop gains its window.
    #[test]
    fn the_recipient_conjunct_reads_the_board_but_can_never_move_a_published_offer() {
        use crate::game::engine::entry_publishes_pin_slots;
        use crate::types::ability::{AbilityCost, Effect, QuantityExpr, TargetFilter};

        let (state, src) = u2_relief_board();
        let mana = || AbilityCost::Mana {
            cost: crate::types::mana::ManaCost::Cost {
                shards: vec![],
                generic: 1,
            },
        };
        let pay_cost = |payer: TargetFilter| Effect::PayCost {
            cost: mana(),
            scale: None,
            payer,
        };

        // ── (a) the state-reading branch is REACHED and returns the proposer ──
        let reached = u2_shape_b_entry(src, 990, pay_cost(TargetFilter::Controller), |_| {});
        let StackEntryKind::TriggeredAbility { ability, .. } = &reached.kind else {
            panic!("the fixture is a triggered ability");
        };
        assert_eq!(
            crate::game::effects::optional_prompt_player(&state, ability),
            PlayerId(0),
            "(a) reach-guard: the `PayCost` branch really routes through \
             `resolve_effect_player_ref`'s `Controller` arm and returns the proposer"
        );
        let published = entry_publishes_pin_slots(&state, &reached, PlayerId(0))
            .expect("(a) the state-reading branch publishes");
        let may = published
            .may
            .expect("(a) a `may` slot IS minted through the state-reading branch");

        // ── (a′) matched negative: exactly the payer filter differs ──
        let opposed = u2_shape_b_entry(src, 991, pay_cost(TargetFilter::Opponent), |_| {});
        let StackEntryKind::TriggeredAbility { ability, .. } = &opposed.kind else {
            panic!("the fixture is a triggered ability");
        };
        assert_ne!(
            crate::game::effects::optional_prompt_player(&state, ability),
            PlayerId(0),
            "(a′) reach-guard: the `Opponent` arm resolves to a seat that is NOT the proposer"
        );
        assert!(
            entry_publishes_pin_slots(&state, &opposed, PlayerId(0)).is_none(),
            "(a′) the mint's verdict is decided by `resolve_effect_player_ref`'s RETURN value, \
             not by the effect's shape — so the state reads really are result-bearing"
        );

        // ── (b) the closure: the offer is refused anyway, on all three branches ──
        let branches: Vec<(&str, Effect)> = vec![
            ("PayCost", pay_cost(TargetFilter::Controller)),
            (
                "Sacrifice",
                Effect::Sacrifice {
                    target: TargetFilter::ParentTargetController,
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 1,
                },
            ),
            (
                "SearchLibrary",
                Effect::SearchLibrary {
                    source_zones: vec![crate::types::zones::Zone::Library],
                    filter: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 1 },
                    reveal: false,
                    target_player: Some(TargetFilter::ParentTargetController),
                    selection_constraint: Default::default(),
                    split: None,
                },
            ),
        ];
        for (label, effect) in branches {
            let mut board = state.clone();
            let entry = u2_shape_b_entry(src, 992, effect, |_| {});
            // Reach-guard for the ANNOUNCEMENT loop: without this, a `false` below could come
            // from gate (3) instead of from gate (6) and the closure claim would be vacuous.
            assert!(
                stack_entry_has_no_ordering_input(&board, &entry),
                "{label}: reach-guard — shape (B) announces no choice, so the announcement \
                 loop must PASS and the refusal below is attributable to gate (6)"
            );
            board.stack.push_back(entry);
            assert!(
                !stack_choices_are_all_specified(
                    &board,
                    PlayerId(0),
                    std::slice::from_ref(&may),
                    None,
                    &mut PeriodVerdicts::for_period(&[], &board, PlayerId(0))
                ),
                "{label}: CR 732.2a conjunct (6) refuses the offer — the effect sits in \
                 `effect_resolution_choice_freedom`'s fail-closed grouped arm, so a `may` \
                 slot minted through a state-reading branch can never reach a published offer"
            );
        }
    }

    /// R33 arms (a) / (b) / (a′1) / (c) — THE FROZEN EXEMPTION IS KEYED TO THE CERTIFYING
    /// DISJUNCT, AT THE CONSTRUCTOR **AND** AT THE CONSUMER, ON A REAL DRIVEN WINDOW.
    ///
    /// CR 732.2a + CR 608.1. The exemption's limb (*observed-frozen ⇒ frozen across the
    /// fast-forward*) needs P2 (the period cannot SHRINK the stack) and P4 (the fast-forward
    /// IS the repetition of the observed period). Exactly one certifying disjunct supplies
    /// both, so `frozen_ids` is non-empty under `BoardCovered` and EMPTY under the other two.
    ///
    /// The board is the dellian dump DRIVEN through `apply()` — the dump ships with an empty
    /// ring, so every frame the window is built from was accumulated by this drive. The beat
    /// is SEARCHED FOR by its construction requirements rather than hardcoded, because a
    /// hardcoded beat index is a fixture that drifts silently when the drive policy moves.
    ///
    /// (a) the window really freezes something — asserted against an INDEPENDENT lower bound
    /// ([`frozen_lower_bound`]), never against the callee's own answer. (b)/(a′1) the two
    /// matched negatives, differing from (a) in EXACTLY ONE ARGUMENT. (c) the consumer half:
    /// the same two touches through `stack_choices_are_all_specified`, where the boolean flip
    /// is the load-bearing assertion and the counters are its attribution.
    ///
    /// REVERT-PROBE 1: delete `certified_period_touch`'s pre-walk early return so `frozen_ids`
    /// is computed unconditionally ⇒ (b) and (a′1) FLIP from empty to the full set and (c)'s
    /// two arms collapse to equal populations.
    #[test]
    fn r33_frozen_exemption_is_keyed_to_the_certificate_on_the_real_dellian_window() {
        let mut state = dump_state(include_bytes!(
            "../../tests/fixtures/dellian_emblem_conqueror_4p.json.gz"
        ));
        assert_eq!(
            state.loop_detect_ring.len(),
            0,
            "reach-guard: the dump must ship with an EMPTY ring — every frame the window \
             below is built from is accumulated by THIS drive, not restored"
        );

        // Drive until a beat satisfies the row's construction requirements: a usable ring
        // AND a window that genuinely freezes something. Both are checked BEFORE the board
        // is captured, so the arms below cannot run on a beat that does not carry them.
        let mut hit: Option<(usize, GameState)> = None;
        for beat in 0..80usize {
            if state.loop_detect_ring.len() >= 2 {
                let live: Vec<&GameState> =
                    state.loop_detect_ring.iter().map(|f| &f.live).collect();
                let window = &live[live.len() - 2..];
                if frozen_lower_bound(window, &state) > 0 {
                    drop(live);
                    hit = Some((beat, state.clone()));
                    break;
                }
            }
            if dump_drive_one_beat(&mut state).is_err() {
                break;
            }
        }
        let (beat, board) = hit.expect(
            "REACH-GUARD: no driven beat carried a ring >= 2 frames AND a window with a \
             non-empty observed-frozen prefix. Every arm below would be vacuous on such a \
             beat, so the row FAILS rather than passing over a window that freezes nothing",
        );

        let live: Vec<&GameState> = board.loop_detect_ring.iter().map(|f| &f.live).collect();
        // The newest candidate pair, i.e. `span == 1` — the shape §3 D2's walk reaches first.
        let window = &live[live.len() - 2..];
        let bound = frozen_lower_bound(window, &board);
        let proposer = board.active_player;

        // ── (a) the exemption is genuinely AVAILABLE on this window ──────────────────────
        let cover = certified_period_touch(window, &board, PeriodCertification::BoardCovered);
        assert!(
            cover.frozen_ids.len() >= bound && bound > 0,
            "(a) beat {beat}: the cover certificate must freeze at least the {bound} entries \
             the independent common-prefix bound proves are index-stable across every window \
             frame; got {}",
            cover.frozen_ids.len()
        );

        // ── (b) / (a′1) the two matched negatives, ONE argument different ────────────────
        let sig =
            certified_period_touch(window, &board, PeriodCertification::ResourceSignatureOnly);
        assert!(
            sig.frozen_ids.is_empty(),
            "(b) beat {beat}: basis B consults no board predicate, so it supplies neither P2 \
             nor P4 and the subtraction is withdrawn; got {} frozen",
            sig.frozen_ids.len()
        );
        let eq = certified_period_touch(window, &board, PeriodCertification::BoardEqualOnly);
        assert!(
            eq.frozen_ids.is_empty(),
            "(a′1) beat {beat}: the equality disjunct supplies P2 but NOT P4 — it has no \
             items (4)/(5) — so it is as fail-closed as basis B; got {} frozen",
            eq.frozen_ids.len()
        );

        // ANTI-OVER-NARROWING: only the SUBTRACTION is keyed. A certificate change must not
        // zero the whole touch — that would under-publish the mint instead of un-exempting.
        let ids = |t: &PeriodTouch<'_>| -> Vec<ObjectId> {
            t.announced.iter().map(|(_, e)| e.id).collect()
        };
        assert_eq!(
            ids(&sig),
            ids(&cover),
            "the certificate keys the frozen subtraction ONLY: `announced` must be \
             element-for-element identical under every value"
        );
        assert_eq!(ids(&eq), ids(&cover), "same, for the equality value");

        // ── (c) THE CONSUMER HALF: the same two touches, through conjunct (6) ────────────
        // Fresh containers per arm — a shared one would carry a warm memo and a part-spent
        // budget into the second arm, making the counters incomparable.
        let mut v_cover = PeriodVerdicts::for_period(&live, &board, proposer);
        let c1 = stack_choices_are_all_specified(&board, proposer, &[], Some(&cover), &mut v_cover);
        let mut v_sig = PeriodVerdicts::for_period(&live, &board, proposer);
        let c2 = stack_choices_are_all_specified(&board, proposer, &[], Some(&sig), &mut v_sig);

        // The BOOLEAN FLIP is the load-bearing assertion; the counters are its attribution.
        assert!(
            c1 && !c2,
            "(c) beat {beat}: conjunct (6) must ACCEPT under the exempting certificate and \
             REFUSE under the non-exempting one. Measured c1={c1} (asks={}, skips={}, \
             spent={}, denied={}) c2={c2} (asks={}, skips={}, spent={}, denied={}), stack={}",
            v_cover.conjunct6_asks(),
            v_cover.conjunct6_frozen_skips(),
            v_cover.spent(),
            v_cover.denied(),
            v_sig.conjunct6_asks(),
            v_sig.conjunct6_frozen_skips(),
            v_sig.spent(),
            v_sig.denied(),
            board.stack.len()
        );
        assert!(
            v_cover.conjunct6_frozen_skips() as usize >= bound,
            "(c1) the accepted arm must have SKIPPED the frozen ids rather than classified \
             them; skips={} bound={bound}",
            v_cover.conjunct6_frozen_skips()
        );
        assert!(
            !v_cover.denied() && v_cover.spent() <= PROBE_BUDGET,
            "(c1) the exempted classification must fit the cap: spent={} denied={}",
            v_cover.spent(),
            v_cover.denied()
        );
        assert_eq!(
            v_sig.conjunct6_frozen_skips(),
            0,
            "(c2) the non-exempting certificate must skip NOTHING"
        );
        assert!(
            v_sig.denied() && v_sig.spent() == PROBE_BUDGET,
            "(c2) the unexempted sweep must exhaust the cap and refuse fail-closed — its \
             measured demand is far above it. spent={} denied={} cap={PROBE_BUDGET}",
            v_sig.spent(),
            v_sig.denied()
        );
    }

    /// R21 (b) + (b-unproven) — THE EXEMPTION NARROWS CONJUNCT (6)'s POPULATION BY EXACTLY
    /// THE FROZEN SET, AND "NO PROOF" NARROWS IT BY NOTHING.
    ///
    /// CR 732.2a + CR 608.1. Both TRACKED dumps, driven through `apply()` to the first beat
    /// carrying a real certified window with a non-empty observed-frozen prefix.
    ///
    /// ⚠ THE PLAN'S FIGURES FOR THIS ROW ARE HEAD-ERA AND ARE RE-MEASURED HERE, NOT COPIED.
    /// It expects *"`conjunct6_asks()` = 2–4, `conjunct6_frozen_skips()` = 152, their sum =
    /// `current.stack.len()` = 154–156"*. Measured on the driven tree (`dellian` beat 14,
    /// `ring=2 stack=154 announced=2 frozen=152`): `asks=4`, `skips=152`, **sum = 156**, and
    /// `156 = announced + stack`, NOT `stack` — post-U3 the predicate's domain is
    /// `touch.announced ∪ (stack \ frozen)`, so the announced pairs are asks the HEAD-era
    /// figure could not include. The SUM IDENTITY is asserted in the corrected form; it is
    /// what fails if a future edit exempts an entry without counting it, which a bare
    /// `asks == 2..4` would not see. (`dina` beat 10: `stack=8 announced=3 frozen=5`,
    /// `asks=6 skips=5`, sum `11 = 3 + 8`.)
    ///
    /// (b-unproven): the shipped meaning of NO PROOF is `touch == None`, and it must exempt
    /// NOTHING — `skips == 0` on both dumps, and where the sweep completes, the ask count is
    /// the UNCONDITIONED `current.stack`. Without this arm the exemption could be made
    /// unconditional and nothing would fail.
    ///
    /// REVERT-PROBE: delete the `frozen_ids` skip from `stack_choices_are_all_specified`'s
    /// current-stack loop ⇒ `skips` drops to 0 while `frozen_ids` stays non-empty ⇒ the
    /// `skips == frozen_ids.len()` arm FLIPS on both dumps.
    #[test]
    fn r21_b_the_exemption_narrows_conjunct_six_by_exactly_the_frozen_set() {
        // Set by whichever dump's UNPROVEN sweep runs to completion, so the population claim
        // below is asserted against a COMPLETE count and never against a truncated one.
        let mut unproven_population_witness: Option<(&str, usize, usize)> = None;

        for (name, gz) in TRACKED_DUMPS {
            let (beat, board) = drive_dump_until(gz, 80, has_frozen_window).unwrap_or_else(|| {
                panic!(
                    "REACH-GUARD [{name}]: no driven beat carried a ring >= 2 frames AND a \
                     window with a non-empty observed-frozen prefix; every arm below would be \
                     vacuous on such a beat"
                )
            });
            let live: Vec<&GameState> = board.loop_detect_ring.iter().map(|f| &f.live).collect();
            let window = &live[live.len() - 2..];
            let proposer = board.active_player;
            let cover = certified_period_touch(window, &board, PeriodCertification::BoardCovered);

            // ── reach-guards: neither half of the domain may be empty ────────────────────
            assert!(
                !cover.frozen_ids.is_empty(),
                "[{name}] beat {beat}: the exemption must have something to remove"
            );
            assert!(
                !cover.announced.is_empty(),
                "[{name}] beat {beat}: the announced half must be non-empty, else the sum \
                 identity below degenerates into a statement about `current.stack` alone"
            );
            assert!(
                cover.frozen_ids.len() < board.stack.len(),
                "[{name}] beat {beat}: a fully-frozen stack would make `asks` zero and the \
                 narrowing unobservable; frozen {} of {}",
                cover.frozen_ids.len(),
                board.stack.len()
            );

            let measure = |touch: Option<&PeriodTouch<'_>>| {
                let mut v = PeriodVerdicts::for_period(&live, &board, proposer);
                let r = stack_choices_are_all_specified(&board, proposer, &[], touch, &mut v);
                (
                    r,
                    v.conjunct6_asks() as usize,
                    v.conjunct6_frozen_skips() as usize,
                    v.spent(),
                    v.denied(),
                )
            };

            // ── (b) the exempting certificate ────────────────────────────────────────────
            let (r_cov, asks_cov, skips_cov, spent_cov, denied_cov) = measure(Some(&cover));
            assert!(
                r_cov,
                "[{name}] beat {beat}: the exempted sweep must RUN TO COMPLETION, else every \
                 count below is a truncation rather than a population. asks={asks_cov} \
                 skips={skips_cov} spent={spent_cov} denied={denied_cov}"
            );
            assert_eq!(
                skips_cov,
                cover.frozen_ids.len(),
                "[{name}] beat {beat}: conjunct (6) must skip EXACTLY the proven-frozen ids — \
                 no more (an over-skip is the fail-open direction) and no fewer"
            );
            assert_eq!(
                asks_cov + skips_cov,
                cover.announced.len() + board.stack.len(),
                "[{name}] beat {beat}: THE SUM IDENTITY. Every member of the derived domain \
                 `announced ∪ current.stack` is either ASKED or COUNTED as exempt; an entry \
                 exempted without being counted would land here. asks={asks_cov} \
                 skips={skips_cov} announced={} stack={}",
                cover.announced.len(),
                board.stack.len()
            );

            // ── (b-unproven) NO PROOF exempts NOTHING ────────────────────────────────────
            let (r_none, asks_none, skips_none, spent_none, denied_none) = measure(None);
            assert_eq!(
                skips_none, 0,
                "[{name}] beat {beat}: (b-unproven) a caller that proved no period gets \
                 byte-identical pre-change behaviour — the subtraction is not merely smaller, \
                 it is WITHDRAWN. asks={asks_none} spent={spent_none} denied={denied_none}"
            );
            if r_none {
                unproven_population_witness = Some((name, asks_none, board.stack.len()));
            }
            assert!(
                skips_cov > 0,
                "[{name}] beat {beat}: the two arms must actually DIFFER on this board"
            );
        }

        let (name, asks_none, stack_len) = unproven_population_witness.expect(
            "REACH-GUARD: neither dump's UNPROVEN sweep ran to completion, so the population \
             claim below would be asserted against a budget-truncated count. The row FAILS \
             rather than silently weakening to `skips == 0`",
        );
        assert_eq!(
            asks_none, stack_len,
            "[{name}] (b-unproven) POPULATION: with no period proof the resolution gate scans \
             the UNCONDITIONED `current.stack`, element for element"
        );
    }

    /// R21 (b-placement-S) — THE FROZEN SKIP IS READ AT EXACTLY ONE SITE, AND THAT SITE IS
    /// BELOW ITEM (6)'s LOOP HEAD.
    ///
    /// CR 732.2a. §3 D2 step 3's replacement argument for the exemption is an ITEM-ORDERING
    /// argument: items (2)/(4)/(5) are what establish the premises the skip consumes, and each
    /// of them `return false`s strictly before it. Its sole precondition is that the skip lives
    /// in item (6) and nowhere earlier — a source-level fact, asserted here rather than argued.
    ///
    /// This arm covers item (5) as well, and better than a scan count would: item (5) is inside
    /// the extent, so a skip placed there raises the count to 2.
    ///
    /// COMMENT LINES ARE EXCLUDED, per R8's own ruling: a comment reads nothing, and counting
    /// one would make the tripwire fire on prose. (The extent carries exactly one such line —
    /// item (4)'s "`frozen_ids` is deliberately not read here".)
    ///
    /// REVERT-PROBE: add a `frozen_ids.contains(&e.id)` skip to item (4)'s closure ⇒ the read
    /// count goes 1 → 2 ⇒ FLIPS.
    #[test]
    fn r21_b_placement_s_the_frozen_skip_is_read_once_and_only_below_item_six() {
        let src = include_str!("resource.rs");
        let lines: Vec<&str> = src.lines().collect();

        // Symbol-anchored extent, the §6 R8 self-census discipline: column-0 signature line
        // to the first column-0 `}`.
        let extent = |signature: &str| -> (usize, usize) {
            let head = lines
                .iter()
                .position(|l| l.starts_with(signature))
                .unwrap_or_else(|| panic!("extractor found no column-0 `{signature}`"));
            let end = lines[head..]
                .iter()
                .position(|l| *l == "}")
                .map(|i| head + i)
                .unwrap_or_else(|| panic!("`{signature}` has no column-0 closing brace"));
            (head, end)
        };
        // Needles ASSEMBLED at runtime so this test's own source cannot be counted by its own
        // instrument (R13's hardening, applied here by construction).
        let frozen_token = format!("frozen{}ids", '_');
        let scan_token = format!("note{}conjunct4{}scan", '_', '_');

        let (head, end) = extent("pub(crate) fn loop_states_cover_modulo_growth_scoped");
        assert!(
            end - head > 100,
            "the extractor must return the whole predicate, not a truncated span; got \
             {head}-{end}"
        );
        // The shared comment rule (`crate::source_census::code`), so a trailing comment naming
        // one of the tokens below cannot be read as a code site.
        let code: Vec<(usize, &str)> = (head..=end)
            .map(|i| (i, crate::source_census::code(lines[i])))
            .collect();

        let item6_head = code
            .iter()
            .find(|(_, l)| l.contains("for entry in &current.stack"))
            .map(|(i, _)| *i)
            .expect("item (6)'s loop head must be inside the extent");
        let reads: Vec<usize> = code
            .iter()
            .filter(|(_, l)| l.contains(&frozen_token))
            .map(|(i, _)| *i)
            .collect();

        assert_eq!(
            reads.len(),
            1,
            "R21(b-placement-S): the frozen subtraction must be read at EXACTLY ONE site in \
             {head}-{end}; a second read is a premise consumed before it is proved. Found at \
             lines {:?} (1-based)",
            reads.iter().map(|i| i + 1).collect::<Vec<_>>()
        );
        assert!(
            reads[0] > item6_head,
            "R21(b-placement-S): the one read (line {}) must sit BELOW item (6)'s loop head \
             (line {}) — items (2)/(4)/(5) establish the premises it consumes and each \
             returns strictly above it",
            reads[0] + 1,
            item6_head + 1
        );

        // POSITIVE CONTROL AGAINST A DEAD GREP, same extractor and same filter: a token known
        // to be present in the SAME extent must be found, and one known to be absent must not.
        // One instrument, two values, one input.
        assert_eq!(
            code.iter().filter(|(_, l)| l.contains(&scan_token)).count(),
            1,
            "the instrument must be able to find a token that IS there — item (4)'s scan \
             notifier lives inside {head}-{end}"
        );
        assert_eq!(
            code.iter()
                .filter(|(_, l)| l.contains("ring_delta_signature"))
                .count(),
            0,
            "…and must not find one that is not"
        );
    }

    /// R21 (b-placement-B) — THE BEHAVIOURAL HALF, ON THE REAL DELLIAN WINDOW: ITEM (4) SCANS
    /// THE UNEXEMPTED STACK WHILE CONJUNCT (6) SKIPS IT.
    ///
    /// CR 732.2a. The matched pair is on ONE board and ONE touch, one predicate apart, so the
    /// difference is attributable to the placement and to nothing else.
    ///
    /// ⚠ THE PLAN'S STATED PAIR IS FALSIFIED BY MEASUREMENT AND IS RE-KEYED. It asks for
    /// *"the mutated board is REFUSED … the unmutated board still OFFERS (B-pos, row D1's
    /// beat)"*. Measured on the driven tree, the unmutated dellian beat-14 board does NOT
    /// offer — the mint returns `NoCertification` (`spent=26 scans=36 cert=None`), because
    /// item (4) already trips on a projected-resource reader at stack index 35 and basis B's
    /// `ring_delta_signature` finds no signature at `ring=2`. A pair whose two arms both
    /// refuse discriminates nothing. And no mutation is needed to make the point: the entry
    /// item (4) trips on IS ITSELF a frozen one, so the unmutated board already witnesses
    /// that item (4) does not consult the exemption.
    ///
    /// ⚠ THE WINDOW IS PRODUCTION'S, NOT `len - 2`. The old selection asserted `span == 1`
    /// silently, and the answer-beat sampler halved the newest adjacent pair on this dump:
    /// MEASURED, a span-1 window scans **0** entries at every beat 0..79, while production's
    /// first item-(4) candidate is **span 2** and scans 36. Both the search and the window now
    /// come from [`newest_item4_window`], i.e. from `game::engine::candidate_windows`.
    ///
    /// REVERT-PROBE: add a `frozen_ids` skip to item (4)'s closure ⇒ the scan can no longer
    /// reach index 35 and `conjunct4_scans` collapses to at most the non-exempt population
    /// (2 on this board) ⇒ FLIPS **while the search still succeeds and lands on the same
    /// beat** — which is what proves the search is a precondition and not the assertion.
    #[test]
    fn r21_b_placement_b_item_four_scans_frozen_entries_that_conjunct_six_skips() {
        let (beat, board) = drive_dump_until(TRACKED_DUMPS[1].1, 80, |s| {
            let live: Vec<&GameState> = s.loop_detect_ring.iter().map(|f| &f.live).collect();
            newest_item4_window(&live, s, s.active_player).is_some()
        })
        .expect(
            "REACH-GUARD: the dellian drive must reach a beat whose production candidate window \
             runs item (4) at all",
        );
        let live: Vec<&GameState> = board.loop_detect_ring.iter().map(|f| &f.live).collect();
        let proposer = board.active_player;
        let (idx, cover, span) = newest_item4_window(&live, &board, proposer)
            .expect("the search predicate accepted this very board one line ago");
        let window = &live[idx..];
        let prior = window[0];
        let non_exempt = board.stack.len() - cover.frozen_ids.len();
        assert!(
            cover.frozen_ids.len() > non_exempt,
            "REACH-GUARD beat {beat}: the frozen prefix must DOMINATE the stack, else \
             `scans > non_exempt` is satisfiable without ever touching a frozen entry; \
             frozen {} non-exempt {non_exempt}",
            cover.frozen_ids.len()
        );

        // ── ITEM (4): the scan population is the UNEXEMPTED current stack ────────────────
        let mut v4 = PeriodVerdicts::for_period(&live, &board, proposer);
        let covered =
            loop_states_cover_modulo_growth_pinned(prior, &board, proposer, &[], &cover, &mut v4);
        let scans = v4.conjunct4_scans() as usize;
        assert!(
            scans > non_exempt,
            "R21(b-placement-B) beat {beat} (window idx {idx}, span {span}): item (4) scanned \
             {scans} entries, which must EXCEED the {non_exempt} non-exempt ones — a scan that \
             consulted `frozen_ids` could never get past them"
        );
        assert!(
            scans > 0 && scans < board.stack.len(),
            "attribution: item (4)'s `.any()` must have SHORT-CIRCUITED inside the stack \
             ({scans} of {}), which is what makes it the refuser rather than a later item",
            board.stack.len()
        );
        assert!(
            cover.frozen_ids.contains(&board.stack[scans - 1].id),
            "R21(b-placement-B): the entry item (4) refused on (stack index {}) must itself \
             be PROVEN FROZEN — that is the whole content of `the skip lives in item (6) and \
             nowhere earlier`",
            scans - 1
        );
        assert!(
            !covered,
            "attribution: with a projected-resource reader inside the scanned population the \
             cover disjunct must fail; a `true` here would mean the scan found nothing and \
             the index assertion above was about the wrong entry"
        );
        assert_eq!(
            v4.conjunct6_frozen_skips(),
            0,
            "the cover predicate returned at item (4), so item (6) never ran and can have \
             taken no exemption — the two counters below come from the OTHER arm"
        );

        // ── MATCHED PAIR: the SAME touch, at the gate the skip actually lives in ─────────
        let mut v6 = PeriodVerdicts::for_period(&live, &board, proposer);
        let specified =
            stack_choices_are_all_specified(&board, proposer, &[], Some(&cover), &mut v6);
        // The guard states what it actually needs: a gate that ASKED must have COMPLETED, and no
        // answer was denied. MEASURED on production's own span-2 window the gate returns false
        // with `denied=false` and `conjunct6_asks=0` — a structural refusal at a pre-ask
        // conjunct, AFTER taking every one of the frozen skips. The exemption's COMPLETENESS is
        // checked by the verbatim equality below, not here. (A truncation of the skip count is
        // not a reachable failure mode: `note_conjunct6_frozen_skip`'s loop has no break and no
        // early return — the gate's `return false`s live in the NEXT loop, which runs only after
        // the counting loop has finished.)
        assert!(
            !v6.denied() && specified == (v6.conjunct6_asks() > 0),
            "REACH-GUARD: no answer may be denied, and a gate that ASKED must have completed. \
             specified={specified} denied={} asks={}",
            v6.denied(),
            v6.conjunct6_asks()
        );
        assert_eq!(
            v6.conjunct6_frozen_skips() as usize,
            cover.frozen_ids.len(),
            "R21(b-placement-B) matched pair: the SAME {} proven-frozen ids item (4) scanned \
             are the ones the resolution gate exempts",
            cover.frozen_ids.len()
        );
    }

    /// R17 — ID FRESHNESS ON THE DRIVEN DUMPS, AND `normalize_for_loop` PRESERVES EVERY
    /// `StackEntry.id`.
    ///
    /// CR 608.1 + CR 104.4b. The frozen-prefix exemption is the ONE place 5d makes the
    /// resolution gate strictly NARROWER than HEAD, and its soundness rests on a fixture that
    /// is structurally unconstructible: *a window whose prefix is identity-stable across every
    /// sampled frame while an exempted entry DOES announce or resolve in the driven period*.
    /// Both disjuncts die on one fact — an entry that RESOLVED inside the window has RETIRED
    /// its `StackEntry.id` (ids come from the monotone `next_object_id`), and an entry that
    /// ANNOUNCED inside the window was absent from the oldest window frame, which the
    /// exemption requires presence in. The second is definitional; the first is the invariant
    /// this row asserts, since a fixture cannot.
    ///
    /// ARM 3 is the cross-frame comparison's unstated dependency, stated: `certified_period_touch`
    /// compares `StackEntry.id` across frames, and `normalize_for_loop`'s products still feed
    /// `loop_states_equal` / `loop_states_cover_modulo_growth*` (CR 104.4b equality), so an id
    /// rewrite inside a function whose stated job is zeroing volatile monotone fields is
    /// exactly the plausible future regression.
    ///
    /// REVERT-PROBE (arm 3, EXECUTED IN-TEST as an instrument-liveness control): zero one id in
    /// a constructed normalized clone ⇒ the comparison must report a mismatch. REVERT-PROBE
    /// (arms 1/2): inject a synthetic re-push of a retired id into the observed sequence ⇒ the
    /// revival assertion FLIPS.
    ///
    /// ⚠ SCOPE: "both dumps" is the two TRACKED dumps. F4 is untracked until §5 U5.
    #[test]
    fn r17_a_retired_stack_entry_id_never_returns_and_normalization_preserves_it() {
        use std::collections::HashSet;

        for (name, gz) in TRACKED_DUMPS {
            let mut state = dump_state(gz);
            let mut retired: HashSet<ObjectId> = HashSet::new();
            let mut prev: HashSet<ObjectId> = HashSet::new();
            let mut announcements = 0usize;
            let mut resolutions = 0usize;
            let mut revivals: Vec<(usize, ObjectId)> = Vec::new();
            let mut normalization_checks = 0usize;
            let mut beats = 0usize;

            for beat in 0..40usize {
                beats = beat;
                let cur: HashSet<ObjectId> = state.stack.iter().map(|e| e.id).collect();
                for id in cur.difference(&prev) {
                    announcements += 1;
                    if retired.contains(id) {
                        revivals.push((beat, *id));
                    }
                }
                for id in prev.difference(&cur) {
                    resolutions += 1;
                    retired.insert(*id);
                }
                prev = cur;

                // ── ARM 3, on every beat carrying a stack ────────────────────────────────
                if !state.stack.is_empty() {
                    let ids =
                        |s: &GameState| -> Vec<ObjectId> { s.stack.iter().map(|e| e.id).collect() };
                    let before = ids(&state);
                    let normalized = state.normalize_for_loop();
                    assert_eq!(
                        ids(&normalized),
                        before,
                        "[{name}] beat {beat}: ARM 3 — `normalize_for_loop` zeroes \
                         `next_object_id` and clears trigger identity; rewriting a \
                         `StackEntry.id` would break both `certified_period_touch`'s \
                         cross-frame comparison and CR 104.4b equality"
                    );
                    // INSTRUMENT-LIVENESS CONTROL: the comparison must be able to SEE a
                    // rewrite. Without it, `ids(..) == before` proves nothing about the
                    // detector, only about this board.
                    let mut rewritten = normalized;
                    rewritten.stack[0].id = ObjectId(u64::MAX);
                    assert_ne!(
                        ids(&rewritten),
                        before,
                        "[{name}] beat {beat}: the arm-3 comparison must FLIP on a rewritten id"
                    );
                    normalization_checks += 1;
                }

                if announcements >= 3 && resolutions >= 3 && normalization_checks >= 3 {
                    break;
                }
                if dump_drive_one_beat(&mut state).is_err() {
                    break;
                }
            }

            // ── PAIRED POSITIVE REACH-GUARD: the population is non-empty in BOTH directions
            assert!(
                announcements >= 3 && resolutions >= 3,
                "[{name}] REACH-GUARD: the invariant must be checked over a population that \
                 really announces AND really resolves, else it passes over nothing. \
                 {announcements} announcements / {resolutions} resolutions in {beats} beats"
            );
            assert!(
                normalization_checks >= 3,
                "[{name}] REACH-GUARD: arm 3 must have run against a NON-EMPTY stack; \
                 {normalization_checks} checks"
            );
            assert!(
                revivals.is_empty(),
                "[{name}] ARMS 1/2 — CR 608.1: an id that left the stack must never reappear \
                 on it. `StackEntry.id` is drawn from the monotone `next_object_id`, whose \
                 only two plain (non-`+= 1`) production assignments write THROWAWAY CLONES \
                 (`effects/prepare.rs`'s simulated clone and `normalize_for_loop`'s comparand). \
                 Revived: {revivals:?}"
            );
        }
    }

    /// R27 (a2) — THE SEAM: EVERY BOARD `certified_period_touch` HANDS THE CLASSIFIER IS AN
    /// UN-NORMALIZED EVALUATION BOARD.
    ///
    /// CR 732.2a + CR 104.4b. Announcement and resolution are evaluated against the frame that
    /// CARRIES the pair; a `normalize_for_loop` product zeroes `next_object_id`, so a
    /// resolution evaluated against one allocates `ObjectId(0)` over a live object. The window
    /// carrier is therefore the sample's `live` half, never its `normalized` half.
    ///
    /// The matched pair is the two halves of the SAME ring, one argument apart — the
    /// `.normalized` arm is rounds 13–33's carrier, executed here as an instrument-liveness
    /// control, so the `!= 0` assertion cannot be true for want of a board that could fail it.
    ///
    /// ⚠ WHAT THIS ARM DOES NOT COVER: the mint's own carrier CHOICE (`ring_live` in
    /// `game::engine::bounded_cycle_offer`) is pinned structurally by
    /// `game::engine`'s `the_period_touch_window_is_carried_by_the_live_half`, because a test
    /// that builds its own window cannot flip on an edit to the mint's.
    #[test]
    fn r27_a2_every_announced_pair_carries_an_unnormalized_evaluation_board() {
        for (name, gz) in TRACKED_DUMPS {
            let (beat, board) = drive_dump_until(gz, 80, has_frozen_window).unwrap_or_else(|| {
                panic!("REACH-GUARD [{name}]: no driven beat carried a usable window")
            });
            assert_ne!(
                board.next_object_id, 0,
                "[{name}] beat {beat}: the LIVE board's allocator cursor must be non-zero, \
                 else the axis this row is keyed to cannot discriminate"
            );

            let live: Vec<&GameState> = board.loop_detect_ring.iter().map(|f| &f.live).collect();
            let norm: Vec<&GameState> = board
                .loop_detect_ring
                .iter()
                .map(|f| &f.normalized)
                .collect();
            let touch = certified_period_touch(
                &live[live.len() - 2..],
                &board,
                PeriodCertification::BoardCovered,
            );
            assert!(
                !touch.announced.is_empty(),
                "[{name}] beat {beat}: REACH-GUARD — the announced half must be non-empty, \
                 else the universal below quantifies over nothing"
            );
            assert!(
                touch
                    .announced
                    .iter()
                    .all(|(frame, _)| frame.next_object_id != 0),
                "[{name}] beat {beat}: (a2) every carrying frame must be an EVALUATION board. \
                 {} of {} announced pairs carry a zeroed allocator cursor",
                touch
                    .announced
                    .iter()
                    .filter(|(f, _)| f.next_object_id == 0)
                    .count(),
                touch.announced.len()
            );

            // ── INSTRUMENT-LIVENESS CONTROL: rounds 13–33's carrier, one argument apart ──
            let control = certified_period_touch(
                &norm[norm.len() - 2..],
                &board,
                PeriodCertification::BoardCovered,
            );
            assert_eq!(
                control.announced.len(),
                touch.announced.len(),
                "[{name}] the control must observe the SAME announcements — normalization \
                 preserves every `StackEntry.id` (R17 arm 3), so only the CARRIER differs"
            );
            assert!(
                control
                    .announced
                    .iter()
                    .filter(|(_, e)| board.stack.iter().all(|s| s.id != e.id))
                    .all(|(frame, _)| frame.next_object_id == 0),
                "[{name}] the control arm must really carry ZEROED boards for the pairs whose \
                 carrying frame is a RING frame; if it did not, the (a2) assertion above \
                 would be true of any board and would prove nothing"
            );
        }
    }

    /// R16 (ii-b) — THE MAX SPEND ACROSS MINTABLE BEATS SATURATES THE CAP, AND IT DOES SO
    /// AWAY FROM THE BEAT THE CORPUS OFFERS ON.
    ///
    /// CR 732.2a. (ii-a)'s companion, and the reason the two are SPLIT: exhaustion at a
    /// non-offering beat is a fail-closed refusal on a beat that was refusing anyway — the
    /// budget's stall-bounding job — while exhaustion at an OFFERING beat is a starved
    /// acceptance and a defect. This arm records the first half on a real driven board.
    ///
    /// MEASURED, not predicted: at dellian beat 14 (`ring=2 stack=154 frozen=152`) the mint
    /// spends the FULL cap and refuses `NoCertification`; the corpus's one offering beat
    /// (dina, integration row `r16_the_offering_beats_probe_demand_is_exactly_measured`)
    /// spends 13 and is NOT denied. Same seam, same cap, opposite sides of the budget.
    ///
    /// ⚠ THE SEARCH IS THE ROW'S OWN CONSTRUCTION REQUIREMENT, NOT `has_frozen_window`. "Mintable"
    /// means the walk reached the METERED CLASSIFIER, i.e. `meter.spent > 0`. The old predicate
    /// asserted `span == 1` on the newest pair and landed on the first ring-bearing beat, where
    /// the mint spends 0 and never reaches the classifier at all. `meter.denied` was REJECTED as
    /// a search predicate: `denied` can only latch after exhaustion, so `denied ⇒ spent == cap`
    /// and the assertion would assert itself.
    ///
    /// REVERT-PROBE: raise `PROBE_BUDGET` above dellian's unexempted demand (measured 96–107
    /// at these beats) ⇒ `denied` goes false ⇒ FLIPS. Lowering it cannot flip this arm, which
    /// is exactly why the offering-beat row is a separate one. **That revert-probe is SHIPPED
    /// IN-ROW as a positive control** ([`ProbeCap::RaisedTwiceLinks`], the same board, one
    /// argument apart): the meter provably returns `(x, false)` here, so `(cap, true)` is a
    /// verdict about the SHIPPED cap and not a property of the instrument. Self-checking — if
    /// the raised cap were still insufficient, `!raised.denied` fails loudly.
    #[test]
    fn r16_ii_b_a_non_offering_mintable_beat_saturates_the_probe_budget() {
        use crate::game::engine::{try_offer_bounded_cycle_shortcut_metered, ProbeCap};
        use crate::types::game_state::WaitingFor;

        /// The row's board construction, shared by the SEARCH and the measurement so the beat
        /// the search accepted is byte-identically the beat the assertions read.
        fn at_priority_of(s: &GameState) -> GameState {
            let mut at_priority = s.clone();
            at_priority.waiting_for = WaitingFor::Priority {
                player: s.active_player,
            };
            at_priority
        }

        let (beat, board) = drive_dump_until(TRACKED_DUMPS[1].1, 80, |s| {
            let (_, meter) = try_offer_bounded_cycle_shortcut_metered(
                &at_priority_of(s),
                false,
                ProbeCap::Shipped,
            );
            meter.spent > 0
        })
        .expect("REACH-GUARD: the dellian drive must reach a beat the metered classifier runs on");
        assert!(
            beat > 0,
            "ANTI-VACUITY: the search must have REJECTED at least one beat, else `spent > 0` is \
             a tautology satisfied by beat 0 rather than a filter"
        );
        let at_priority = at_priority_of(&board);
        assert!(
            at_priority.last_loop_action_sequence.is_empty()
                && at_priority.loop_detect_ring.len() >= 2,
            "REACH-GUARD beat {beat}: steps (1)/(1b)/(2)/(2b) must all pass, else the mint \
             refuses above the classifier and spends nothing for a reason unrelated to cost"
        );
        assert!(
            at_priority.stack.len() > PROBE_BUDGET as usize,
            "REACH-GUARD beat {beat}: the beat must carry more entries than the cap can pay \
             for, else saturation is not reachable at all; stack {} cap {PROBE_BUDGET}",
            at_priority.stack.len()
        );

        let (outcome, meter) =
            try_offer_bounded_cycle_shortcut_metered(&at_priority, false, ProbeCap::Shipped);
        assert!(
            outcome.is_err(),
            "R16(ii-b): this beat must NOT offer — the whole point of the split is that \
             saturation here is a refusal on a beat that was refusing anyway. Got {outcome:?}"
        );
        assert_eq!(
            (meter.spent, meter.denied),
            (PROBE_BUDGET, true),
            "R16(ii-b) beat {beat}: the max observable spend at the shipped cap IS the cap, \
             and the exhaustion is LATCHED so it can be attributed rather than inferred. \
             meter {meter:?}, stack {}",
            at_priority.stack.len()
        );

        // ── ANTI-VACUITY: `spent > 0` does NOT imply saturation, proven ON THIS BOARD ───────
        // The row's own revert-probe, shipped. One argument apart from the measurement above:
        // same board, same seam, a cap of twice the board's link count.
        let (_, raised) = try_offer_bounded_cycle_shortcut_metered(
            &at_priority,
            false,
            ProbeCap::RaisedTwiceLinks,
        );
        assert!(
            raised.spent > PROBE_BUDGET && !raised.denied,
            "POSITIVE CONTROL: at a cap of twice the link count the SAME board must spend past \
             the shipped cap WITHOUT latching exhaustion — otherwise `(cap, true)` above is a \
             property of the instrument rather than a verdict about the shipped cap. \
             raised {raised:?}, shipped cap {PROBE_BUDGET}, stack {}",
            at_priority.stack.len()
        );
    }

    // ───────────────────────────────────────────────────────────────────────────────────
    // §6 U3 ROWS — R14 / R19 / R29 / R31(completeness) / R32
    // ───────────────────────────────────────────────────────────────────────────────────

    /// Shape (A): a proposer-controlled triggered ability declaring exactly ONE mandatory
    /// PLAYER target (CR 115.2 "target opponent"), with the announcement ALREADY MADE.
    ///
    /// The announcement is load-bearing, not decoration: `optional_cleared_classification`
    /// resolves the ability on the board `resolve_top` would hand it, and an UNANNOUNCED
    /// target derives no events at all, which `probe_resolution` classifies `Prompted`
    /// (§6 R11). A shape-(A) fixture without announced targets therefore has residual
    /// `MayPrompt` for every slot vector and R19's transition could never fire.
    fn u3_shape_a_entry(src: ObjectId, id: u64) -> StackEntry {
        use crate::types::ability::{
            ControllerRef, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
            TypedFilter,
        };
        let mut ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![],
                    controller: Some(ControllerRef::Opponent),
                    properties: vec![],
                })),
            },
            vec![TargetRef::Player(PlayerId(1))],
            src,
            PlayerId(0),
        );
        ability.optional = true;
        StackEntry {
            id: ObjectId(id),
            source_id: src,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: src,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        }
    }

    /// §6 R7 — THE FROZEN PREFIX IS AN `(index, id)` IDENTITY, NOT A PRESENCE COUNT.
    ///
    /// CR 732.2a: `certified_period_touch` may exempt a `current.stack` entry from conjunct (6)
    /// only when that entry sits at the SAME INDEX carrying the SAME `ObjectId` in every window
    /// frame — i.e. it demonstrably did not participate in the observed period. A weaker test
    /// ("something is at that index") would exempt an entry the period shifted underneath, and
    /// a shifted entry is exactly one that DID move.
    ///
    /// This is the constructed unit R33/R21 do not cover: those rows measure the exemption on
    /// the real dellian window, where every frame's prefix happens to be identity-stable, so
    /// neither of them can separate the identity conjunct from a presence check.
    ///
    /// Arms, on ONE constructed 3-frame window (`[f0, f1, f2]` + `current`):
    /// * **(a)** a stable `(index, id)` prefix of length 2 ⇒ `frozen_ids` contains exactly it;
    /// * **(a′)** the reach-guard that makes (a) non-trivial: the stack's TAIL entry differs
    ///   across the frames, so `frozen_ids` is a PROPER subset and "freeze everything" fails;
    /// * **(b)** one frame's stack shifted by a single index (an extra entry pushed at the
    ///   front) ⇒ the prefix ids are no longer at their own indices ⇒ NOTHING is frozen, even
    ///   though every id is still PRESENT in that frame.
    ///
    /// REVERT-PROBE (RUN, see the journal): weaken the identity conjunct to a presence check
    /// (`frame.stack.get(*index).is_some()`) ⇒ (b)'s frozen set becomes the whole prefix again
    /// ⇒ (b) FAILS. (a)/(a′) stay green under that probe, which is what makes (b) the arm the
    /// identity conjunct is answerable to.
    #[test]
    fn r7_the_frozen_prefix_is_an_index_id_identity_never_a_presence_count() {
        let entry = |id: u64| churn_entry(id, 0, lose_ability(1), None);
        // ids 7001/7002 are the STABLE prefix; the tail differs per frame so the frozen set is
        // a proper subset and this row cannot be satisfied by "freeze the whole stack".
        let frame_with_tail = |tail: u64| {
            let mut s = GameState::new_two_player(20);
            s.stack.push_back(entry(7001));
            s.stack.push_back(entry(7002));
            s.stack.push_back(entry(tail));
            s
        };
        let (f0, f1, f2) = (
            frame_with_tail(7010),
            frame_with_tail(7011),
            frame_with_tail(7012),
        );
        let current = frame_with_tail(7013);

        // ── (a) + (a′) ──
        let touch = certified_period_touch(
            &[&f0, &f1, &f2],
            &current,
            PeriodCertification::BoardCovered,
        );
        let frozen: Vec<ObjectId> = touch.frozen_ids.iter().copied().collect();
        assert_eq!(
            frozen,
            vec![ObjectId(7001), ObjectId(7002)],
            "(a) CR 732.2a: exactly the entries holding the SAME id at the SAME index in every \
             window frame are provably outside the observed period"
        );
        assert!(
            frozen.len() < current.stack.len(),
            "(a′) reach-guard: the frozen set must be a PROPER subset — a fixture whose whole \
             stack froze could not tell the identity conjunct from `freeze everything`; \
             stack {} vs frozen {}",
            current.stack.len(),
            frozen.len()
        );

        // ── (b) one frame shifted by a single index; every id is still PRESENT in it ──
        let shifted = {
            let mut s = f1.clone();
            s.stack.push_front(entry(7099));
            s
        };
        let shifted_ids: std::collections::BTreeSet<ObjectId> =
            shifted.stack.iter().map(|e| e.id).collect();
        assert!(
            shifted_ids.contains(&ObjectId(7001)) && shifted_ids.contains(&ObjectId(7002)),
            "(b) reach-guard: the shift must PRESERVE both prefix ids in that frame, otherwise \
             a presence check would reject them too and the arms would not separate"
        );
        let shifted_touch = certified_period_touch(
            &[&f0, &shifted, &f2],
            &current,
            PeriodCertification::BoardCovered,
        );
        assert!(
            shifted_touch.frozen_ids.is_empty(),
            "(b) CR 732.2a: a single index shift means the entry moved WITHIN the observed \
             period, so no exemption is provable — presence at some index is not the property. \
             got {:?}",
            shifted_touch.frozen_ids
        );
    }

    /// R14 — THE DEGENERATE WINDOW ANNOUNCES `current.stack`, ELEMENT FOR ELEMENT.
    ///
    /// CR 732.2a + CR 608.1. With NO window frame there is no transition to observe, so the
    /// honest degenerate reading is *"every current entry may announce"* — which is exactly
    /// what makes `bounded_cycle_pin_slots` a thin alias of the window enumerator rather
    /// than a second authority.
    ///
    /// ⚠ THE COMPARISON IS AGAINST THE TEST'S OWN INPUT DATA, NEVER AGAINST A SECOND CALL OF
    /// THE FUNCTION UNDER TEST. Rounds 5/6 wrote this row as
    /// `bounded_cycle_pin_slots_for_window(&certified_period_touch(&[], state, ..), p)` vs
    /// `bounded_cycle_pin_slots(state, p)` — post-U3 the same function body on both sides, so
    /// the equality held by construction AND the stated revert-probe edited a dependency BOTH
    /// sides call. Round 7's replacement (a HEAD-captured frozen point sequence) was struck in
    /// turn: U2's shape-(B) mint publishes on beats HEAD refuses, and a per-beat sequence
    /// embeds `ObjectId` literals that a fixture re-dump renumbers. The PROPERTY against a
    /// CONSTRUCTED frame has neither failure mode.
    ///
    /// REVERT-PROBE that actually flips: restore round 4's `window.len() < 2 ⇒ announced`
    /// EMPTY branch ⇒ `announced.len() == 0` while the constructed stack is non-empty ⇒ the
    /// element-for-element equality fails on the LENGTH assertion alone, and the ≥1-point
    /// reach-guard fails with it.
    #[test]
    fn r14_the_degenerate_window_announces_the_current_stack_element_for_element() {
        use crate::game::engine::{bounded_cycle_pin_slots_for_window, entry_publishes_pin_slots};

        let (mut state, src) = u2_relief_board();
        // ≥2 entries, deliberately HETEROGENEOUS: the first publishes for P0, the second is
        // controlled by P1 and the mint refuses it. `announced` must carry BOTH — the touch
        // enumerates the period, the mint filters it — so an `announced` accidentally built
        // from the mint's accepted set would fail the element-for-element equality.
        let publishing = u2_shape_b_entry(src, 9140, u2_draw_effect(), |_| {});
        let mut foreign = u2_shape_b_entry(src, 9141, u2_draw_effect(), |_| {});
        foreign.controller = PlayerId(1);
        state.stack.push_back(publishing);
        state.stack.push_back(foreign);
        assert!(
            state.stack.len() >= 2,
            "NON-DEGENERACY: a one-entry stack would make the ordering half of the claim \
             untestable and an empty one would make the whole property vacuous"
        );

        let expected: Vec<(usize, ObjectId)> = state
            .stack
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.id))
            .collect();

        for cert in [
            PeriodCertification::BoardCovered,
            PeriodCertification::BoardEqualOnly,
            PeriodCertification::ResourceSignatureOnly,
        ] {
            let touch = certified_period_touch(&[], &state, cert);
            let observed: Vec<(usize, ObjectId)> = touch
                .announced
                .iter()
                .enumerate()
                .map(|(i, (_, e))| (i, e.id))
                .collect();
            assert_eq!(
                observed, expected,
                "{cert:?}: the degenerate window's announced pairs are `current.stack` \
                 element for element, IN ORDER"
            );
            assert!(
                touch
                    .announced
                    .iter()
                    .all(|(frame, _)| std::ptr::eq(*frame, &state)),
                "{cert:?}: every degenerate pair's carrying frame IS `current` — there is no \
                 other frame for it to be"
            );
            assert!(
                touch.frozen_ids.is_empty(),
                "{cert:?}: with no window frame nothing is PROVEN frozen, on ANY certificate \
                 value — the empty-window branch returns before the frozen walk"
            );
        }

        // PAIRED POSITIVE REACH-GUARD, and it is what stops the property collapsing into a
        // vacuous truth about a stack nothing publishes on.
        let touch = certified_period_touch(&[], &state, PeriodCertification::ResourceSignatureOnly);
        let points = bounded_cycle_pin_slots_for_window(&touch, PlayerId(0));
        assert!(
            !points.is_empty(),
            "reach-guard: at least one constructed entry must be ACCEPTED by the mint, or the \
             equality above would hold over a period the enumerator never publishes from"
        );
        assert!(
            entry_publishes_pin_slots(&state, &state.stack[1], PlayerId(0)).is_none(),
            "reach-guard, other direction: the P1-controlled entry is REFUSED by the mint yet \
             still appears in `announced` — proof the touch is the period's enumeration and \
             not the mint's accepted set"
        );
    }

    /// R19 — THE CACHED VERDICT HOLDS NOTHING SLOTS-DERIVED.
    ///
    /// The named regression row for the branch NOT taken (key the memo on
    /// `(StackEntry.id, slots)`). If that option's risk returns one layer over, this is what
    /// loses.
    ///
    /// **(a) BEHAVIOURAL.** `state` and `entry` are held FIXED and only `slots` varies, over
    /// FOUR cases — `{}`, `{target}`, `{may}`, `{target, may}`. The vector is SHAPE-DEPENDENT
    /// and both shapes are asserted, because D3 re-expressed the gate as *"`may` pinned AND
    /// (`target.is_none()` OR target pinned)"*:
    /// * **(a-A) shape (A)** — targeted AND optional ⇒ `[None, None, None, Some(residual)]`.
    /// * **(a-B) shape (B)** — may-only ⇒ `[None, None, Some(residual), Some(residual)]`.
    ///
    /// The `{may}` case is the one that DIFFERS between the shapes, and asserting BOTH is what
    /// makes D3's `target.is_none()` disjunct load-bearing: with (a-A) alone, deleting that
    /// disjunct changes nothing and the row cannot see it. The two SINGLETON cases are what
    /// make both disjuncts load-bearing, which a 0/1/2-slot sweep could not do.
    ///
    /// **(b) STRUCTURAL.** `EntryVerdict` destructures EXHAUSTIVELY into
    /// `{ published, primary, residual }`, all three produced by functions whose signatures
    /// take NO slots, so a future slots-derived field is a COMPILE ERROR rather than a review
    /// miss. ⚠ `..` IS FORBIDDEN ON THAT DESTRUCTURE. Adding e.g. `pub slots_digest: u64` to
    /// `EntryVerdict` makes the line below **E0027**; rustc's own `help:` then suggests adding
    /// `..`, which silently disarms the guard. THE E0027 IS THIS GUARD FIRING, NOT A COMPILE
    /// ERROR TO FIX.
    ///
    /// REVERT-PROBE (a): collapse the design to an id-keyed cache of the RELIEF verdict —
    /// i.e. adopt option 1's shape with an incomplete key — ⇒ every case returns the `{}`
    /// verdict `None` ⇒ the fourth arm's `Some(residual)` assertion FLIPS TO FAIL. The paired
    /// positive reach-guard is mandatory and is asserted: the last case must return `Some` on
    /// the unmutated design, otherwise the leading `None`s pass over a relief that never
    /// relieves anything.
    #[test]
    fn r19_the_relief_vector_varies_only_with_slots_and_the_cached_value_is_slots_free() {
        use crate::game::engine::entry_publishes_pin_slots;
        use crate::game::resolution_prompt::ResolutionChoiceFreedom;

        let (mut state, src) = u2_relief_board();
        // Two live opponents, so the shape-(A) announcement is a REAL choice rather than a
        // forced one — the mint's own acceptance is what this row rides, not target scarcity.
        let shape_a = u3_shape_a_entry(src, 9190);
        let shape_b = u2_shape_b_entry(src, 9191, u2_draw_effect(), |_| {});
        state.stack.push_back(shape_a.clone());
        state.stack.push_back(shape_b.clone());

        let pub_a = entry_publishes_pin_slots(&state, &shape_a, PlayerId(0))
            .expect("(a-A) reach-guard: the shape-(A) fixture must reach the mint");
        let target = pub_a
            .target
            .clone()
            .expect("(a-A) reach-guard: shape (A) publishes its CR 601.2c announcement slot");
        let may_a = pub_a
            .may
            .clone()
            .expect("(a-A) reach-guard: shape (A) is optional, so it publishes its CR 603.5 gate");
        let pub_b = entry_publishes_pin_slots(&state, &shape_b, PlayerId(0))
            .expect("(a-B) reach-guard: the shape-(B) fixture must reach the mint");
        assert!(
            pub_b.target.is_none(),
            "(a-B) reach-guard: shape (B) announces NOTHING, so it publishes no target slot — \
             that `None` is exactly what the `{{may}}` case discriminates"
        );
        let may_b = pub_b
            .may
            .clone()
            .expect("(a-B) reach-guard: shape (B) publishes its CR 603.5 gate");

        let mut verdicts = PeriodVerdicts::for_period(&[], &state, PlayerId(0));
        let f = verdicts
            .frame_ix(&state)
            .expect("reach-guard: the container holds the board the relief is asked about");

        let relieved = |verdicts: &mut PeriodVerdicts<'_>,
                        entry: &StackEntry,
                        slots: &[DecisionSlot]|
         -> bool {
            matches!(
                pinned_may_choice_relief(f, entry, verdicts, u2_scope(slots)),
                Some(ResolutionChoiceFreedom::FreeUnlessReplacements(_))
            )
        };

        // ── (a-A) shape (A): the gate needs BOTH slots ──────────────────────────────────
        let vector_a = [
            relieved(&mut verdicts, &shape_a, &[]),
            relieved(&mut verdicts, &shape_a, std::slice::from_ref(&target)),
            relieved(&mut verdicts, &shape_a, std::slice::from_ref(&may_a)),
            relieved(&mut verdicts, &shape_a, &[target.clone(), may_a.clone()]),
        ];
        assert_eq!(
            vector_a,
            [false, false, false, true],
            "(a-A) CR 603.5 + CR 601.2c: a targeted optional entry is relieved ONLY when both \
             published slots are pinned. The two SINGLETON cases are what make both disjuncts \
             of `may pinned AND (target.is_none() OR target pinned)` load-bearing"
        );

        // ── (a-B) shape (B): `target.is_none()` discharges the second conjunct outright ──
        let vector_b = [
            relieved(&mut verdicts, &shape_b, &[]),
            relieved(&mut verdicts, &shape_b, std::slice::from_ref(&target)),
            relieved(&mut verdicts, &shape_b, std::slice::from_ref(&may_b)),
            relieved(&mut verdicts, &shape_b, &[target.clone(), may_b.clone()]),
        ];
        assert_eq!(
            vector_b,
            [false, false, true, true],
            "(a-B) CR 601.2c: a may-only entry surfaces no announcement choice, so demanding a \
             pinned target would refuse relief the mint's own schema fully describes. The \
             `{{may}}` case is where the two shapes DIFFER — that difference is the whole \
             reason D3's `target.is_none()` disjunct exists"
        );

        // ── (b) STRUCTURAL: the cached value is slots-free BY DESTRUCTURE ───────────────
        // ⚠ NO `..`. A fourth field here is E0027 and the E0027 IS THIS GUARD FIRING.
        let super::verdict_memo::EntryVerdict {
            published,
            primary,
            residual,
        } = verdicts.verdict(f, &shape_a);
        assert!(
            published.is_some() && matches!(primary, ResolutionChoiceFreedom::MayPrompt),
            "(b) reach-guard: the destructured value must be the LIVE one this row's (a) arm \
             consumed — a mint answer plus the CR 603.5 `MayPrompt` that makes relief the \
             deciding layer"
        );
        assert!(
            residual.is_some(),
            "(b) reach-guard: the optional-cleared residual is computed BECAUSE a `may` slot \
             is published — the field is not decorative"
        );
    }

    /// R29 — THE CR 603.3c MID-CONSTRUCTION GUARD SURVIVES THE `FrameIx` REWRITE, AND IT
    /// ANSWERS FROM THE CARRYING FRAME.
    ///
    /// Asserted AT THE RELIEF SEAM deliberately: an offer-level negative on this axis is
    /// DOMINATED by conjunct (6)'s own refusals, and the relief has no upstream that can
    /// satisfy it.
    ///
    /// * **(a) THE GUARD** — an entry the mint publishes a `target` slot for, that slot
    ///   PINNED (so the mint half alone answers `true`), with the frame's
    ///   `pending_trigger_entry = Some(entry.id)` ⇒ `false`.
    /// * **(a′) MATCHED POSITIVE, byte-identical except the cursor** — `None` ⇒ `true`. This
    ///   is the reach-guard: without it, (a) passes over a mint that published nothing.
    /// * **(b) THE EQUALITY, NOT THE PRESENCE** — a cursor naming NO live entry
    ///   (`ObjectId(u64::MAX)`) ⇒ still `true`, so the row pins `== Some(entry.id)` rather
    ///   than "any cursor refuses". In-tree precedent for exactly this control on the sibling
    ///   gate: `a_pinned_slot_skips_gate_three_and_six`'s gate-(1) control.
    /// * **(c) WHICH BOARD** — the conjunct the 4-arg signature could not even express. The
    ///   pair is driven from a RETAINED frame, then the two boards are CROSSED: (c1) carrying
    ///   frame's cursor set, `current`'s clear ⇒ `false` (the carrying frame decides); (c2)
    ///   carrying frame's cursor clear, `current`'s set ⇒ `true` (the live board does NOT
    ///   decide for a retained pair). Byte-identical except which board holds the cursor.
    ///
    /// REVERT-PROBE (a)/(a′)/(b): delete
    /// `if board.pending_trigger_entry == Some(entry.id) { return false; }` from
    /// `entry_target_choice_is_pinned` — i.e. ship the round-35 4-arg signature, which makes
    /// the statement unwritable — ⇒ (a) answers `true` ⇒ FLIPS, while (a′)/(b) stay green.
    /// ⚠ That revert COMPILES, which is the whole reason this row exists: the fail-open
    /// direction leaves no type error behind and `PeriodVerdicts.frames` is private, so an
    /// executor who drops the board has no way to notice.
    ///
    /// REVERT-PROBE (c): pass the live `current` instead of the pair's carrying frame ⇒ (c1)
    /// and (c2) BOTH FLIP, while (a)/(a′)/(b) stay green on the degenerate current-stack pair
    /// where the two boards coincide.
    #[test]
    fn r29_the_cr_603_3c_cursor_is_read_from_the_pairs_carrying_frame() {
        use crate::game::engine::entry_publishes_pin_slots;

        let (mut frame, src) = u2_relief_board();
        let entry = u3_shape_a_entry(src, 9290);
        frame.stack.push_back(entry.clone());
        let published = entry_publishes_pin_slots(&frame, &entry, PlayerId(0))
            .expect("reach-guard: the fixture must reach the mint");
        let target = published.target.clone().expect(
            "reach-guard: the mint must publish a TARGET slot, or the guard below \
                     would be refused by the `target.is_some()` conjunct instead",
        );
        let slots = vec![target];

        // ── (a′) matched positive: no cursor ────────────────────────────────────────────
        let mut clear = frame.clone();
        clear.pending_trigger_entry = None;
        {
            let mut verdicts = PeriodVerdicts::for_period(&[], &clear, PlayerId(0));
            let f = verdicts
                .frame_ix(&clear)
                .expect("container holds the board");
            assert!(
                entry_target_choice_is_pinned(&clear, f, &entry, &mut verdicts, u2_scope(&slots)),
                "(a′) with the published slot pinned and no mid-construction cursor, the \
                 announcement choice IS specified"
            );
        }

        // ── (a) the guard: the cursor names THIS entry ──────────────────────────────────
        let mut cursored = frame.clone();
        cursored.pending_trigger_entry = Some(entry.id);
        {
            let mut verdicts = PeriodVerdicts::for_period(&[], &cursored, PlayerId(0));
            let f = verdicts
                .frame_ix(&cursored)
                .expect("container holds the board");
            assert!(
                !entry_target_choice_is_pinned(
                    &cursored,
                    f,
                    &entry,
                    &mut verdicts,
                    u2_scope(&slots)
                ),
                "(a) CR 603.3c: a mid-construction entry's announcement is not yet complete, \
                 so no published slot can specify it — and `pending_trigger_entry` is set \
                 exactly while a prompt is up, which is why the mint (a function of the \
                 BOARD, never of the PROMPT) cannot carry it and the relief must"
            );
        }

        // ── (b) EQUALITY, not presence ─────────────────────────────────────────────────
        let mut foreign_cursor = frame.clone();
        foreign_cursor.pending_trigger_entry = Some(ObjectId(u64::MAX));
        {
            let mut verdicts = PeriodVerdicts::for_period(&[], &foreign_cursor, PlayerId(0));
            let f = verdicts
                .frame_ix(&foreign_cursor)
                .expect("container holds the board");
            assert!(
                entry_target_choice_is_pinned(
                    &foreign_cursor,
                    f,
                    &entry,
                    &mut verdicts,
                    u2_scope(&slots)
                ),
                "(b) the guard is `== Some(entry.id)`, not `is_some()` — a cursor naming no \
                 live entry refuses nothing"
            );
        }

        // ── (c) WHICH BOARD, on a RETAINED pair ────────────────────────────────────────
        // The container's frames are `[carrying, current]`, and `carrying` is NOT `current`,
        // so the two boards can disagree — which is exactly the shape the degenerate
        // current-stack pair cannot express.
        let mut carrying = frame.clone();
        let mut current = frame.clone();
        current.stack.clear();

        carrying.pending_trigger_entry = Some(entry.id);
        current.pending_trigger_entry = None;
        {
            let ring = [&carrying];
            let mut verdicts = PeriodVerdicts::for_period(&ring, &current, PlayerId(0));
            let f = verdicts
                .frame_ix(&carrying)
                .expect("(c) reach-guard: the CARRYING frame is in the period");
            assert_ne!(
                verdicts.frame_ix(&carrying),
                verdicts.frame_ix(&current),
                "(c) reach-guard: the two boards must be DISTINCT frames, or (c1)/(c2) are \
                 the same assertion twice"
            );
            assert!(
                !entry_target_choice_is_pinned(
                    &carrying,
                    f,
                    &entry,
                    &mut verdicts,
                    u2_scope(&slots)
                ),
                "(c1) the CARRYING frame decides: its cursor names this entry, so the \
                 announcement is mid-construction on the board the announcement was made \
                 against — regardless of what the live board says"
            );
        }

        carrying.pending_trigger_entry = None;
        current.pending_trigger_entry = Some(entry.id);
        {
            let ring = [&carrying];
            let mut verdicts = PeriodVerdicts::for_period(&ring, &current, PlayerId(0));
            let f = verdicts
                .frame_ix(&carrying)
                .expect("(c) reach-guard: the CARRYING frame is in the period");
            assert!(
                entry_target_choice_is_pinned(
                    &carrying,
                    f,
                    &entry,
                    &mut verdicts,
                    u2_scope(&slots)
                ),
                "(c2) the LIVE board does not decide for a retained pair — byte-identical to \
                 (c1) except which board holds the cursor"
            );
        }

        // ── (c3) THE SAME CROSSING, AT THE PRODUCTION SEAM THAT WRITES THE ARGUMENT ─────
        // (c1)/(c2) pin the PREDICATE's board-sensitivity, which is where the plan sites this
        // row (an offer-level negative on this axis is dominated by conjunct (6)'s own
        // refusals). But the argument the row is about is written in
        // `stack_choices_are_all_specified`'s announcement loop, so the plan's stated
        // revert-probe — "pass the live `current` instead of the pair's carrying frame" —
        // needs a site that FLIPS. This arm is that site: the (c2) crossing driven end to
        // end, where the correct argument CERTIFIES and the reverted one REFUSES.
        //
        // Both published slots are pinned here (the `may` too), so the entry also clears
        // conjunct (6) and the `true` below is the whole predicate's answer, not one gate's.
        {
            let may = published
                .may
                .clone()
                .expect("(c3) reach-guard: the fixture is optional, so it publishes a may gate");
            let both = vec![slots[0].clone(), may];
            let mut oldest = carrying.clone();
            oldest.stack.clear();
            let ring = [&oldest, &carrying];
            let touch =
                certified_period_touch(&ring, &current, PeriodCertification::ResourceSignatureOnly);
            assert!(
                touch
                    .announced
                    .iter()
                    .any(|(frame, e)| std::ptr::eq(*frame, &carrying) && e.id == entry.id),
                "(c3) reach-guard: the pair must arrive from the RETAINED frame — on a \
                 current-stack pair the two boards coincide and the argument is untestable"
            );
            assert!(
                current.pending_trigger_entry == Some(entry.id)
                    && carrying.pending_trigger_entry.is_none(),
                "(c3) reach-guard: the crossing must still be in place — `current` holds the \
                 CR 603.3c cursor and the carrying frame does not"
            );
            let mut verdicts = PeriodVerdicts::for_period(&ring, &current, PlayerId(0));
            assert!(
                stack_choices_are_all_specified(
                    &current,
                    PlayerId(0),
                    &both,
                    Some(&touch),
                    &mut verdicts
                ),
                "(c3) with both published slots pinned and the mid-construction cursor sitting \
                 only on the LIVE board, the announcement loop must certify — because it asks \
                 the CARRYING frame. Passing `current` there makes the cursor bite, the `||`'s \
                 other disjunct is also false (two legal assignments on the frame), and this \
                 assertion FLIPS"
            );
        }
    }

    /// R32 — THE ANNOUNCEMENT LOOP'S OR-DISJUNCT READS THE PAIR'S CARRYING FRAME, NEVER
    /// `current`. Paired with R29, which pins the same discipline for the other disjunct:
    /// the two halves of one `||` must not read two boards.
    ///
    /// `stack_entry_has_no_ordering_input`'s arity does NOT change under 5d, which is why
    /// only a row can pin the decision — §7 held it under *"Reused verbatim"*. It is
    /// board-sensitive: `state.pending_trigger_entry`, then `forced_unique_targeting` →
    /// `build_target_slots` + `auto_select_targets_for_ability`, i.e. the verdict is a
    /// function of the board's legal-target POPULATION.
    ///
    /// THE PAIR. A two-frame window whose announced entry declares a target with **two**
    /// legal assignments on its CARRYING FRAME and **one** on `current` (a creature that left
    /// the battlefield after the announcement).
    /// * **(a) CORRECT BOARD** ⇒ `false` (two assignments ⇒ not forced), and with no pin
    ///   published for that entry, `stack_choices_are_all_specified` ⇒ `false` — the
    ///   fail-closed answer.
    /// * **(b) THE FAIL-OPEN TWIN** ⇒ passing `current` sees ONE legal assignment ⇒ `true` ⇒
    ///   the entry is RELIEVED and the offer can certify over an announcement choice that is
    ///   not forced on the board where it is actually made.
    /// * **(c) POSITIVE CONTROL that the instrument is keyed** — same window, an entry with
    ///   one legal assignment on BOTH boards ⇒ both arms agree on `true`, so (a)/(b) differ
    ///   BECAUSE OF THE BOARD and not because the fixture always disagrees.
    ///
    /// REVERT-PROBE: change `stack_entry_has_no_ordering_input(announced[i].0, entry)` to
    /// `(state, entry)` in `stack_choices_are_all_specified`'s announcement loop ⇒ (b) is
    /// what you get and (a)'s offer-level assertion FLIPS.
    #[test]
    fn r32_the_announcement_disjunct_reads_the_carrying_frame_not_the_live_board() {
        use crate::game::scenario::GameScenario;
        use crate::types::ability::{
            Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TypeFilter, TypedFilter,
        };

        // One creature the announcement can still reach on `current`, one that leaves.
        let mut carrying = GameScenario::new_n_player(2, 7).build().state().clone();
        let stays = battlefield_creature(&mut carrying, 9320, 1);
        let leaves = battlefield_creature(&mut carrying, 9321, 1);

        let creature_entry = |id: u64, announced: Vec<TargetRef>| {
            let ability = ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: None,
                        properties: vec![],
                    }),
                    damage_source: None,
                    excess: None,
                },
                announced,
                ObjectId(9320),
                PlayerId(0),
            );
            StackEntry {
                id: ObjectId(id),
                source_id: ObjectId(9320),
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: ObjectId(9320),
                    ability: Box::new(ability),
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: String::new(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            }
        };

        let two_ways = creature_entry(9330, vec![TargetRef::Object(leaves)]);
        carrying.stack.push_back(two_ways.clone());

        // The window's oldest frame carries NO stack, so the entry ANNOUNCED inside it.
        let mut oldest = carrying.clone();
        oldest.stack.clear();
        // `current`: the entry has resolved off the stack AND one legal target is gone.
        let mut current = carrying.clone();
        current.stack.clear();
        current.objects.remove(&leaves);
        current.battlefield.retain(|o| *o != leaves);

        // ── reach-guards: the two boards really carry DIFFERENT legal populations ───────
        assert_eq!(
            carrying
                .battlefield
                .iter()
                .filter(|o| carrying.objects.contains_key(o))
                .count(),
            2,
            "reach-guard: the carrying frame must offer TWO legal assignments"
        );
        assert!(
            current.objects.contains_key(&stays) && !current.objects.contains_key(&leaves),
            "reach-guard: `current` must offer exactly ONE — the fail-open direction needs a \
             target that is legal on the frame and gone from the live board"
        );

        // ── (a) / (b) THE MATCHED PAIR, one variable: which board ──────────────────────
        assert!(
            !stack_entry_has_no_ordering_input(&carrying, &two_ways),
            "(a) CORRECT BOARD: two legal assignments on the carrying frame ⇒ the \
             announcement choice is NOT forced ⇒ fail-closed"
        );
        assert!(
            stack_entry_has_no_ordering_input(&current, &two_ways),
            "(b) THE FAIL-OPEN TWIN: on the live board one target has gone, so the assignment \
             LOOKS forced and the entry would be relieved — an offer certified over a choice \
             that is not forced where it is actually made"
        );

        // ── (a) at the OFFER-LEVEL seam, which is where the argument is actually written ─
        let ring = [&oldest, &carrying];
        let touch =
            certified_period_touch(&ring, &current, PeriodCertification::ResourceSignatureOnly);
        assert!(
            touch
                .announced
                .iter()
                .any(|(frame, e)| std::ptr::eq(*frame, &carrying) && e.id == two_ways.id),
            "reach-guard: the pair must arrive from the RETAINED frame, not from \
             `current.stack` — on the degenerate pair the two boards coincide and the row \
             would be untestable"
        );
        let mut verdicts = PeriodVerdicts::for_period(&ring, &current, PlayerId(0));
        assert!(
            !stack_choices_are_all_specified(
                &current,
                PlayerId(0),
                &[],
                Some(&touch),
                &mut verdicts
            ),
            "(a) offer level: with no pin published for the entry, the announcement loop's \
             `||` must refuse — and it can only refuse if its second disjunct read the \
             CARRYING frame. Passing `current` there yields (b) and this assertion flips"
        );

        // ── (c) POSITIVE CONTROL: one legal assignment on BOTH boards ──────────────────
        let mut solo_carrying = carrying.clone();
        solo_carrying.objects.remove(&leaves);
        solo_carrying.battlefield.retain(|o| *o != leaves);
        let solo = creature_entry(9331, vec![TargetRef::Object(stays)]);
        assert!(
            stack_entry_has_no_ordering_input(&solo_carrying, &solo)
                && stack_entry_has_no_ordering_input(&current, &solo),
            "(c) the instrument is KEYED, not always-disagreeing: an entry with one legal \
             assignment on both boards is answered identically on both"
        );
    }

    /// R31 COMPLETENESS ARM (the U3 half of a row whose arms (a)/(a′)/(b) shipped in U2).
    ///
    /// U2's arms proved the closure on entries sitting on `current.stack`. This arm proves
    /// the premise those arms silently rest on — *every minted pair is SCANNED* — for the
    /// population U2 could not reach: a pair that ANNOUNCED inside the certified period and
    /// has since left the stack. It ships here and not in U2 because its revert-probe names
    /// `touch.announced`, which U3 creates: at U2 `stack_choices_are_all_specified` still
    /// carried HEAD's 3-argument shape, so there was no `touch` to drop.
    ///
    /// Off-stack announced pairs are the MAJORITY population on both measured dumps
    /// (`beats_offstack_nonzero` 157/161 F4, 19/23 dellian), so this is the common case, not
    /// an edge one.
    ///
    /// THE BOARD: the entry carries `Effect::PayCost { payer: Controller }` — a member of
    /// `effect_resolution_choice_freedom`'s fail-closed grouped arm, i.e. exactly U2 arm
    /// (b)'s subject — announced on the retained carrying frame and ABSENT from
    /// `current.stack`. Conjunct (6) must still refuse it.
    ///
    /// REVERT-PROBE: narrow the RESOLUTION loop back to `current.stack` only (drop the
    /// `touch.announced` half of `pairs`) ⇒ the pair is never classified ⇒ the predicate
    /// returns `true` ⇒ FLIPS. That is what makes "every minted pair is scanned" a MEASURED
    /// premise rather than a stated one.
    #[test]
    fn r31_completeness_an_announced_off_stack_pair_is_still_scanned_by_conjunct_six() {
        use crate::game::engine::entry_publishes_pin_slots;
        use crate::types::ability::{AbilityCost, Effect, TargetFilter};

        let (frame, src) = u2_relief_board();
        let entry = u2_shape_b_entry(
            src,
            9310,
            Effect::PayCost {
                cost: AbilityCost::Mana {
                    cost: crate::types::mana::ManaCost::Cost {
                        shards: vec![],
                        generic: 1,
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            |_| {},
        );
        let may = entry_publishes_pin_slots(&frame, &entry, PlayerId(0))
            .expect("reach-guard: the fixture must reach the mint")
            .may
            .expect(
                "reach-guard: it publishes its CR 603.5 gate, so the refusal below is the \
                     RESIDUAL's and not the mint's",
            );

        // oldest ⇒ carrying ⇒ current: the entry announces on `carrying` and has RESOLVED
        // OFF the stack by `current`.
        let oldest = frame.clone();
        let mut carrying = frame.clone();
        carrying.stack.push_back(entry.clone());
        let current = frame.clone();

        let ring = [&oldest, &carrying];
        let touch =
            certified_period_touch(&ring, &current, PeriodCertification::ResourceSignatureOnly);

        // ── reach-guards: the pair is reachable ONLY through `announced` ────────────────
        assert!(
            current.stack.is_empty(),
            "reach-guard: with the entry still on `current.stack` this arm would be U2's \
             arm (b) again and the completeness premise would go untested"
        );
        assert!(
            touch
                .announced
                .iter()
                .any(|(f, e)| std::ptr::eq(*f, &carrying) && e.id == entry.id),
            "reach-guard: the window must actually MINT the pair, or the refusal below would \
             be a refusal over an empty population"
        );
        assert!(
            stack_entry_has_no_ordering_input(&carrying, &entry),
            "reach-guard: shape (B) announces no choice, so the ANNOUNCEMENT loop passes and \
             the refusal below is attributable to conjunct (6)"
        );

        let mut verdicts = PeriodVerdicts::for_period(&ring, &current, PlayerId(0));
        assert!(
            !stack_choices_are_all_specified(
                &current,
                PlayerId(0),
                std::slice::from_ref(&may),
                Some(&touch),
                &mut verdicts
            ),
            "CR 732.2a: the described sequence is EVERY choice the shortcut makes, not the \
             subset that happens to sit on the stack at the offer beat. A `PayCost` residual \
             sits in the fail-closed grouped arm, so an announced-then-resolved pair must \
             still refuse the offer"
        );
        assert!(
            verdicts.conjunct6_asks() >= 1,
            "attribution: conjunct (6) must have ASKED about the off-stack pair — a `false` \
             with zero asks would be some other gate's refusal. asks={}",
            verdicts.conjunct6_asks()
        );
    }

    /// R22's own NEGATIVE CONTROL — the VACUITY BOUNDARY, never coverage.
    ///
    /// At window offset zero the window-relative position EQUALS the absolute one
    /// (`w_pos == abs ⟺ idx == 0`) and an id-only memo key serves the same frame it would
    /// have computed anyway, so the shipped mint and BOTH reverts AGREE. Round 15's re-key
    /// from two ring frames to three reproduced the diagnosed defect one parameter over
    /// precisely because a "three-frame period" taken as the WHOLE ring plus `current` is
    /// still `idx == 0`.
    ///
    /// This test is GREEN under every probe R22 runs. That measured agreement is the whole
    /// point: it is what a fixture at this offset can prove, which is nothing.
    #[test]
    fn r22_control_at_window_offset_zero_the_two_key_arithmetics_agree() {
        let (base, src) = u2_relief_board();
        let entry = u2_shape_b_entry(src, 9221, u2_draw_effect(), |_| {});
        let f1 = base.clone();
        let mut f2 = base.clone();
        f2.stack.push_back(entry.clone());
        let mut current = base.clone();
        current.stack.push_back(entry.clone());

        let ring = [&f1, &f2];
        let mut flat = PeriodVerdicts::for_period(&ring, &current, PlayerId(0));
        let f = flat
            .frame_ix(&f1)
            .expect("the control's carrying frame is the FIRST — that is the whole defect");
        // `FrameIx`'s field is private outside its module — deliberately, it is a token and not
        // a number — so offset zero is asserted by ORDER: nothing in the table precedes it.
        let ix2 = flat.frame_ix(&f2).expect("f2 is in the period");
        let ixc = flat.frame_ix(&current).expect("current is in the period");
        assert!(
            f < ix2 && ix2 < ixc,
            "the control is only a control AT offset zero: if the carrying frame stopped being \
             the FIRST, the fixture drifted into the discriminating shape and this test would \
             start claiming coverage it cannot support"
        );
        assert!(
            flat.verdict(f, &entry).published.is_some(),
            "at idx == 0 every key arithmetic agrees — measured, and reported as a boundary \
             rather than as evidence"
        );
    }

    /// R22 — THE VERDICT DOOR IS TOTAL, FRAME-CORRECT AND PROPOSER-CORRECT.
    ///
    /// The class row for the derived cache, and the ONLY row that can lose if a key-component
    /// error returns. FIVE conjuncts.
    ///
    /// **(1) TOTALITY, STRUCTURAL.** `PeriodVerdicts::verdict(&mut self, f: FrameIx,
    /// entry: &StackEntry) -> &EntryVerdict`. ⚠ AN `Option` RETURN, AN INDEXING OPERATOR, OR
    /// ANY SIBLING `get(..)` ACCESSOR IS FORBIDDEN — a partial container has a MISS CONTRACT,
    /// and a row asserting how a miss behaves asserts an unreachable state. Totality is scoped
    /// honestly: it is total over every `FrameIx` THIS container minted, and MINTING
    /// (`frame_ix`) is the membership question — conjunct (2′) is that half.
    ///
    /// **(2) FRAME-CORRECTNESS — THE PIN IS THE WINDOW OFFSET, NOT THE FRAME COUNT.** Round 15
    /// re-keyed this two frames → three and reproduced the diagnosed defect one parameter over:
    /// `w_pos == abs ⟺ idx == 0`, so a "three-frame period" built as the WHOLE ring plus
    /// `current` is exactly as vacuous as the two-frame form it replaced. The construction
    /// requirement is therefore `ring.len() >= 3` **AND** the candidate window starting at
    /// `idx >= 1` — a STRICT SUFFIX of the ring plus `current` — and BOTH are carried as
    /// EXECUTABLE reach-guards, not prose. The period is built so the SAME entry id classifies
    /// DIFFERENTLY per frame (the source object is absent from the older frame, so the mint
    /// answers `None` there and `Some` on the carrying frame), and the consumed verdict must be
    /// the CARRYING frame's, never the window-relative position's.
    ///
    /// The `idx == 0` shape is RETAINED as this conjunct's own NEGATIVE CONTROL: there the
    /// shipped mint and the window-relative revert AGREE, and that measured agreement is the
    /// VACUITY BOUNDARY — it must never be reported as coverage.
    ///
    /// **(2′) MINT-IDENTITY.** Every announced pair of a real window resolves through
    /// `frame_ix`, and a FOREIGN frame — a fresh clone, byte-equal in CONTENT — resolves to
    /// `None` and the consumer REFUSES. Correctness by IDENTITY, which index arithmetic cannot
    /// satisfy on any `idx > 0` candidate.
    ///
    /// **(3) CONTAINER/CURRENT AGREEMENT — a PRODUCTION guard, not a `debug_assert`.**
    /// Consumers resolve their own `current` through `frame_ix`; a memo built over a DIFFERENT
    /// frame set yields `None` ⇒ certification refuses.
    ///
    /// **(4) PROPOSER COMPLETENESS.** The effective key is `(proposer, FrameIx, ObjectId)` with
    /// the first component constant per container: a `for_period(A)` container publishes for A
    /// while an `unproven` container publishes NOTHING for the SAME (frame, entry) — and each
    /// resolves that frame through its OWN `frame_ix`, because a `FrameIx` never crosses the
    /// container that minted it. The relief's agreement guard is the second half: pins minted
    /// for A consumed under a container bound to B get `None`.
    ///
    /// REVERT-PROBES (all three RUN, see the journal):
    /// * **(1)/(2)** key the memo by `ObjectId` ALONE (drop the `FrameIx` component) ⇒ the
    ///   id-keyed blindness returns, one entry gets ONE verdict across frames ⇒ (2) FLIPS while
    ///   the `idx == 0` control stays green — which is exactly the vacuity boundary.
    /// * **(2′)/(3)** make the `current` resolution UNCHECKED (return the last index without
    ///   the `ptr::eq` test) ⇒ the foreign clone resolves and the mismatched memo certifies
    ///   against the wrong frame set ⇒ (2′) and (3) FLIP.
    /// * **(4)** hard-code the container's proposer ⇒ the `for_period(A)`-vs-`unproven` pair
    ///   collapses to one answer ⇒ FLIPS.
    #[test]
    fn r22_the_verdict_door_is_total_frame_correct_and_proposer_correct() {
        use crate::game::engine::entry_publishes_pin_slots;

        let (base, src) = u2_relief_board();
        let entry = u2_shape_b_entry(src, 9220, u2_draw_effect(), |_| {});

        // The per-frame difference: the source object is GONE from the oldest frame, so the
        // mint's `object_decision_source` conjunct answers `None` there and `Some` elsewhere.
        // One entry id, two different verdicts — which is what an id-only key cannot express.
        let mut f0 = base.clone();
        f0.objects.remove(&src);
        let f1 = base.clone();
        // The entry ANNOUNCES at f2 — absent from the window's first frame, present after —
        // which is what gives conjunct (2′) a real announced pair to quantify over.
        let mut f2 = base.clone();
        f2.stack.push_back(entry.clone());
        let mut current = base.clone();
        current.stack.push_back(entry.clone());

        assert!(
            entry_publishes_pin_slots(&f1, &entry, PlayerId(0)).is_some()
                && entry_publishes_pin_slots(&f0, &entry, PlayerId(0)).is_none(),
            "(2) reach-guard: the two frames must give the SAME entry id DIFFERENT mint \
             answers, or the frame component of the key is unobservable and every assertion \
             below is vacuous"
        );

        // ── (2) THE OPERATING POINT: ring >= 3 AND the window a STRICT suffix (idx >= 1) ────
        let ring = [&f0, &f1, &f2];
        let idx = 1usize;
        assert!(
            ring.len() >= 3,
            "(2) construction requirement: fewer than three ring frames cannot carry a strict \
             suffix, and the round-14 two-frame form was measured vacuous"
        );
        assert!(
            idx >= 1,
            "(2) construction requirement: at idx == 0 the window-relative position EQUALS the \
             absolute one (`w_pos == abs ⟺ idx == 0`) and the revert cannot flip"
        );
        let window = &ring[idx..];

        let mut verdicts = PeriodVerdicts::for_period(&ring, &current, PlayerId(0));
        let ix0 = verdicts.frame_ix(&f0).expect("f0 is in the period");
        let ix1 = verdicts.frame_ix(&f1).expect("f1 is in the period");
        let ix2 = verdicts.frame_ix(&f2).expect("f2 is in the period");
        let ixc = verdicts
            .frame_ix(&current)
            .expect("current is in the period");
        assert!(
            ix0 < ix1 && ix1 < ix2 && ix2 < ixc,
            "(2) the frames table is ordered by IDENTITY, ring-then-current: {ix0:?} {ix1:?} \
             {ix2:?} {ixc:?}"
        );
        assert_ne!(
            ix1, ix0,
            "(2) reach-guard `abs != w_pos`: the CARRYING frame f1 sits at absolute index 1 \
             while window-relative arithmetic would place it at 0 (= f0). Equal indices here \
             would mean the fixture drifted back to the vacuous idx == 0 shape"
        );

        // (1) TOTALITY + (2) FRAME-CORRECTNESS on one run: a pair that was never pre-computed
        // returns a REAL verdict (the signature returns `&EntryVerdict`, never an `Option`),
        // and the two frames answer DIFFERENTLY for one entry id.
        assert!(
            verdicts.verdict(ix1, &entry).published.is_some(),
            "(2) the consumed verdict is the CARRYING frame's — f1 holds the source object, so \
             the mint publishes"
        );
        assert!(
            verdicts.verdict(ix0, &entry).published.is_none(),
            "(2) …and the OLDER frame's answer is its own. Under window-relative arithmetic \
             (or an id-only key) the carrying frame's cached `Some` would be served here"
        );

        // The idx == 0 negative control lives in its own test — see
        // `r22_control_at_window_offset_zero_the_two_key_arithmetics_agree`, which must stay
        // GREEN under this row's probes and must never be reported as coverage.

        // ── (2′) MINT-IDENTITY: every announced pair resolves; a foreign CLONE does not ─────
        let touch =
            certified_period_touch(window, &current, PeriodCertification::ResourceSignatureOnly);
        assert!(
            !touch.announced.is_empty(),
            "(2′) reach-guard: the window must actually announce something, or the loop below \
             quantifies over nothing"
        );
        for (frame, pair_entry) in &touch.announced {
            assert!(
                verdicts.frame_ix(frame).is_some(),
                "(2′) every announced pair's carrying frame is minted by `frame_ix` — entry {:?}",
                pair_entry.id
            );
        }
        let foreign = current.clone();
        assert!(
            verdicts.frame_ix(&foreign).is_none(),
            "(2′) POINTER IDENTITY, NOT EQUALITY: a fresh clone is byte-equal in content and \
             still outside the period, so it must not resolve. Index arithmetic cannot make \
             this distinction at all"
        );
        // …and the CONSUMER refuses on it, fail-closed.
        let foreign_touch = PeriodTouch {
            announced: vec![(&foreign, &entry)],
            frozen_ids: BTreeSet::new(),
        };
        let mut v_foreign = PeriodVerdicts::for_period(&ring, &current, PlayerId(0));
        assert!(
            !stack_choices_are_all_specified(
                &current,
                PlayerId(0),
                &[],
                Some(&foreign_touch),
                &mut v_foreign
            ),
            "(2′) a frame outside this container's period costs a CERTIFICATE, never a wrong \
             one — the consumer's let-else on `frame_ix(..)` returns `false` and that is the \
             fail-closed arm"
        );

        // Conjuncts (3) and (4) are separate tests below, so each one's revert-probe is
        // measured on its own assertion rather than shadowed by an earlier panic.
    }

    /// R22 conjunct (3) — CONTAINER/CURRENT AGREEMENT IS A **PRODUCTION** GUARD.
    ///
    /// A `debug_assert` here would be compiled out of release and the mismatch would then be
    /// silent in exactly the build that ships. Asserted as a MATCHED PAIR: the positive proves
    /// the board certifies at all (without it the negative passes over a board that refuses for
    /// unrelated reasons), the negative differs ONLY in which frame set the memo was built over.
    ///
    /// REVERT-PROBE (RUN): make `frame_ix` resolve unchecked — fall back to the last index
    /// instead of returning `None` — and the mismatched container certifies against the wrong
    /// frame set ⇒ the negative FLIPS.
    #[test]
    fn r22_conjunct3_a_memo_over_a_different_frame_set_refuses_certification() {
        use crate::game::engine::entry_publishes_pin_slots;

        let (base, src) = u2_relief_board();
        let entry = u2_shape_b_entry(src, 9222, u2_draw_effect(), |_| {});
        let mut relieved_board = base.clone();
        relieved_board.stack.push_back(entry.clone());
        let may = entry_publishes_pin_slots(&relieved_board, &entry, PlayerId(0))
            .expect("(3) reach-guard: the fixture reaches the mint")
            .may
            .expect("(3) reach-guard: it publishes its CR 603.5 gate");
        let mut matched = PeriodVerdicts::for_period(&[], &relieved_board, PlayerId(0));
        assert!(
            stack_choices_are_all_specified(
                &relieved_board,
                PlayerId(0),
                std::slice::from_ref(&may),
                None,
                &mut matched
            ),
            "(3) POSITIVE: a container that holds the caller's `current` certifies this board — \
             without it the negative below would pass over a board that refuses anyway"
        );
        let other_current = base.clone();
        let mut mismatched = PeriodVerdicts::for_period(&[], &other_current, PlayerId(0));
        assert!(
            !stack_choices_are_all_specified(
                &relieved_board,
                PlayerId(0),
                std::slice::from_ref(&may),
                None,
                &mut mismatched
            ),
            "(3) NEGATIVE: a memo built over a DIFFERENT frame set yields `None` for the \
             caller's `current`, so certification refuses. This is a PRODUCTION guard, not a \
             `debug_assert` compiled out of release"
        );
    }

    /// R22 conjunct (4) — THE EFFECTIVE KEY CARRIES THE CONTAINER'S PROPOSER.
    ///
    /// `(proposer, FrameIx, ObjectId)`, with the first component constant per container and
    /// therefore held by the container rather than by the memo key. Both halves are asserted:
    /// a `for_period(A)` container publishes where an `unproven` one publishes NOTHING for the
    /// SAME (frame, entry) — each resolving that frame through its OWN `frame_ix`, because a
    /// `FrameIx` never crosses the container that minted it — and the relief refuses pins
    /// minted for A when the container is bound to B (CR 603.5: the cached `published` IS the
    /// mint's answer for the CONTAINER's proposer).
    ///
    /// REVERT-PROBES (both RUN): hard-code the proposer inside `verdict` ⇒ the
    /// `for_period(A)`-vs-`unproven` pair collapses to one answer ⇒ FLIPS. Delete
    /// `pinned_may_choice_relief`'s agreement guard ⇒ B's container relieves A's pins ⇒ FLIPS.
    #[test]
    fn r22_conjunct4_the_effective_key_carries_the_containers_proposer() {
        use crate::game::engine::entry_publishes_pin_slots;

        let (base, src) = u2_relief_board();
        let entry = u2_shape_b_entry(src, 9223, u2_draw_effect(), |_| {});
        let mut relieved_board = base.clone();
        relieved_board.stack.push_back(entry.clone());
        let mut bound_to_a = PeriodVerdicts::for_period(&[], &relieved_board, PlayerId(0));
        let fa = bound_to_a
            .frame_ix(&relieved_board)
            .expect("(4) the container holds the board");
        assert!(
            bound_to_a.verdict(fa, &entry).published.is_some(),
            "(4) reach-guard, MANDATORY: the `Some` arm must fire, or the `None` below is \
             satisfied by a board that publishes nothing to anyone"
        );
        let mut unproven_c = PeriodVerdicts::unproven(&relieved_board);
        let fu = unproven_c.frame_ix(&relieved_board).expect(
            "(4) each container resolves the frame through its OWN `frame_ix` — a \
                     `FrameIx` never crosses the container that minted it",
        );
        assert!(
            unproven_c.verdict(fu, &entry).published.is_none(),
            "(4) an `unproven` container binds NO proposer, so nothing is published: that is \
             the mint's own answer for 'no offer binds a proposer', not an invented one"
        );
        // ── The relief half: pins minted for A, container bound to B ────────────────────────
        //
        // ⚠ THE VACUITY THIS ARM HAD TO ESCAPE. Run against the P0-controlled `entry` above,
        // this arm passes with the agreement guard DELETED — measured: probe P4 left it green.
        // The mint's own `entry.controller != proposer` conjunct already answers `None` for B,
        // so the negative was satisfied upstream of the guard it claimed to cover. The entry
        // below is controlled by B, so B's container genuinely publishes and the guard is the
        // ONLY thing left standing between A's pins and a relief minted for another seat.
        let mut b_entry = u2_shape_b_entry(src, 9224, u2_draw_effect(), |a| {
            a.controller = PlayerId(1);
        });
        b_entry.controller = PlayerId(1);
        let mut b_board = base.clone();
        b_board.stack.push_back(b_entry.clone());
        let b_may = entry_publishes_pin_slots(&b_board, &b_entry, PlayerId(1))
            .expect("(4) reach-guard: B's OWN entry reaches the mint under B")
            .may
            .expect("(4) reach-guard: and publishes its CR 603.5 gate under B");
        let b_slots = std::slice::from_ref(&b_may);

        let mut bound_to_b = PeriodVerdicts::for_period(&[], &b_board, PlayerId(1));
        let fb = bound_to_b
            .frame_ix(&b_board)
            .expect("(4) B's container holds the board");
        assert!(
            pinned_may_choice_relief(
                fb,
                &b_entry,
                &mut bound_to_b,
                u3_scope_for(PlayerId(1), b_slots)
            )
            .is_some(),
            "(4) POSITIVE REACH-GUARD: under B's OWN pins this entry IS relieved. Without this \
             the negative below would be satisfied by an entry nobody can relieve"
        );
        let mut bound_to_b2 = PeriodVerdicts::for_period(&[], &b_board, PlayerId(1));
        let fb2 = bound_to_b2
            .frame_ix(&b_board)
            .expect("(4) same board, fresh container so the memo cannot carry B's answer over");
        assert!(
            pinned_may_choice_relief(
                fb2,
                &b_entry,
                &mut bound_to_b2,
                u3_scope_for(PlayerId(0), b_slots)
            )
            .is_none(),
            "(4) CR 603.5: pins minted by A's offer may never be spent against a verdict this \
             container minted for B — the cached `published` IS the mint's answer for the \
             CONTAINER's proposer, so consuming it under another seat's pins would relieve a \
             choice that seat never described"
        );
    }

    // ───────────────────────────────────────────────────────────────────────────────────
    // §6 R27 — THE `.live`-READER CONJUNCTS (a3) / (b) / (c) / (e)
    //
    // All four need what (a1)/(a2) did not: an announced pair whose CARRYING FRAME is a
    // RETAINED SAMPLE rather than `current`. The shared fixture below is the only thing that
    // makes them differ from the shipped `.normalized`-blind rows — and, measured, it is what
    // lets (b) and (c) flip BEHAVIOURALLY on the mint's own carrier line, which round 7's
    // self-built-window rows structurally could not (probe P6).
    // ───────────────────────────────────────────────────────────────────────────────────

    /// A retained ring whose NEWEST sample carries stack entries neither the older samples nor
    /// the live board hold.
    ///
    /// CR 732.2a + CR 608.1: [`certified_period_touch`] announces an entry at the FIRST window
    /// frame it appears on, so an entry seeded only into the newest retained sample is
    /// announced with THAT SAMPLE as its carrying frame — the `frame != current` population
    /// every `.live` reader is about, and the one a fixture assembled from `current.stack`
    /// can never reach. `state.stack` is deliberately left EMPTY: with no live entry at all,
    /// an arm that passed by reading the live board would have nothing to read.
    ///
    /// Certification is basis A's EQUALITY disjunct, and that is a construction property
    /// rather than a hope: `setup` runs BEFORE any frame is snapshotted, so `ring[1]`
    /// (the certifying prior at `span == 1`) and `current` agree on everything, and only
    /// `ring[2]` — which the equality disjunct never compares — is widened. Both halves of
    /// every sample are built exactly as `record_loop_detect_sample` builds them, so the
    /// fixture cannot diverge from production's construction; in particular the `.normalized`
    /// half really is `normalize_for_loop`d, which is what the instrument-liveness controls
    /// below depend on.
    ///
    /// The per-frame life step is `game::engine`'s `drain_ring` orientation — frame `i` sits
    /// `FRAMES - i` life above the live board — so the certified period moves a real CR 704.5a
    /// resource and step (7) publishes a bound instead of refusing at `NoNarrowedLegalCount`.
    fn ring_announcing_on_its_newest_sample(
        setup: impl Fn(&mut GameState),
        newest_sample: impl Fn(&mut GameState),
    ) -> GameState {
        use crate::game::scenario::GameScenario;
        use crate::types::game_state::{LoopDetectionMode, WaitingFor};
        use crate::types::LoopDetectSample;

        const FRAMES: usize = 3;
        let mut scenario = GameScenario::new_n_player(2, 7);
        // Stocked libraries are load-bearing for the arms whose announced entry DRAWS: an
        // empty library derives no `ZoneChange`, which moves the classification for a reason
        // that has nothing to do with the carrier axis.
        let names: Vec<String> = (0..40).map(|i| format!("Filler {i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        scenario.with_library_top(PlayerId(0), &refs);
        scenario.with_library_top(PlayerId(1), &refs);
        let mut state = scenario.build().state().clone();
        state.loop_detection = LoopDetectionMode::Interactive;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.active_player = PlayerId(0);
        state.last_loop_action_sequence.clear();
        setup(&mut state);
        for i in 0..FRAMES {
            let mut frame = state.clone();
            frame
                .players
                .iter_mut()
                .find(|p| p.id == PlayerId(1))
                .expect("the two-seat scenario always has P1")
                .life += (FRAMES - i) as i32;
            if i + 1 == FRAMES {
                newest_sample(&mut frame);
            }
            state
                .loop_detect_ring
                .push_back(std::sync::Arc::new(LoopDetectSample {
                    normalized: frame.normalize_for_loop(),
                    live: frame.loop_detect_live_sample(),
                }));
        }
        state
    }

    /// The battlefield source every announced entry names. `entered_battlefield_turn` is set
    /// to the live turn because R27 (c)'s intervening-if reads exactly that through its
    /// `TriggerSourceContext` (CR 603.4); the other three arms are indifferent to it.
    fn announcing_ring_source(state: &mut GameState, id: u64) -> ObjectId {
        let oid = ObjectId(id);
        let mut object = GameObject::new(
            oid,
            CardId(77),
            PlayerId(0),
            "Retained Sample Source".to_string(),
            Zone::Battlefield,
        );
        object.card_types.core_types = vec![CoreType::Creature];
        object.incarnation = 3;
        object.entered_battlefield_turn = Some(state.turn_number);
        state.objects.insert(oid, object);
        state.battlefield.push_back(oid);
        oid
    }

    /// One proposer-controlled triggered-ability entry for the newest sample's stack.
    fn announced_trigger_entry(
        id: u64,
        src: ObjectId,
        ability: crate::types::ability::ResolvedAbility,
        condition: Option<crate::types::ability::TriggerCondition>,
    ) -> StackEntry {
        StackEntry {
            id: ObjectId(id),
            source_id: src,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: src,
                ability: Box::new(ability),
                condition,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        }
    }

    /// The ONE reach-guard every R27 `.live` arm runs before it asserts anything: the fixture
    /// really announces from a retained sample, the live board carries no entry at all, and
    /// the pair the production enumerator hands its consumers is that sample's — not
    /// `current`'s. Returns the announced pair's carrying frame.
    fn announced_from_retained_sample(state: &GameState, entry_id: u64) -> &GameState {
        assert!(
            state.stack.is_empty(),
            "REACH-GUARD: `current.stack` must be EMPTY, else an arm could be satisfied by the \
             live board and the `.live`-reader claim would not be under test"
        );
        let retained: Vec<&GameState> = state.loop_detect_ring.iter().map(|f| &f.live).collect();
        assert!(
            retained.len() >= 3,
            "REACH-GUARD: the walk needs `span >= 1` at `idx = len - 2`; got {} frames",
            retained.len()
        );
        let touch = certified_period_touch(
            &retained[retained.len() - 2..],
            state,
            PeriodCertification::BoardEqualOnly,
        );
        let (frame, entry) = *touch
            .announced
            .first()
            .expect("REACH-GUARD: the newest sample's extra entry must ANNOUNCE in the window");
        assert_eq!(
            (touch.announced.len(), entry.id),
            (1, ObjectId(entry_id)),
            "REACH-GUARD: exactly the constructed entry announces, so every assertion below is \
             about it and not about an incidental second pair"
        );
        assert!(
            !std::ptr::eq(frame, state),
            "REACH-GUARD (the R27 precondition, executable rather than prose): the pair's \
             carrying frame must NOT be `current`. A fixture drift that collapsed the pair onto \
             the live board would leave every arm below passing for the degenerate reason"
        );
        frame
    }

    // ───────────────────────────────────────────────────────────────────────────────────
    // N3 — the CR 616.1 obligation is discharged against the LIVE board as well as the
    // carrying frame, because the shortcut's remaining repetitions resolve under the board
    // that exists NOW.
    // ───────────────────────────────────────────────────────────────────────────────────

    /// An OPTIONAL (or mandatory) `Draw` replacement definition, of exactly the shape
    /// `find_applicable_replacements` draws for a `ProposedEvent::Draw`.
    fn n3_draw_replacement(optional: bool) -> crate::types::ability::ReplacementDefinition {
        use crate::types::ability::{
            DrawReplacementScope, QuantityModification, ReplacementDefinition, ReplacementMode,
        };
        use crate::types::replacements::ReplacementEvent;
        let mut def = ReplacementDefinition::new(ReplacementEvent::Draw);
        if optional {
            def.mode = ReplacementMode::Optional { decline: None };
        }
        // CR 121.2: a Draw definition must declare its stage or the pipeline debug-asserts.
        def.draw_scope = Some(DrawReplacementScope::IndividualDraw);
        def.quantity_modification = Some(QuantityModification::Plus { value: 1 });
        def
    }

    /// N3 — **A REPLACEMENT THAT ENTERED PLAY AFTER THE FRAME WAS CAPTURED STILL REFUSES.**
    ///
    /// CR 616.1 + CR 732.2a. Conjunct (6) classifies each announced entry on its CARRYING
    /// FRAME, which is a retained ring sample and therefore a board from the PAST. Discharging
    /// the resulting `FreeUnlessReplacements` obligation against that frame alone answers the
    /// wrong question: the shortcut is a claim about the FUTURE, and every remaining repetition
    /// resolves under the board that exists NOW. A definition that entered the battlefield
    /// after the sample was taken is invisible to the frame-side discharge — and it is exactly
    /// the CR 616.1 resolution-time choice CR 732.2a forbids a described sequence from
    /// containing.
    ///
    /// The fixture makes the two boards differ in EXACTLY that field: every ring frame is
    /// cloned before the definition is installed, so the def exists on `state` and nowhere
    /// else. `announced_from_retained_sample` is the reach-guard that the pair really is
    /// carried by a frame that is not `current` — without it every arm here would be about the
    /// first discharge.
    ///
    /// | arm | where the def lives | mode | gate |
    /// |---|---|---|---|
    /// | (pos) | nowhere | — | **specified** |
    /// | (live) | live board only | OPTIONAL | **refused** |
    /// | (live-mandatory) | live board only | mandatory | **specified** |
    /// | (both) | every frame AND live | OPTIONAL | **refused** |
    ///
    /// (live-mandatory) is what keys (live) to OPTIONALITY rather than to "a definition
    /// exists"; (both) proves the frame-side discharge is still doing its own job, so (live)
    /// is a strictly ADDED refusal and not a relocated one.
    ///
    /// REVERT-PROBE: delete the second `resolution_events_are_discharged(state, ..)` call ⇒
    /// arm (live) certifies ⇒ FLIPS, while (pos), (live-mandatory) and (both) are unmoved.
    #[test]
    fn n3_a_replacement_installed_after_the_frame_was_captured_refuses_certification() {
        let announced_id = 9310u64;
        let build = |where_def: Option<(bool, bool)>| -> GameState {
            // `where_def = Some((in_frames, optional))`.
            let in_frames = where_def.is_some_and(|(f, _)| f);
            let optional = where_def.is_some_and(|(_, o)| o);
            let mut state = ring_announcing_on_its_newest_sample(
                |st| {
                    let src = announcing_ring_source(st, 931);
                    if in_frames {
                        st.objects
                            .get_mut(&src)
                            .expect("just inserted")
                            .replacement_definitions
                            .push(n3_draw_replacement(optional));
                    }
                },
                |frame| {
                    let src = ObjectId(931);
                    let ability = crate::types::ability::ResolvedAbility::new(
                        u2_draw_effect(),
                        vec![],
                        src,
                        PlayerId(0),
                    );
                    frame.stack.push_back(announced_trigger_entry(
                        announced_id,
                        src,
                        ability,
                        None,
                    ));
                },
            );
            if where_def.is_some() && !in_frames {
                state
                    .objects
                    .get_mut(&ObjectId(931))
                    .expect("the announcing source is on the live board too")
                    .replacement_definitions
                    .push(n3_draw_replacement(optional));
            }
            state
        };

        let gate = |state: &GameState| -> bool {
            // REACH-GUARD, run on every arm: the pair is carried by a retained frame that is
            // NOT `current`, so the second discharge is reachable at all.
            announced_from_retained_sample(state, announced_id);
            let ring: Vec<&GameState> = state.loop_detect_ring.iter().map(|f| &f.live).collect();
            let cover = certified_period_touch(
                &ring[ring.len() - 2..],
                state,
                PeriodCertification::BoardEqualOnly,
            );
            let mut verdicts = PeriodVerdicts::for_period(&ring, state, PlayerId(0));
            stack_choices_are_all_specified(state, PlayerId(0), &[], Some(&cover), &mut verdicts)
        };

        assert!(
            gate(&build(None)),
            "(pos) MATCHED POSITIVE, asserted first: with no replacement definition anywhere \
             the announced mandatory draw is choice-free and the period certifies. Without \
             this arm every refusal below could belong to an unrelated conjunct"
        );
        assert!(
            !gate(&build(Some((false, true)))),
            "(live) CR 616.1 + CR 732.2a: an OPTIONAL definition that exists on the LIVE board \
             and on no retained frame is a real resolution-time choice for every remaining \
             repetition. The frame-side discharge cannot see it — this is the arm the second \
             discharge exists for"
        );
        assert!(
            gate(&build(Some((false, false)))),
            "(live-mandatory) the SAME live-only definition, MANDATORY, opens no choice and the \
             period still certifies. Without this arm (live) would be keyed to `a definition \
             exists` rather than to OPTIONALITY"
        );
        assert!(
            !gate(&build(Some((true, true)))),
            "(both) the definition present in every frame AND live still refuses — the \
             frame-side discharge keeps doing its own job, so (live) is an ADDED refusal and \
             not a relocated one"
        );
    }

    /// N3, the `ptr::eq` short-circuit — **SKIPPING THE SECOND DISCHARGE WHEN THE CARRYING
    /// FRAME *IS* THE LIVE BOARD COSTS NOTHING.**
    ///
    /// CR 616.1. The second discharge is guarded by `!std::ptr::eq(*frame, state)`, which is a
    /// de-duplication and not a hole: when the announced pair is carried by `current` itself
    /// the FIRST discharge already ran against that very board. This row exhibits that arm —
    /// an entry on `current`'s own stack, no ring at all — and shows the optional definition is
    /// still refused, on the same board shape where the guard suppresses the second call.
    ///
    /// Paired with a positive on the identical board one field apart (the mandatory mode), so
    /// the refusal is attributable to the definition rather than to the `frames: &[]` shape.
    #[test]
    fn n3_b_a_live_carried_pair_is_still_discharged_by_the_first_call() {
        let board = |optional: Option<bool>| -> GameState {
            let (mut state, src) = u2_relief_board();
            let entry = u2_shape_b_entry(src, 9311, u2_draw_effect(), |ability| {
                // MANDATORY: an optional ability classifies `MayPrompt` and never reaches the
                // discharge at all, which would make both arms below vacuous.
                ability.optional = false;
            });
            if let Some(optional) = optional {
                state
                    .objects
                    .get_mut(&src)
                    .expect("u2's source is on the battlefield")
                    .replacement_definitions
                    .push(n3_draw_replacement(optional));
            }
            state.stack.push_back(entry);
            state
        };
        let gate = |state: &GameState| -> bool {
            let mut verdicts = PeriodVerdicts::for_period(&[], state, PlayerId(0));
            stack_choices_are_all_specified(state, PlayerId(0), &[], None, &mut verdicts)
        };

        assert!(
            gate(&board(None)),
            "REACH-GUARD: with no definition the live-carried entry certifies, so the arms \
             below are about the definition and not about the `frames: &[]` shape"
        );
        assert!(
            gate(&board(Some(false))),
            "a MANDATORY definition opens no CR 616.1 choice — the paired positive"
        );
        assert!(
            !gate(&board(Some(true))),
            "an OPTIONAL definition on a LIVE-CARRIED pair is still refused by the FIRST \
             discharge, which is why the second one is guarded by `ptr::eq` rather than \
             unconditional: the guard removes a duplicate call, never a refusal"
        );
    }

    /// R27 (a3) — THE BEHAVIOUR: A RETAINED SAMPLE DERIVES THE SAME EVENT SET THE LIVE BOARD
    /// DOES, AND THE NORMALIZED HALF DOES NOT.
    ///
    /// CR 732.2a + CR 104.4b + CR 111.1. (a2) pinned that every carrying frame is an
    /// un-normalized board by reading `next_object_id`; this arm pins the CONSEQUENCE — that
    /// the classification a `.live` carrier produces for an `Effect::Token` announcement is
    /// the classification the live board produces, event for event.
    ///
    /// THE INSTRUMENT-LIVENESS CONTROL IS THE ARM THAT MAKES THE EQUALITY MEAN ANYTHING, and
    /// it is MEASURED rather than predicted. The plan forecast the divergence at the
    /// ALLOCATOR (`create_object` handing out `ObjectId(0)` over a live object); measured, the
    /// derivation diverges one field earlier and more directly — `normalize_for_loop` runs
    /// `clear_trigger_identity_recursive`, which sets `ability.source_id = ObjectId(0)`, and
    /// the resolver carries that straight into `TokenSpec.source_id`. So a normalized carrier
    /// proposes a token whose CR 111.1 source is the null object, and the two sets differ.
    ///
    /// ⚠ SCOPE, stated because a reader will ask why this arm is not an offer-level one:
    /// BOTH derivations are `event_is_accounted`, so the mint OFFERS on either carrier and
    /// (a3) alone has NO behavioural flip at the seam. The carrier axis IS flipped
    /// behaviourally, on exactly the shared revert the plan names, by
    /// `r27_b_a_stored_may_auto_choice_survives_the_ring` and
    /// `r27_c_an_intervening_if_binds_with_the_retained_samples_trigger_source` below, and
    /// structurally by `game::engine`'s `the_period_touch_window_is_carried_by_the_live_half`.
    /// This arm's own flipping site is the control's: delete
    /// `ResolvedAbility::clear_trigger_identity_recursive`'s `self.source_id = ObjectId(0)`
    /// ⇒ the two halves stop differing ⇒ the control FAILS.
    #[test]
    fn r27_a3_a_retained_sample_derives_the_live_boards_event_set() {
        use crate::game::engine::{try_offer_bounded_cycle_shortcut_metered, ProbeCap};
        use crate::game::resolution_prompt::ResolutionChoiceFreedom;
        use crate::types::ability::{Effect, PtValue, QuantityExpr, ResolvedAbility, TargetFilter};
        use crate::types::proposed_event::ProposedEvent;

        const SRC: ObjectId = ObjectId(940);
        const ENTRY: u64 = 954;
        let state = ring_announcing_on_its_newest_sample(
            |s| {
                announcing_ring_source(s, SRC.0);
            },
            |frame| {
                let ability = ResolvedAbility::new(
                    Effect::Token {
                        name: "Servo".to_string(),
                        power: PtValue::Fixed(1),
                        toughness: PtValue::Fixed(1),
                        types: vec!["Artifact".to_string(), "Creature".to_string()],
                        colors: vec![],
                        keywords: vec![],
                        tapped: false,
                        count: QuantityExpr::Fixed { value: 1 },
                        owner: TargetFilter::Controller,
                        attach_to: None,
                        enters_attacking: false,
                        supertypes: vec![],
                        static_abilities: vec![],
                        enter_with_counters: vec![],
                    },
                    vec![],
                    SRC,
                    PlayerId(0),
                );
                frame
                    .stack
                    .push_back(announced_trigger_entry(ENTRY, SRC, ability, None));
            },
        );
        let frame = announced_from_retained_sample(&state, ENTRY);
        let entry = frame.stack.back().expect("the announced entry").clone();

        // ── the two derivations, each on its own budget so neither starves the other ──
        let mut on_frame_budget = ProbeBudget::for_test(1_000);
        let mut on_live_budget = ProbeBudget::for_test(1_000);
        let from_sample =
            stack_entry_resolution_choice_freedom(frame, &entry, &mut on_frame_budget);
        let from_live = stack_entry_resolution_choice_freedom(&state, &entry, &mut on_live_budget);

        // ── REACH-GUARD: both sides must be non-empty `FreeUnlessReplacements`, so the
        //    equality below cannot be two refusals or two empties matching ──
        let derived = |freedom: &ResolutionChoiceFreedom| -> Vec<ProposedEvent> {
            match freedom {
                ResolutionChoiceFreedom::FreeUnlessReplacements(events) => events.clone(),
                ResolutionChoiceFreedom::MayPrompt => panic!(
                    "REACH-GUARD: an `Effect::Token` announcement must classify as a derived \
                     event set on BOTH boards; a refusal here means the fixture never reached \
                     the derivation and the equality would be vacuous"
                ),
            }
        };
        let sample_events = derived(&from_sample);
        let live_events = derived(&from_live);
        assert!(
            !sample_events.is_empty() && !live_events.is_empty(),
            "REACH-GUARD: `probe_resolution` classifies an EMPTY derivation `Prompted`, so a \
             non-empty set is what proves the resolution really ran"
        );

        // ── (a3): the retained sample's derivation IS the live board's ──
        assert_eq!(
            from_sample, from_live,
            "(a3) CR 732.2a: an announcement carried by a retained sample must classify exactly \
             as the same entry classified against the live board. Sequence equality is STRICTER \
             than the set equality the row claims, which is the safe direction — both sides come \
             from one deterministic `resolve_ability_chain`"
        );

        // ── INSTRUMENT-LIVENESS CONTROL: the OTHER half of the same sample, one field apart ──
        let normalized_half = &state.loop_detect_ring[state.loop_detect_ring.len() - 1].normalized;
        let normalized_entry = normalized_half
            .stack
            .back()
            .expect("normalization preserves every `StackEntry.id` (R17 arm 3)")
            .clone();
        let mut control_budget = ProbeBudget::for_test(1_000);
        let from_normalized = stack_entry_resolution_choice_freedom(
            normalized_half,
            &normalized_entry,
            &mut control_budget,
        );
        let token_source = |freedom: &ResolutionChoiceFreedom| -> Option<ObjectId> {
            match freedom {
                ResolutionChoiceFreedom::FreeUnlessReplacements(events) => {
                    events.iter().find_map(|event| match event {
                        ProposedEvent::CreateToken { spec, .. } => Some(spec.source_id),
                        _ => None,
                    })
                }
                ResolutionChoiceFreedom::MayPrompt => None,
            }
        };
        assert_eq!(
            (token_source(&from_sample), token_source(&from_normalized)),
            (Some(SRC), Some(ObjectId(0))),
            "(a3) CONTROL — CR 400.7 + CR 111.1: `normalize_for_loop` runs \
             `clear_trigger_identity_recursive`, which zeroes `ability.source_id`, and the \
             resolver carries that into `TokenSpec.source_id`. The normalized half therefore \
             proposes a token whose source is the NULL object. Without this arm the equality \
             above would be true of any two boards and would prove nothing"
        );
        assert_ne!(
            from_sample, from_normalized,
            "(a3) CONTROL: and the two halves' derivations must genuinely DIFFER, so the \
             carrier is a choice with a consequence rather than a label"
        );

        // ── the seam companion: conjunct (6) really consumed THIS pair on the real mint ──
        let (outcome, meter) =
            try_offer_bounded_cycle_shortcut_metered(&state, false, ProbeCap::Shipped);
        assert!(
            outcome.is_ok() && meter.conjunct6_asks == 1,
            "(a3) the board the equality is asserted on must be one the SEAM evaluates: the \
             mint offers and asks conjunct (6) exactly once — about the announced pair, since \
             `current.stack` is empty. Got {outcome:?}, meter {meter:?}"
        );
    }

    /// R27 (b) — THE ENTRY-IDENTITY AXIS: A STORED CR 603.5 AUTO-CHOICE STILL REFUSES THE MINT
    /// WHEN THE PAIR ARRIVES FROM THE RING.
    ///
    /// CR 603.5 + CR 732.2a. R25 pinned the mint's second-authority conjunct on a board whose
    /// entry sits on `current.stack`; this is its missing production twin — the same board
    /// driven THROUGH the ring, so the mint is asked about a retained sample. The refusal has
    /// to survive that, because the announced population is where the mint's domain actually
    /// lives (`bounded_cycle_pin_slots_for_window` maps over `touch.announced`).
    ///
    /// MATCHED PAIR, differing ONLY in the seeded record, so no upstream conjunct can dominate:
    /// without it the board OFFERS and publishes exactly one CR 603.5 `MayChoice` point; with
    /// it the mint publishes nothing, the relief has no `may` to spend, and step (6) refuses
    /// `UnspecifiedChoiceWindow`.
    ///
    /// REVERT-PROBE (the plan's shared carrier revert, and it FLIPS — measured): point
    /// `game::engine::bounded_cycle_offer`'s `ring_live` at `&f.normalized` ⇒ the carrying
    /// frame becomes a comparand whose `ability.source_id` is `ObjectId(0)`
    /// (`clear_trigger_identity_recursive`) ⇒ the `MayTriggerAutoChoiceKey` misses ⇒ the `may`
    /// slot IS minted ⇒ the negative arm OFFERS. This is the flip round 7 could not obtain:
    /// a row that builds its own window is blind to the mint's carrier, a row driven through
    /// the mint is not.
    #[test]
    fn r27_b_a_stored_may_auto_choice_survives_the_ring() {
        use crate::game::engine::{
            try_offer_bounded_cycle_shortcut_metered, BoundedOfferRefusal, ProbeCap,
        };
        use crate::types::ability::{
            Effect, QuantityExpr, ResolvedAbility, TargetFilter, TriggerBaseSetInstanceRef,
            TriggerDefinitionOccurrenceRef, TriggerDefinitionRef,
        };
        use crate::types::game_state::{AutoMayChoice, MayTriggerAutoChoiceKey, MayTriggerOrigin};
        use crate::types::identifiers::ObjectIncarnationRef;

        const SRC: ObjectId = ObjectId(940);
        const ENTRY: u64 = 952;
        // The production shape: `triggers.rs` mints `Definition { definition_ref }` from the
        // source's own incarnation plus the printed occurrence — built here identically.
        let origin = MayTriggerOrigin::Definition {
            definition_ref: TriggerDefinitionRef {
                source: ObjectIncarnationRef::of(SRC, 3),
                occurrence: TriggerDefinitionOccurrenceRef::Printed {
                    base_set: TriggerBaseSetInstanceRef::INITIAL,
                    printed_index: 0,
                },
            },
        };
        let key = |origin: MayTriggerOrigin| MayTriggerAutoChoiceKey {
            player: PlayerId(0),
            source_id: SRC,
            origin,
        };
        let announce = |origin: MayTriggerOrigin| {
            move |frame: &mut GameState| {
                // Shape (B): OPTIONAL, declaring no target, so the mint publishes its CR 603.5
                // gate alone and the relief's residual is the same draw with `optional` cleared.
                let mut ability = ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                    vec![],
                    SRC,
                    PlayerId(0),
                );
                ability.optional = true;
                ability.may_trigger_origin = Some(origin.clone());
                frame
                    .stack
                    .push_back(announced_trigger_entry(ENTRY, SRC, ability, None));
            }
        };

        // ── MATCHED POSITIVE: no stored record ⇒ the CR 603.5 gate really asks ⇒ pin spendable
        let open = ring_announcing_on_its_newest_sample(
            |s| {
                announcing_ring_source(s, SRC.0);
            },
            announce(origin.clone()),
        );
        announced_from_retained_sample(&open, ENTRY);
        let (open_outcome, open_meter) =
            try_offer_bounded_cycle_shortcut_metered(&open, false, ProbeCap::Shipped);
        let published_may_points = match &open_outcome {
            Ok(crate::types::game_state::WaitingFor::LoopShortcut { schema, .. }) => schema
                .points
                .iter()
                .filter(|p| {
                    matches!(
                        p.kind,
                        crate::analysis::decision_template::DecisionPointKind::MayChoice
                    )
                })
                .count(),
            other => panic!(
                "MATCHED POSITIVE: the un-seeded board must OFFER, else the negative below is \
                 a dominated refusal. Got {other:?}, meter {open_meter:?}"
            ),
        };
        assert_eq!(
            published_may_points, 1,
            "CR 603.5: the announced pair publishes exactly ONE `MayChoice` point, so the \
             negative's refusal is the loss of THAT point and not of an unrelated slot"
        );

        // ── NEGATIVE: the same board with one record seeded before the frames are snapshotted
        let seeded_origin = origin.clone();
        let sealed = ring_announcing_on_its_newest_sample(
            move |s| {
                announcing_ring_source(s, SRC.0);
                s.set_may_trigger_auto_choice(key(seeded_origin.clone()), AutoMayChoice::Decline);
            },
            announce(origin.clone()),
        );
        let sealed_frame = announced_from_retained_sample(&sealed, ENTRY);
        assert_eq!(
            sealed_frame.may_trigger_auto_choice(&key(origin.clone())),
            Some(AutoMayChoice::Decline),
            "REACH-GUARD: the record must be readable ON THE CARRYING FRAME under the key the \
             mint builds. Seeding it only on `current` would make the negative pass for the \
             wrong reason — and would be exactly the defect this row exists to catch"
        );
        let (sealed_outcome, sealed_meter) =
            try_offer_bounded_cycle_shortcut_metered(&sealed, false, ProbeCap::Shipped);
        assert_eq!(
            sealed_outcome,
            Err(BoundedOfferRefusal::UnspecifiedChoiceWindow),
            "(b) CR 603.5: a stored 'don't ask again' answer is a SECOND authority on the same \
             gate, and the gate returns before setting any prompt — so a pin minted for it \
             would be silently unused. The refusal must survive the pair arriving from a \
             retained sample. meter {sealed_meter:?}"
        );
        assert_eq!(
            (sealed_meter.conjunct6_asks, sealed_meter.certification),
            (1, Some(PeriodCertification::BoardEqualOnly)),
            "(b) ATTRIBUTION: the refusal is step (6)'s on the announced pair — certification \
             matched and conjunct (6) ran exactly once — not an earlier conjunct's"
        );
    }

    // ───────────────────────────────────────────────────────────────────────────────────
    // F2 / N0 / A5 — one CR 603.5 authority, its two consumers, and the stored answer.
    // ───────────────────────────────────────────────────────────────────────────────────

    /// A5 / N0 — **A STORED `Accept` RELIEVES GATE (6). A STORED `Decline` DOES NOT.**
    ///
    /// CR 603.5 + CR 732.2a. The matched pair `r27_b` is one field short of: same board, same
    /// key, ONE value different. It is the whole content of the auto-choice relief basis, and
    /// it is the arm the user's own MODE1 board rides on — that capture carries a stored
    /// "always take", so guard (b) withholds the pin slot and the ONLY thing that can specify
    /// the window is this relief.
    ///
    /// | stored | pin published | gate (6) | offer |
    /// |---|---|---|---|
    /// | `Accept` | NO (guard (b) withholds it) | relieved by the AUTO basis | **OFFERS** |
    /// | `Decline` | NO (same withholding) | not relieved | **`UnspecifiedChoiceWindow`** |
    ///
    /// THE ASYMMETRY IS NOT CAUTION, it is what the residual MEANS.
    /// `optional_cleared_classification` re-classifies the ability as if it RESOLVED with its
    /// gate discharged, which is what a stored `Accept` produces. A stored `Decline` is equally
    /// prompt-free but produces the OPPOSITE board, so relieving it would hand the certificate
    /// a claim about events the shortcut never proposes.
    ///
    /// The `Accept` arm is also the anti-vacuity control for the `Decline` arm: without it
    /// "Decline refuses" is indistinguishable from a relief that never fires at all.
    ///
    /// REVERT-PROBE: delete the `auto_may_choice_relief` disjunct from gate (6) ⇒ the `Accept`
    /// arm stops offering ⇒ FLIPS. TRIVIALIZE-PROBE: relieve on ANY stored answer (drop the
    /// `matches!(.., Accept)` conjunct) ⇒ the `Decline` arm starts offering ⇒ FLIPS.
    #[test]
    fn a5_a_stored_accept_relieves_gate_six_and_a_stored_decline_does_not() {
        use crate::game::engine::{
            entry_publishes_pin_slots, try_offer_bounded_cycle_shortcut_metered,
            BoundedOfferRefusal, ProbeCap,
        };
        use crate::types::ability::{
            Effect, QuantityExpr, ResolvedAbility, TargetFilter, TriggerBaseSetInstanceRef,
            TriggerDefinitionOccurrenceRef, TriggerDefinitionRef,
        };
        use crate::types::game_state::{AutoMayChoice, MayTriggerAutoChoiceKey, MayTriggerOrigin};
        use crate::types::identifiers::ObjectIncarnationRef;

        const SRC: ObjectId = ObjectId(940);
        const ENTRY: u64 = 954;
        let origin = MayTriggerOrigin::Definition {
            definition_ref: TriggerDefinitionRef {
                source: ObjectIncarnationRef::of(SRC, 3),
                occurrence: TriggerDefinitionOccurrenceRef::Printed {
                    base_set: TriggerBaseSetInstanceRef::INITIAL,
                    printed_index: 0,
                },
            },
        };
        let key = MayTriggerAutoChoiceKey {
            player: PlayerId(0),
            source_id: SRC,
            origin: origin.clone(),
        };
        let announce = {
            let origin = origin.clone();
            move |frame: &mut GameState| {
                let mut ability = ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                    vec![],
                    SRC,
                    PlayerId(0),
                );
                ability.optional = true;
                ability.may_trigger_origin = Some(origin.clone());
                frame
                    .stack
                    .push_back(announced_trigger_entry(ENTRY, SRC, ability, None));
            }
        };
        let board = |stored: AutoMayChoice| {
            let key = key.clone();
            ring_announcing_on_its_newest_sample(
                move |s| {
                    announcing_ring_source(s, SRC.0);
                    s.set_may_trigger_auto_choice(key.clone(), stored);
                },
                announce.clone(),
            )
        };

        for stored in [AutoMayChoice::Accept, AutoMayChoice::Decline] {
            let state = board(stored);
            let frame = announced_from_retained_sample(&state, ENTRY);
            // REACH-GUARD, on BOTH arms: the record is readable on the CARRYING frame under
            // the key the authority builds, and guard (b) therefore withholds the pin. Without
            // this the `Accept` arm could be offering through the ORDINARY pin basis, which
            // would make the whole row about something else.
            assert_eq!(
                frame.may_trigger_auto_choice(&key),
                Some(stored),
                "REACH-GUARD [{stored:?}]: the stored answer must be readable on the carrying \
                 frame, not only on `current`"
            );
            let entry = frame.stack.back().expect("the announced entry").clone();
            assert!(
                entry_publishes_pin_slots(frame, &entry, PlayerId(0))
                    .is_none_or(|slots| slots.may.is_none()),
                "REACH-GUARD [{stored:?}]: guard (b) must WITHHOLD the `MayChoice` slot for an \
                 already-answered gate, so the auto basis is the only thing that can specify \
                 this window"
            );

            let (outcome, meter) =
                try_offer_bounded_cycle_shortcut_metered(&state, false, ProbeCap::Shipped);
            match stored {
                AutoMayChoice::Accept => assert!(
                    outcome.is_ok(),
                    "CR 603.5: a stored `Accept` makes the per-iteration window the MOST \
                     determined a choice can be — it never opens — so gate (6) is specified \
                     and the offer stands. got {outcome:?}, meter {meter:?}"
                ),
                AutoMayChoice::Decline => assert_eq!(
                    outcome,
                    Err(BoundedOfferRefusal::UnspecifiedChoiceWindow),
                    "CR 732.2a: a stored `Decline` is equally prompt-free but produces the \
                     OPPOSITE board, so the optional-cleared residual would describe events \
                     the shortcut never proposes. Fail closed. meter {meter:?}"
                ),
            }
        }
    }

    /// F2a — **ONE AUTHORITY, AND THE TWO ABILITY SHAPES THE THIRD COPY GOT WRONG.**
    ///
    /// CR 603.5 + CR 608.2d + CR 101.4. Before adoption, three places answered *"does this
    /// ability open one up-front optional gate?"*: production's own branch in
    /// `resolve_chain_body`, the mint's guard (b), and this module's `auto_may_answer_for`.
    /// The latter two asked the same four predicates and OMITTED two conjuncts production has
    /// — `optional_for` and the CR 608.2d feasibility probe — so on two ability shapes they
    /// called a may "already answered" where production never reads the store at all.
    ///
    /// This row is that divergence, asserted at the authority. Every arm seeds a stored
    /// `Accept` under exactly the key the old copy would have built, so an omitted conjunct
    /// shows up as a WRONG ANSWER rather than as an absent one.
    ///
    /// | arm | one field different | gate | `stored_may_answer` |
    /// |---|---|---|---|
    /// | (P) plain optional | — | `Some` | `Some(Accept)` |
    /// | (O) `optional_for: AnyOpponent` | CR 608.2d fan-out | `None` | **`None`** |
    /// | (I) infeasible `RemoveCounter` | zero matching counters | `None` | **`None`** |
    /// | (I-pos) the SAME `RemoveCounter`, feasible | one counter on the source | `Some` | `Some(Accept)` |
    ///
    /// (I-pos) is what keys (I) to FEASIBILITY rather than to the effect discriminant: the two
    /// boards differ only in whether the source carries a `+1/+1` counter. (P) is the paired
    /// positive for (O).
    ///
    /// It is also the row that EXERCISES `OptionalFeasibility::Probe` on both of its outcomes
    /// — `stored_may_answer` passes `Probe`, so (I) and (I-pos) run the real probe and take
    /// opposite branches. Without them the variant would be constructed but never decisive.
    ///
    /// REVERT-PROBE: delete `optional_for.is_some() ⇒ None` from the authority ⇒ (O) FLIPS.
    /// Delete the feasibility conjunct ⇒ (I) FLIPS. Neither touches (P) or (I-pos).
    #[test]
    fn f2a_the_upfront_gate_authority_answers_the_two_shapes_the_third_copy_omitted() {
        use crate::game::effects::{stored_may_answer, upfront_optional_gate, OptionalFeasibility};
        use crate::types::ability::{
            Effect, OpponentMayScope, QuantityExpr, ResolvedAbility, TargetFilter,
            TriggerBaseSetInstanceRef, TriggerDefinitionOccurrenceRef, TriggerDefinitionRef,
        };
        use crate::types::counter::CounterType;
        use crate::types::game_state::{AutoMayChoice, MayTriggerAutoChoiceKey, MayTriggerOrigin};
        use crate::types::identifiers::ObjectIncarnationRef;

        let src = ObjectId(CHURN_SRC);
        let origin = MayTriggerOrigin::Definition {
            definition_ref: TriggerDefinitionRef {
                source: ObjectIncarnationRef::of(src, 0),
                occurrence: TriggerDefinitionOccurrenceRef::Printed {
                    base_set: TriggerBaseSetInstanceRef::INITIAL,
                    printed_index: 0,
                },
            },
        };
        // The key the OLD copy built: `optional_prompt_player` (P0 here) + source + origin.
        // Seeded on every arm, so an omitted conjunct is a wrong answer and not a missing one.
        let key = MayTriggerAutoChoiceKey {
            player: PlayerId(0),
            source_id: src,
            origin: origin.clone(),
        };
        let board = |counters: u32| {
            let mut state = drain_state(4);
            state.set_may_trigger_auto_choice(key.clone(), AutoMayChoice::Accept);
            if counters > 0 {
                state
                    .objects
                    .get_mut(&src)
                    .expect("drain_state seats the churn source")
                    .counters
                    .insert(CounterType::Plus1Plus1, counters);
            }
            state
        };
        let optional_ability = |effect: Effect| {
            let mut ability = ResolvedAbility::new(effect, vec![], src, PlayerId(0));
            ability.optional = true;
            ability.may_trigger_origin = Some(origin.clone());
            ability
        };
        let draw = || {
            optional_ability(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            })
        };
        let remove_counter = || {
            // `SelfRef` is CR 608.2c's printed-name anaphor: it resolves to the source
            // object, so the feasibility probe reads THAT object's counters and the two
            // boards below differ in exactly one field.
            optional_ability(Effect::RemoveCounter {
                counter_type: Some(CounterType::Plus1Plus1),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            })
        };

        let plain = board(0);
        assert!(
            upfront_optional_gate(&plain, &draw(), OptionalFeasibility::Probe).is_some(),
            "(P) MATCHED POSITIVE, asserted first: a plain optional ability DOES open one \
             up-front gate. Without it every `None` below could be a broken authority"
        );
        assert_eq!(
            stored_may_answer(&plain, &draw()),
            Some(AutoMayChoice::Accept),
            "(P) …and the stored preference answers it"
        );

        let mut fanned = draw();
        fanned.optional_for = Some(OpponentMayScope::AnyOpponent);
        assert!(
            upfront_optional_gate(&plain, &fanned, OptionalFeasibility::Probe).is_none(),
            "(O) CR 608.2d + CR 101.4: an `optional_for` ability opens an APNAP CASCADE of up \
             to one window PER LIVING PLAYER, not one up-front gate — production returns at \
             the fan-out before the gate is reached at all"
        );
        assert_eq!(
            stored_may_answer(&plain, &fanned),
            None,
            "(O) THE DIVERGENCE: the store holds an `Accept` under exactly the key the third \
             copy built, and the answer is still `None`, because the gate that preference \
             would answer never opens"
        );

        assert!(
            upfront_optional_gate(&plain, &remove_counter(), OptionalFeasibility::Probe).is_none(),
            "(I) CR 608.2d: \"a player can't choose an impossible option\" — with zero matching \
             counters the optional removal opens no window"
        );
        assert_eq!(
            stored_may_answer(&plain, &remove_counter()),
            None,
            "(I) THE SECOND DIVERGENCE, on the same seeded store"
        );

        let stocked = board(1);
        assert!(
            upfront_optional_gate(&stocked, &remove_counter(), OptionalFeasibility::Probe)
                .is_some(),
            "(I-pos) THE FEASIBILITY CONTROL: the SAME ability on a board one counter \
             different DOES open its gate. This is what keys (I) to feasibility rather than \
             to the effect discriminant — and it is the arm that proves \
             `OptionalFeasibility::Probe` reaches a decision, not merely a construction"
        );
        assert_eq!(
            stored_may_answer(&stocked, &remove_counter()),
            Some(AutoMayChoice::Accept),
            "(I-pos) …and the same stored preference now answers it"
        );

        // `Known` is the mode production uses, and it must OVERRIDE the probe rather than
        // re-run it — otherwise adoption A would pay the clone twice on every resolve.
        assert!(
            upfront_optional_gate(
                &stocked,
                &remove_counter(),
                OptionalFeasibility::Known(true)
            )
            .is_none(),
            "`Known(true)` suppresses the gate on a board the probe would call FEASIBLE, so \
             the caller's already-computed answer is what is used"
        );
        assert!(
            upfront_optional_gate(&plain, &remove_counter(), OptionalFeasibility::Known(false))
                .is_some(),
            "…and `Known(false)` admits it on a board the probe would call INFEASIBLE. The \
             pair proves the probe is not re-run under `Known`"
        );
    }

    /// F2b — **GUARD (b) WITHHOLDS A PIN THE CR 603.5 GATE CAN NEVER SPEND.**
    ///
    /// CR 732.2a + CR 608.2d. The mint publishes a `MayChoice` slot so a declaration can pin
    /// the ONE up-front window an entry opens. Before adoption it minted that slot for two
    /// shapes that open no such window: an `optional_for` fan-out (an APNAP cascade of up to
    /// one window per living player — one slot standing for N prompts is exactly the
    /// cardinality defect group (c) already argues against) and an infeasible optional (a pin
    /// the gate can never spend, invisible even to a fail-closed inject arm).
    ///
    /// THE FIXTURE IS UNSEEDED ON PURPOSE. Every arm carries `may_trigger_origin: None`, so
    /// guard (b)'s store conjunct is vacuously true on both the old predicate and the new one
    /// and the ONLY thing that can move `may` is `optional_for` / feasibility. A SEEDED variant
    /// is explicitly rejected: a stored answer makes the store conjunct false on every arm,
    /// `may` is `None` for the stored-answer reason throughout, and the axis under test cannot
    /// move at all.
    ///
    /// | arm | one field different | published `may` |
    /// |---|---|---|
    /// | (P) plain optional drain | — | **`Some`** |
    /// | (O) `optional_for: AnyOpponent` | CR 608.2d fan-out | **`None`** |
    /// | (I) infeasible optional `RemoveCounter` | zero matching counters | **`None`** |
    /// | (I-pos) the SAME entry, feasible | one counter on the source | **`Some`** |
    ///
    /// Direction: strictly FEWER offers, never more.
    ///
    /// REVERT-PROBE: delete `optional_for.is_some() ⇒ None` from the authority ⇒ (O) publishes
    /// ⇒ FLIPS. Delete the feasibility conjunct ⇒ (I) publishes ⇒ FLIPS. (P) and (I-pos) are
    /// the paired positives that keep both negatives out of "the mint publishes nothing".
    #[test]
    fn f2b_guard_b_withholds_a_pin_the_cr_603_5_gate_can_never_spend() {
        use crate::game::engine::entry_publishes_pin_slots;
        use crate::types::ability::{
            Effect, OpponentMayScope, QuantityExpr, ResolvedAbility, TargetFilter,
        };
        use crate::types::counter::CounterType;

        let src = ObjectId(CHURN_SRC);
        let board = |counters: u32| {
            let mut state = drain_state(4);
            if counters > 0 {
                state
                    .objects
                    .get_mut(&src)
                    .expect("drain_state seats the churn source")
                    .counters
                    .insert(CounterType::Plus1Plus1, counters);
            }
            state
        };
        let published_may = |state: &GameState, entry: &StackEntry| -> bool {
            // REACH-GUARD baked into the reader: the entry must carry NO stored preference, so
            // guard (b)'s store conjunct cannot be what moves the answer.
            assert!(
                entry
                    .ability()
                    .is_some_and(|a| a.may_trigger_origin.is_none()),
                "UNSEEDED FIXTURE: an arm with a `may_trigger_origin` could be answered by the \
                 store conjunct and the axis under test would be dominated"
            );
            entry_publishes_pin_slots(state, entry, PlayerId(0))
                .is_some_and(|slots| slots.may.is_some())
        };

        let plain = board(0);
        let p_entry = optional_drain(20);
        assert!(
            published_may(&plain, &p_entry),
            "(P) MATCHED POSITIVE, asserted first: a plain optional drain publishes its \
             CR 603.5 gate. Without it every withholding below is indistinguishable from a \
             mint that publishes nothing"
        );

        let o_entry = {
            let mut ability = p_entry
                .ability()
                .expect("the drain is a triggered ability")
                .clone();
            ability.optional_for = Some(OpponentMayScope::AnyOpponent);
            churn_entry(21, 0, ability, None)
        };
        assert!(
            !published_may(&plain, &o_entry),
            "(O) CR 608.2d + CR 101.4 + CR 732.2a: a fan-out `may` is not ONE window — it is \
             an APNAP cascade of up to one window per living player, and a shortcut must \
             describe THE sequence of choices. One published slot cannot stand for N prompts"
        );

        let remove_counter_entry = |id: u64| {
            // Shape (B), may-only: no declared target, so `build_target_slots` surfaces
            // nothing and the entry publishes its CR 603.5 gate alone.
            let mut ability = ResolvedAbility::new(
                Effect::RemoveCounter {
                    // CR 608.2c `SelfRef`: the probe reads the SOURCE's counters, which is
                    // the one field (I) and (I-pos) differ in.
                    counter_type: Some(CounterType::Plus1Plus1),
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                },
                vec![],
                src,
                PlayerId(0),
            );
            ability.optional = true;
            churn_entry(id, 0, ability, None)
        };
        assert!(
            !published_may(&plain, &remove_counter_entry(22)),
            "(I) CR 608.2d: an infeasible optional opens no window at all, so a slot minted \
             for it is a pin the gate can never spend — invisible even to a fail-closed \
             inject arm, which is why the mint has to refuse it here"
        );
        assert!(
            published_may(&board(1), &remove_counter_entry(23)),
            "(I-pos) THE FEASIBILITY CONTROL: the byte-identical entry on a board one counter \
             different DOES publish. (I) is therefore about feasibility and not about the \
             effect discriminant or the shape-(B) route"
        );
    }

    /// R27 (c) — THE SCOPE-BINDING AXIS: A CR 603.4 INTERVENING-IF ON A RETAINED SAMPLE BINDS
    /// WITH ITS TRIGGER SOURCE.
    ///
    /// CR 603.4 + CR 732.2a. `bind_resolution_scope` rechecks the intervening-if as the
    /// ability resolves, reading `ability.trigger_source`; `normalize_for_loop` sets that to
    /// `None`. A `TriggerCondition::SourceEnteredThisTurn` is TRUE only when the context is
    /// present, so classifying such an entry against a normalized carrier takes the
    /// absent-context path, the recheck fails, and `stack_entry_resolution_choice_freedom`
    /// returns `MayPrompt` — a mandatory entry the mint publishes no `may` for, hence a
    /// refusal the live board would never make.
    ///
    /// THE MATCHED PAIR IS THE CONTEXT ITSELF, byte-identical otherwise, so the offer's
    /// existence is attributable to `trigger_source` and to nothing else on the board. The
    /// plan's "must classify identically to the same entry classified against `current`" ships
    /// as its own conjunct alongside.
    ///
    /// REVERT-PROBE (the shared carrier revert, and it FLIPS — measured): point
    /// `bounded_cycle_offer`'s `ring_live` at `&f.normalized` ⇒ the POSITIVE arm stops
    /// offering and returns `UnspecifiedChoiceWindow`.
    #[test]
    fn r27_c_an_intervening_if_binds_with_the_retained_samples_trigger_source() {
        use crate::game::engine::{
            try_offer_bounded_cycle_shortcut_metered, BoundedOfferRefusal, ProbeCap,
        };
        use crate::game::resolution_prompt::ResolutionChoiceFreedom;
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TriggerCondition};

        const SRC: ObjectId = ObjectId(940);
        const ENTRY: u64 = 953;
        let announce = |with_context: bool| {
            move |frame: &mut GameState| {
                // MANDATORY: an optional entry would be relieved through the CR 603.5 pin and
                // the refusal below would be about the wrong gate.
                let mut ability = ResolvedAbility::new(
                    Effect::LoseLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        target: None,
                    },
                    vec![],
                    SRC,
                    PlayerId(0),
                );
                if with_context {
                    let source = frame.objects[&SRC].clone();
                    ability.trigger_source = Some(
                        crate::game::triggers::trigger_source_context_for_latch(frame, &source),
                    );
                }
                frame.stack.push_back(announced_trigger_entry(
                    ENTRY,
                    SRC,
                    ability,
                    Some(TriggerCondition::SourceEnteredThisTurn),
                ));
            }
        };

        // ── POSITIVE: the context is present, the CR 603.4 recheck passes, the mint offers ──
        let bound = ring_announcing_on_its_newest_sample(
            |s| {
                announcing_ring_source(s, SRC.0);
            },
            announce(true),
        );
        let bound_frame = announced_from_retained_sample(&bound, ENTRY);
        let entry = bound_frame
            .stack
            .back()
            .expect("the announced entry")
            .clone();

        // (c)'s own claim, at the predicate: the retained sample classifies the intervening-if
        // exactly as the live board would.
        let mut sample_budget = ProbeBudget::for_test(1_000);
        let mut live_budget = ProbeBudget::for_test(1_000);
        let from_sample =
            stack_entry_resolution_choice_freedom(bound_frame, &entry, &mut sample_budget);
        let from_live = stack_entry_resolution_choice_freedom(&bound, &entry, &mut live_budget);
        assert!(
            matches!(
                from_sample,
                ResolutionChoiceFreedom::FreeUnlessReplacements(_)
            ),
            "REACH-GUARD: the recheck must PASS on the carrying frame, else the equality below \
             is two refusals agreeing. Got {from_sample:?}"
        );
        assert_eq!(
            from_sample, from_live,
            "(c) CR 603.4: the intervening-if recheck reads `ability.trigger_source`, and a \
             retained sample carries it, so the pair classifies exactly as the live board does"
        );

        // INSTRUMENT-LIVENESS CONTROL: the same sample's NORMALIZED half, one field apart.
        let normalized_half = &bound.loop_detect_ring[bound.loop_detect_ring.len() - 1].normalized;
        let normalized_entry = normalized_half
            .stack
            .back()
            .expect("normalization preserves every `StackEntry.id`")
            .clone();
        let mut control_budget = ProbeBudget::for_test(1_000);
        assert_eq!(
            stack_entry_resolution_choice_freedom(
                normalized_half,
                &normalized_entry,
                &mut control_budget
            ),
            ResolutionChoiceFreedom::MayPrompt,
            "(c) CONTROL — CR 603.4 + CR 400.7: `clear_trigger_identity_recursive` sets \
             `trigger_source = None`, so `check_trigger_condition_with_source` takes the \
             absent-context path, `bind_resolution_scope` returns false and the classifier is \
             fail-closed `MayPrompt`. Without this the equality above would hold on any board"
        );

        let (bound_outcome, bound_meter) =
            try_offer_bounded_cycle_shortcut_metered(&bound, false, ProbeCap::Shipped);
        assert!(
            bound_outcome.is_ok(),
            "(c) MATCHED POSITIVE: with the context present the mint OFFERS, so the negative \
             below is attributable to the context. Got {bound_outcome:?}, {bound_meter:?}"
        );

        // ── NEGATIVE: byte-identical except that the entry carries no `TriggerSourceContext` ──
        let unbound = ring_announcing_on_its_newest_sample(
            |s| {
                announcing_ring_source(s, SRC.0);
            },
            announce(false),
        );
        announced_from_retained_sample(&unbound, ENTRY);
        let (unbound_outcome, unbound_meter) =
            try_offer_bounded_cycle_shortcut_metered(&unbound, false, ProbeCap::Shipped);
        assert_eq!(
            unbound_outcome,
            Err(BoundedOfferRefusal::UnspecifiedChoiceWindow),
            "(c) CR 603.4: with no trigger source the recheck cannot pass, the resolution scope \
             does not bind, and the fail-closed classifier refuses — which is exactly the \
             verdict a normalized carrier would force on the POSITIVE board. meter \
             {unbound_meter:?}"
        );
    }

    /// R27 (e) — THE CANDIDATE-AUTHORITY HALF IS FRAME-SENSITIVE TOO.
    ///
    /// CR 614.1 + CR 616.1 + CR 732.2a. (a3) pins the EVENT half of the discharge against the
    /// pair's carrying frame and says nothing about the half that CONSUMES it:
    /// `resolution_events_are_discharged` hands `proposed_event_prompt_cause` a board, and
    /// that board runs `find_applicable_replacements` over its OWN replacement population. A
    /// wrong board there checks one frame's events against another frame's candidates, and
    /// (a3) stays green while it does — both its sides are event sets.
    ///
    /// FOUR BOARDS, byte-identical except which one carries the definition and whether that
    /// definition draws a PROMPT CAUSE (CR 614.1a: a mandatory replacement's own body is
    /// stashed as a continuation and drained through an arbitrary `ResolvedAbility`, which can
    /// set a non-priority `waiting_for` — the cause this fixture uses. CR 616.1's ORDERING
    /// cause needs two competing candidates and is a different shape):
    /// * **frame only** — the constructible direction (the sample holds the permanent, the
    ///   live board no longer does) ⇒ conjunct (6) REFUSES.
    /// * **both** ⇒ also refuses, so the arm is not passing because the def is unreachable.
    /// * **neither** ⇒ certifies, the reach-guard proving the fixture reaches the discharge.
    /// * **frame, but causeless** — the same permanent, a MANDATORY definition with no body,
    ///   which is drawn as a candidate and yields NO cause ⇒ certifies. This is the control
    ///   that keeps the first arm from being "an extra object on the frame refuses".
    ///
    /// THE DEFINITION SHAPE IS MEASURED, NOT CHOSEN. An OPTIONAL draw replacement makes
    /// `probe_resolution` itself prompt (the resolution raises a choice), so the entry is
    /// already `MayPrompt` at the PRIMARY classification and the discharge is never reached —
    /// the refusal would be real but would key on the wrong seam. A MANDATORY definition with
    /// a `runtime_execute` body classifies as a derived event set and still yields
    /// `ReplacementPromptCause::MandatoryBodyContinuation`, which is the shape that reaches
    /// the CR 614.1 + CR 616.1 discharge tail.
    ///
    /// REVERT-PROBE (the plan's own, and it FLIPS — measured): pass the live `state` instead
    /// of `frame` to `resolution_events_are_discharged` in `stack_choices_are_all_specified`
    /// ⇒ the frame-only definition is invisible to `find_applicable_replacements` ⇒ the pair
    /// CERTIFIES and the first arm FLIPS TO FAIL, while the both-boards and neither-board arms
    /// stay green — so the pair discriminates the BOARD ARGUMENT and not the fixture.
    #[test]
    fn r27_e_the_discharge_reads_the_pairs_carrying_frame_not_the_live_board() {
        use crate::game::engine::{
            try_offer_bounded_cycle_shortcut_metered, BoundedOfferRefusal, ProbeCap,
        };
        use crate::types::ability::{
            DrawReplacementScope, Effect, QuantityExpr, ReplacementDefinition, ResolvedAbility,
            TargetFilter,
        };
        use crate::types::replacements::ReplacementEvent;

        const SRC: ObjectId = ObjectId(940);
        const DEF_SRC: ObjectId = ObjectId(941);
        const ENTRY: u64 = 955;

        // CR 614.1 scopes a definition to its controller's events, so the definition sits on a
        // P0-controlled permanent and replaces P0's own draw.
        let install = |with_body: bool| {
            move |board: &mut GameState| {
                let mut definition = ReplacementDefinition::new(ReplacementEvent::Draw);
                // CR 121.2: a Draw definition must declare which stage it watches; the
                // pipeline debug-asserts on one that declares neither.
                definition.draw_scope = Some(DrawReplacementScope::IndividualDraw);
                if with_body {
                    definition.runtime_execute = Some(Box::new(ResolvedAbility::new(
                        Effect::LoseLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            target: None,
                        },
                        vec![],
                        DEF_SRC,
                        PlayerId(0),
                    )));
                }
                let mut object = GameObject::new(
                    DEF_SRC,
                    CardId(78),
                    PlayerId(0),
                    "Frame-Only Watcher".to_string(),
                    Zone::Battlefield,
                );
                object.replacement_definitions.push(definition);
                board.objects.insert(DEF_SRC, object);
                board.battlefield.push_back(DEF_SRC);
            }
        };
        let announce = |board: &mut GameState| {
            let ability = ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                SRC,
                PlayerId(0),
            );
            board
                .stack
                .push_back(announced_trigger_entry(ENTRY, SRC, ability, None));
        };
        let seed_source = |s: &mut GameState| {
            announcing_ring_source(s, SRC.0);
        };

        let neither = ring_announcing_on_its_newest_sample(seed_source, announce);
        let frame_only = ring_announcing_on_its_newest_sample(seed_source, |board| {
            install(true)(board);
            announce(board);
        });
        let both = ring_announcing_on_its_newest_sample(
            |s| {
                seed_source(s);
                install(true)(s);
            },
            announce,
        );
        let causeless = ring_announcing_on_its_newest_sample(seed_source, |board| {
            install(false)(board);
            announce(board);
        });

        // ── the structural conjunct: ONE board carries both halves of the discharge ──
        let frame = announced_from_retained_sample(&frame_only, ENTRY);
        let verdicts = PeriodVerdicts::for_period(
            &frame_only
                .loop_detect_ring
                .iter()
                .map(|f| &f.live)
                .collect::<Vec<_>>(),
            &frame_only,
            PlayerId(0),
        );
        let carried = verdicts
            .frame_ix(frame)
            .expect("the carrying frame is one this container holds");
        let live_ix = verdicts
            .frame_ix(&frame_only)
            .expect("`current` is the container's last frame");
        assert_ne!(
            carried, live_ix,
            "(e) STRUCTURAL: `PeriodVerdicts::frame_ix` mints by POINTER IDENTITY, so the pair's \
             board and the live board must resolve to DIFFERENT indices. A future refactor that \
             re-derived the discharge board from `current` would collapse them here rather than \
             silently cross-check one frame's events against another frame's candidates"
        );

        // ── REACH-GUARD: without a definition anywhere the pair certifies ──
        let (neither_outcome, neither_meter) =
            try_offer_bounded_cycle_shortcut_metered(&neither, false, ProbeCap::Shipped);
        assert!(
            neither_outcome.is_ok() && neither_meter.conjunct6_asks == 1,
            "(e) REACH-GUARD: the fixture must reach and PASS the CR 616.1 tail when nothing \
             competes, else the three refusals below prove nothing about the board argument. \
             Got {neither_outcome:?}, meter {neither_meter:?}"
        );

        // ── the CONTROL: the same permanent on the frame, drawing a candidate with NO cause ──
        let (causeless_outcome, causeless_meter) =
            try_offer_bounded_cycle_shortcut_metered(&causeless, false, ProbeCap::Shipped);
        assert!(
            causeless_outcome.is_ok(),
            "(e) CONTROL: a MANDATORY, bodyless definition on the carrying frame is a candidate \
             with NO prompt cause at all and must still certify. This is what makes the \
             frame-only refusal attributable to the CAUSE rather than to the extra permanent. \
             Got \
             {causeless_outcome:?}, meter {causeless_meter:?}"
        );

        // ── the both-boards arm, ASSERTED FIRST so the stated revert-probe demonstrates in ONE
        //    run that this arm is unaffected while the frame-only arm below flips ──
        let (both_outcome, both_meter) =
            try_offer_bounded_cycle_shortcut_metered(&both, false, ProbeCap::Shipped);
        assert_eq!(
            both_outcome,
            Err(BoundedOfferRefusal::UnspecifiedChoiceWindow),
            "(e) MATCHED POSITIVE: with the definition on BOTH boards the refusal also holds, \
             so the frame-only arm below is keyed to WHICH board carries it and not to the \
             definition being invisible to the pipeline. meter {both_meter:?}"
        );

        // ── (e): the definition the CARRYING FRAME holds refuses, even though `current` has none
        let (frame_only_outcome, frame_only_meter) =
            try_offer_bounded_cycle_shortcut_metered(&frame_only, false, ProbeCap::Shipped);
        assert_eq!(
            frame_only_outcome,
            Err(BoundedOfferRefusal::UnspecifiedChoiceWindow),
            "(e) CR 614.1 + CR 616.1: the candidate authority runs over the board the events \
             were DERIVED on. A definition applicable on the pair's carrying frame and absent \
             from the live board must still refuse — handing the discharge `current` would \
             check one frame's events against another frame's candidates. meter \
             {frame_only_meter:?}"
        );
    }
}
