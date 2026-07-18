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

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::game::game_object::GameObject;
use crate::types::ability::{ActivationRestriction, DamageModification};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::CounterType;
use crate::types::game_state::{loop_states_equal, GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaType;
use crate::types::phase::Phase;
use crate::types::player::{Player, PlayerId};
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceVector {
    /// CR 106.1: floating mana by color, indexed `[W, U, B, R, G, C]` (see
    /// [`MANA_INDEX`]). Summed across all players' pools. **State-readable.**
    pub mana: [i64; 6],

    /// CR 119.1: per-player life total. **State-readable.**
    pub life: BTreeMap<PlayerId, i64>,

    /// CR 120.1: cumulative damage *dealt to* each player this analysis window.
    /// Damage is an event, not a stored total. **Event-fed** (left empty by
    /// `snapshot`).
    pub damage_dealt: BTreeMap<PlayerId, i64>,

    /// CR 401: per-player library size, as a signed delta-friendly count.
    /// Positive = larger library. Mill loops drive this negative.
    /// **State-readable** (absolute library size at snapshot time).
    pub library_delta: BTreeMap<PlayerId, i64>,

    /// CR 122.1 + CR 704.5c: poison counters keyed by VICTIM `PlayerId` (10 ⇒ that
    /// player loses). Per-victim so a multiplayer poison ∞ attributes the loss to the
    /// afflicted seat, not the loop's controller. **State-readable.**
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
    pub counters: BTreeMap<(CounterClass, ObjectClass), i64>,

    /// Generic trigger/keyword-action firings by family (proliferate, magecraft,
    /// …) — the mana-neutral axis a proliferate loop pumps. **Event-fed.**
    pub generic_triggers: BTreeMap<TriggerKind, i64>,
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

    /// The component axes that strictly increased over this delta — the
    /// candidate **unbounded** resources a `WinKind` classifier (PR-2) reads to
    /// name the loop's win condition. A mill axis surfaces here as a negative
    /// `library_delta`, so it is reported separately via its sign.
    ///
    /// Returns each increasing axis as a [`ResourceAxis`] tag with its signed
    /// magnitude.
    pub fn unbounded_components(&self) -> Vec<(ResourceAxis, i64)> {
        let mut out = Vec::new();
        for (i, &n) in self.mana.iter().enumerate() {
            if n > 0 {
                out.push((ResourceAxis::Mana(MANA_INDEX[i]), n));
            }
        }
        for (pid, &n) in &self.life {
            if n > 0 {
                out.push((ResourceAxis::Life(*pid), n));
            }
        }
        for (pid, &n) in &self.damage_dealt {
            if n > 0 {
                out.push((ResourceAxis::DamageDealt(*pid), n));
            }
        }
        // CR 401: a mill loop is unbounded *downward* on library size.
        for (pid, &n) in &self.library_delta {
            if n != 0 {
                out.push((ResourceAxis::LibraryDelta(*pid), n));
            }
        }
        // CR 704.5c: rising poison on a victim is an unbounded loss axis.
        for (pid, &n) in &self.poison {
            if n > 0 {
                out.push((ResourceAxis::Poison(*pid), n));
            }
        }
        for (&key, &n) in &self.counters {
            if n > 0 {
                out.push((ResourceAxis::Counter(key.0, key.1), n));
            }
        }
        for (&kind, &n) in &self.generic_triggers {
            if n > 0 {
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
            if n > 0 {
                out.push((axis, n));
            }
        }
        out
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
fn object_class(core_types: &[CoreType]) -> ObjectClass {
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
    // F1 (PR-7 Phase 4d-ii): `last_recast_context` is EXCLUDED from `impl PartialEq for
    // GameState` (`loop_states_equal` never compares it) and NOT cleared by
    // `project_out_resources`, so compare it explicitly here (fail-closed) — a heterogeneous
    // recast is caught, a homogeneous loop's invariant context compares equal. `None == None`
    // for every non-recast loop ⇒ zero regression to existing loop-equality tests.
    loop_states_equal(&pa, &pb)
        && loyalty_activation_counts_match(&pa, &pb)
        && pa.last_recast_context == pb.last_recast_context
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
    // (1) Board equal modulo the NARROWED projection AND modulo the stack, with the
    // object resource axes STRICT-COMPARED (R5-B1). Project both, clear both stacks
    // (the stack is compared separately in (2)), then require full board equality
    // plus loyalty-activation parity plus strict object damage/counter equality.
    let mut pa = project_out_resources(prior);
    let mut pb = project_out_resources(current);
    pa.stack.clear();
    pb.stack.clear();
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
    for (orig, norm) in current.stack.iter().zip(cur_stack.iter()) {
        let cn = cur_stack.iter().filter(|e| *e == norm).count();
        let pn = prior_stack.iter().filter(|e| *e == norm).count();
        if cn > pn && !stack_entry_has_no_ordering_input(current, orig) {
            return false;
        }
    }

    // (4) On-stack fail-closed resource-read guard: NO entry on `current`'s stack may
    // carry an AST that reads a still-projected axis (player monotone resources +
    // journals). Object-axis readers pass — their drift breaks gate (1) instead.
    if current
        .stack
        .iter()
        .any(stack_entry_reads_projected_resource)
    {
        return false;
    }

    // (5) Off-stack fail-closed fire-time condition guard (the second read surface).
    if fire_time_conditions_read_projected_resource(current) {
        return false;
    }

    // (6) CR 732.2a + CR 608.2d: resolution-time choice gate, fail-closed, over
    // EVERY current-stack entry — the extrapolation models future resolutions the
    // window never observed (grown kinds) and re-runs observed kinds in states that
    // differ on projected axes, where a resolver's choice surface (e.g. proliferate
    // eligibility over player counters, CR 701.34a) can open a prompt that the
    // AST-level item-4 scan cannot see. Verdicts come from the ability_scan
    // classifier (pure fact-producers — rejection is decided ONLY here);
    // FreeUnlessLifeReplacements additionally requires the CR 616.1 environmental
    // guard below. THIS block is the single gate seam for resolution-choice
    // rejection (item 3 is untouched and gates a different fact — announcement-time
    // ordering input). Perf: O(stack × AST) + O(objects × defs) via the guard —
    // same order as items (4)/(5).
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
    let mut needs_life_guard = false;
    for entry in &current.stack {
        match stack_entry_resolution_choice_freedom(entry) {
            crate::game::ability_scan::ResolutionChoiceFreedom::MayPrompt => return false,
            crate::game::ability_scan::ResolutionChoiceFreedom::FreeUnlessLifeReplacements => {
                needs_life_guard = true
            }
        }
    }
    if needs_life_guard && life_event_replacements_may_prompt(current) {
        return false;
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
    if fire_time_conditions_read_growing_class(&cf) {
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
/// pins this). NO live/offline caller in 4d-i — exercised only by unit tests + T-B1i.
///
/// `fodder_class` is a CONTENT authority (a representative `&GameObject`), compared
/// LIVE each call via [`fodder_content_eq`] (modulo tapped) — not latched by
/// ObjectId, because fodder tokens are not id-stable. Covers any inert fungible token
/// class (Saproling, Elf Warrior, Thopter, …), so it builds for the class not a card.
#[cfg_attr(not(test), allow(dead_code))] // 4d-ii wires the live/offline caller; 4d-i exercises via unit tests + T-B1i.
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

    // No live off-stack / on-stack observer reads the growing class.
    if fire_time_conditions_read_growing_class(&cf) {
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

/// CR 122.1: classify how a cycle drives PRESERVED `Generic` object counters. This
/// `match` IS the per-`CounterType` classification table for the counter-growth
/// cover — it is WILDCARD-FREE by construction, so a new `CounterType` variant
/// will not compile until it is explicitly classified here (mirrors
/// `CounterType::is_monotone_loop_resource`, which governs the projection). Kept in
/// lockstep with that partition: monotone counters are `project_out_resources`'d
/// away, the non-`Generic` preserved counters gate SBAs/durations and so must
/// compare strict-equal, and only `Generic` is a pure pumped marker.
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
            let growable = match ct {
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
            };
            if !growable {
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
    // F1 (PR-7 Phase 4d-ii, ONE-SIDED-SAFETY): compare `last_recast_context` here even
    // though `impl PartialEq for GameState` excludes it. Excluding a decision context whose
    // fields are loop-INVARIANT (unit-variant ConvokeMode, cross-incarnation-stable CardId,
    // constant controller/from_zone/uses_buyback across a homogeneous recast) is the
    // fail-DANGEROUS direction — a HETEROGENEOUS recast (alternating uses_buyback / from_zone)
    // whose board coincidentally covers would compare EQUAL under exclusion and be falsely
    // certified an infinite CR 732.2a shortcut. COMPARING catches the differing context and
    // rejects. It is `None` at every non-recast loop's sample beat, so this never suppresses a
    // legitimate loop's detection (this IS the sole discriminator — the custom PartialEq omits it).
    a == b
        && a.post_replacement_token_substitution_count
            == b.post_replacement_token_substitution_count
        && a.last_recast_context == b.last_recast_context
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
fn fire_time_conditions_read_growing_class(state: &GameState) -> bool {
    use crate::game::ability_scan as scan;
    // (1) Trigger fire-time conditions (CR 603.4) AND effect bodies.
    for obj in state.objects.values() {
        for (_, def) in crate::game::functioning_abilities::active_trigger_definitions(state, obj) {
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
    // (2) S5: EVERY ability def on a functioning battlefield permanent, any kind.
    // ponytail: this ability-BODY scan is scoped to the battlefield (an activated
    // ability functions only there, CR 602.5a), so an OFF-battlefield source's
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
        if obj
            .abilities
            .iter()
            .any(scan::ability_definition_reads_sibling_mutable)
        {
            return true;
        }
    }
    // (3) Replacement conditions AND bodies (CR 614.1).
    for (_, _, def) in crate::game::functioning_abilities::active_replacements(state) {
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
            if def
                .condition
                .as_ref()
                .is_some_and(scan::static_condition_reads_sibling_mutable)
            {
                return true;
            }
            if !def.modifications.is_empty() {
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
        // (1) printed cost-keyword family.
        if obj
            .keywords
            .iter()
            .any(scan::keyword_cost_reads_growing_class)
        {
            return true;
        }
        // (1b) granted cost-keyword family (AddKeyword / AddKeywordWithDerivedCost)
        // + (2) the STATIC cost surface (`StaticDefinition::mode`, CR 601.2f).
        for def in obj.static_definitions.iter_all() {
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
/// and strip nested `source_id`s from the embedded ability
/// ([`crate::game::triggers::normalize_ability_identity`]). KEEP `controller` (an
/// opponent's otherwise-identical trigger must never merge with the controller's)
/// and the entire `kind` payload (`condition`, `trigger_event`,
/// `subject_match_count`, `die_result`, `description`, `source_name`) — a residual
/// content difference only SUPPRESSES a match (fail-safe). Two same-controller
/// entries differing only in `source_id` (two Blight-Priest copies) resolve
/// identically after the item-4 guard, so identifying them is sound.
fn normalized_stack_entries(state: &GameState) -> Vec<StackEntry> {
    state
        .stack
        .iter()
        .map(|entry| {
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
            norm
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
fn stack_covers(prior: &[StackEntry], current: &[StackEntry]) -> bool {
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
    // FORCED, so the shortcut stays deterministic. Re-derived per-iteration against
    // the live state (the SOLE caller iterates the grown current-stack entries).
    forced_unique_targeting(state, ability)
}

/// CR 603.3d / CR 608.2b / CR 732.2a: exactly one legal target assignment ⇒ the
/// target choice is FORCED, not player ordering input. Reuses the engine's own
/// auto-target oracle (`auto_select_targets_for_ability => Ok(Some(_))` iff a
/// single legal assignment exists, limit=2) — the same authority the trigger
/// dispatcher uses. Fail-closed on any build error, empty slots, or ≥2 legal
/// assignments (`Ok(None)` / `Err`).
fn forced_unique_targeting(
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
    entry: &StackEntry,
) -> crate::game::ability_scan::ResolutionChoiceFreedom {
    use crate::game::ability_scan::ResolutionChoiceFreedom;
    match &entry.kind {
        StackEntryKind::TriggeredAbility { ability, .. } => {
            crate::game::ability_scan::ability_resolution_choice_freedom(ability)
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
    // (i) Trigger fire-time intervening-if conditions (CR 603.4). `active_trigger_
    // definitions` is the liveness authority (CR 702.26b phased-out + CR 114.4
    // command-zone gate) that deliberately does NOT filter by `condition`.
    for obj in state.objects.values() {
        for (_, def) in crate::game::functioning_abilities::active_trigger_definitions(state, obj) {
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
    for (_, _, def) in crate::game::functioning_abilities::active_replacements(state) {
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

/// The proposed-event class a life-affecting `ReplacementEvent` watches. CR 616.1
/// material-ordering competition is counted PER proposed-event class, because a
/// single `ProposedEvent::LifeLoss` draws candidates from every LifeLoss-matching
/// registry key at once (`LoseLife` + `LifeReduced` + `PayLife`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum LifeEventClass {
    /// Matches `ProposedEvent::LifeGain`.
    LifeGain,
    /// Matches `ProposedEvent::LifeLoss`.
    LifeLoss,
}

/// CR 614.1a: is this replacement event in the LIFE class — i.e. does its
/// registry matcher match `ProposedEvent::LifeGain` or `ProposedEvent::LifeLoss`?
/// Compiler-exhaustive over ALL `ReplacementEvent` variants (no wildcard) so a
/// NEW variant fails to compile until classified against the coupling rule.
///
/// COUPLING RULE (grep-enforced when the set is edited): life-class ⇔ the event's
/// registry matcher (`crate::game::replacement`) matches a life `ProposedEvent`.
/// Measured (`rg -n 'ProposedEvent::Life(Gain|Loss)'` over the matcher fns):
/// `gain_life_matcher` (GainLife → LifeGain), `lose_life_matcher` (LoseLife →
/// LifeLoss), `life_reduced_matcher` (LifeReduced → LifeLoss), `pay_life_matcher`
/// (PayLife → LifeLoss). Classify by the MATCHER, not the name — a hand-picked
/// set had already missed `PayLife` and `LifeReduced`.
fn replacement_event_matches_life(event: &ReplacementEvent) -> Option<LifeEventClass> {
    match event {
        ReplacementEvent::GainLife => Some(LifeEventClass::LifeGain),
        ReplacementEvent::LoseLife | ReplacementEvent::LifeReduced | ReplacementEvent::PayLife => {
            Some(LifeEventClass::LifeLoss)
        }
        // Non-life events (explicitly listed ⇒ None, so a new variant must be
        // classified against the coupling rule before it compiles).
        ReplacementEvent::DamageDone
        | ReplacementEvent::Destroy
        | ReplacementEvent::Discard
        | ReplacementEvent::Draw
        | ReplacementEvent::TurnFaceUp
        | ReplacementEvent::Counter
        | ReplacementEvent::ChangeZone
        | ReplacementEvent::Moved
        | ReplacementEvent::AddCounter
        | ReplacementEvent::RemoveCounter
        | ReplacementEvent::CreateToken
        | ReplacementEvent::Tap
        | ReplacementEvent::Untap
        | ReplacementEvent::DealtDamage
        | ReplacementEvent::Mill
        | ReplacementEvent::Attached
        | ReplacementEvent::SearchFound
        | ReplacementEvent::DrawCards
        | ReplacementEvent::ProduceMana
        | ReplacementEvent::Scry
        | ReplacementEvent::CoinFlip
        | ReplacementEvent::Transform
        | ReplacementEvent::Explore
        | ReplacementEvent::Connive
        | ReplacementEvent::AssembleContraption
        | ReplacementEvent::BeginPhase
        | ReplacementEvent::BeginTurn
        | ReplacementEvent::Cascade
        | ReplacementEvent::CopySpell
        | ReplacementEvent::DeclareBlocker
        | ReplacementEvent::GameLoss
        | ReplacementEvent::GameWin
        | ReplacementEvent::Learn
        | ReplacementEvent::LoseMana
        | ReplacementEvent::PlanarDiceResult
        | ReplacementEvent::Planeswalk
        | ReplacementEvent::Proliferate
        | ReplacementEvent::Other(_) => None,
    }
}

/// §2.2 item 6 environmental guard (CR 616.1 + CR 614.1a): can the current
/// life-event replacement environment open a resolution-time prompt on an
/// allow-listed `GainLife`/`LoseLife` resolution? Paired obligation of
/// `ResolutionChoiceFreedom::FreeUnlessLifeReplacements`.
///
/// Over-approximates `find_applicable_replacements` fail-closed: conditions,
/// `valid_player` scopes, and amounts are deliberately ignored (over-count ⇒
/// over-reject ⇒ fail-safe). Def sources = object-attached defs
/// (`active_replacements`, item 5's authority) CHAINED with the game-state-level
/// floating store `state.pending_damage_replacements` (sentinel `ObjectId(0)`,
/// scanned by `find_applicable_replacements` replacement.rs:4838-4862; skip
/// `is_consumed`, mirroring :4859-4861). `pending_step_end_mana_handlers` is a
/// different type gated behind `ProposedEvent::EmptyManaPool`
/// (replacement.rs:4971-4980) that structurally cannot produce a life-class
/// candidate ⇒ excluded. There are NO virtual life candidates in
/// `find_applicable_replacements` (measured — the only `ProposedEvent::LifeGain`
/// there is a `valid_player` filter, not a candidate creator, replacement.rs:4674).
///
/// Rejects when a life-class def is:
/// (a) OPTIONAL — a single optional candidate prompts (replacement.rs:6221-6247);
/// (b) carries a body continuation (`execute`/`runtime_execute`) — a MANDATORY
///     body is stashed as `PostReplacementContinuation::Resolved`
///     (replacement.rs:5511-5524) and drained via
///     `apply_pending_post_replacement_effect` (engine_replacement.rs:1159),
///     which runs an arbitrary `ResolvedAbility` and can set a non-priority
///     `waiting_for` (e.g. a Sacrifice body ⇒ EffectZoneChoice). `execute` is
///     also rejected by item 5 (resource.rs:1058-1060); re-checked here so the
///     guard does not depend on item ordering, and `runtime_execute` is NOT
///     otherwise covered (item 5 scans it only for projected reads,
///     resource.rs:976-981);
/// (c) one of ≥2 defs competing for the SAME proposed-event class — CR 616.1
///     material-ordering prompt (replacement.rs:6263-6279). A single mandatory
///     quantity-mod def with no body (Bloodletter / Rhox Faithmender class)
///     trips NONE of these and resolves deterministically (replacement.rs:6250-6261).
fn life_event_replacements_may_prompt(state: &GameState) -> bool {
    let object_defs =
        crate::game::functioning_abilities::active_replacements(state).map(|(_, _, def)| def);
    let floating_defs = state
        .pending_damage_replacements
        .iter()
        .filter(|def| !def.is_consumed);

    let mut gain_defs = 0usize;
    let mut loss_defs = 0usize;
    for def in object_defs.chain(floating_defs) {
        let Some(class) = replacement_event_matches_life(&def.event) else {
            continue;
        };
        // (a) single optional candidate prompts.
        if crate::game::replacement::replacement_mode_is_optional(&def.mode) {
            return true;
        }
        // (b) mandatory body-continuation drain is prompt-capable.
        if def.execute.is_some() || def.runtime_execute.is_some() {
            return true;
        }
        match class {
            LifeEventClass::LifeGain => gain_defs += 1,
            LifeEventClass::LifeLoss => loss_defs += 1,
        }
    }
    // (c) ≥2 defs competing for one proposed-event class ⇒ CR 616.1 ordering prompt.
    gain_defs >= 2 || loss_defs >= 2
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
    // same fail-safe direction holds for any state field that still references a raw
    // stack id (`stack_paid_facts`, `pending_trigger_entry`, a `WaitingFor` carrying
    // a stack-entry id): left AS-IS, a residual mismatch can only suppress a match.
    // Canonicalizing the position id can therefore never MANUFACTURE a false positive
    // (a wrongful win); it can only make a genuine repeat visible.
    for (pos, entry) in s.stack.iter_mut().enumerate() {
        entry.id = ObjectId(pos as u64);
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
    use crate::types::identifiers::CardId;
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
        b.triggers_fired_this_turn.insert((oid, 0)); // OncePerTurn gate fired
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
        a.trigger_fire_counts_this_turn.insert((oid, 0), 1);
        let mut b = a.clone();
        b.trigger_fire_counts_this_turn.insert((oid, 0), 2); // limit progressed
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
        let mut b = a.clone();
        b.stack.clear();
        b.stack.push_back(trigger_entry(11, 500, 0)); // same source, fresh id
        assert!(
            loop_states_equal_modulo_resources(&a, &b),
            "same triggered ability from the same source must compare equal modulo its fresh id"
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
                subject_match_count: None,
                die_result: None,
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
        let oid = ObjectId(id);
        let object = crate::game::game_object::GameObject::new(
            oid,
            CardId(7),
            PlayerId(1),
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
    /// (`FreeUnlessLifeReplacements`), reads no projected resource. Distinct
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
    /// loop, or classify `Proliferate` ⇒ `FreeUnlessLifeReplacements`.
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
                let oid = bf_object(state, 810);
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
                let oid = bf_object(state, 811);
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.replacement_definitions
                    .push(ReplacementDefinition::new(ReplacementEvent::GainLife));
                obj.replacement_definitions
                    .push(ReplacementDefinition::new(ReplacementEvent::GainLife));
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
                let oid = bf_object(state, 812);
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

    fn recast_ctx(uses_buyback: bool) -> crate::types::game_state::RecastContext {
        use crate::types::game_state::BuybackUsage;
        crate::types::game_state::RecastContext {
            card_id: CardId(4242),
            controller: PlayerId(0),
            from_zone: Zone::Hand,
            uses_buyback: if uses_buyback {
                BuybackUsage::Used
            } else {
                BuybackUsage::NotUsed
            },
            convoke: Some(crate::types::game_state::ConvokeMode::Convoke),
        }
    }

    /// N7 (F1 two-sided `last_recast_context` classify — COVER path via `eq_except_growable`).
    /// (a) two object-cover-equal frames with EQUAL contexts still CERTIFY (no false-negative);
    /// (b) the same frames with a MUTATED context (`uses_buyback` flipped) REJECT (no
    /// false-positive — a heterogeneous recast is caught). Revert-failing: removing the
    /// `a.last_recast_context == b.last_recast_context` conjunct in `eq_except_growable` flips
    /// (b) to COVER while (a) stays COVER ⇒ this test's (b) assertion fails. (a) is the paired
    /// positive reach-guard for (b). Non-vacuous: the custom `impl PartialEq for GameState`
    /// EXCLUDES the field, so this conjunct is the SOLE discriminator.
    #[test]
    fn fodder_cover_last_recast_context_two_sided() {
        // (a) equal contexts ⇒ still covers.
        let (mut prior, mut current) = fodder_cover_base();
        prior.last_recast_context = Some(recast_ctx(true));
        current.last_recast_context = Some(recast_ctx(true));
        assert!(
            fodder_cover(&prior, &current),
            "(a) equal last_recast_context ⇒ object-growth cover still CERTIFIES"
        );
        // (b) mutated context (uses_buyback true→false) ⇒ rejects.
        let (mut p2, mut c2) = fodder_cover_base();
        p2.last_recast_context = Some(recast_ctx(true));
        c2.last_recast_context = Some(recast_ctx(false));
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
    fn loop_states_equal_last_recast_context_two_sided() {
        let mut a = GameState::new_two_player(7);
        inert_token(&mut a, 900, 0, "Engine");
        let mut b = a.clone();
        // (a) equal contexts ⇒ equal.
        a.last_recast_context = Some(recast_ctx(true));
        b.last_recast_context = Some(recast_ctx(true));
        assert!(
            loop_states_equal_modulo_resources(&a, &b),
            "equal last_recast_context ⇒ loop_states_equal_modulo_resources holds"
        );
        // (b) mutated context ⇒ unequal.
        b.last_recast_context = Some(recast_ctx(false));
        assert!(
            !loop_states_equal_modulo_resources(&a, &b),
            "a mutated last_recast_context (uses_buyback flipped) ⇒ NOT equal (F1 conjunct)"
        );
    }
}
