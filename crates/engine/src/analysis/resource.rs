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

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::types::ability::{ActivationRestriction, DamageModification};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::game_state::{loop_states_equal, GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaType;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::replacements::ReplacementEvent;

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
            // CR 122.1 + CR 704.5c: poison counters live in a dedicated field.
            //
            // GAP-5 (multiplayer prerequisite): the poison axis is AGGREGATE-keyed —
            // `(CounterClass::Poison, ObjectClass::Player)` carries NO victim `PlayerId`,
            // so a poison delta is summed across the whole table, not attributed to the
            // afflicted player. `live_mandatory_loop_winner` reads this summed pair
            // conservatively (loop_check.rs ~239), and `derive_views`' `attribution_player`
            // routes any poison ∞ to the loop's controller (see the note at
            // derived_views.rs). That is correct ONLY because no live producer emits a
            // poison axis today; before any future live poison/infect loop producer is
            // enabled this key MUST be re-keyed by victim `PlayerId` (CR 704.5c: the
            // afflicted player owns the loss), or a multiplayer poison ∞ would attribute
            // to the wrong seat. Inert documentation — no behavior change here.
            if player.poison_counters > 0 {
                v.counters.insert(
                    (CounterClass::Poison, ObjectClass::Player),
                    player.poison_counters as i64,
                );
            }
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
    loop_states_equal(&pa, &pb) && loyalty_activation_counts_match(&pa, &pb)
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
    ability.targets.is_empty()
        && ability.multi_target.is_none()
        && ability.distribution.is_none()
        && ability.target_constraints.is_empty()
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

/// Clone a state through `normalize_for_loop` and additionally zero every
/// monotone resource the modulo comparison must ignore. The result is only ever
/// fed to `loop_states_equal`; it is never used as a live game state.
fn project_out_resources(state: &GameState) -> GameState {
    let mut s = state.normalize_for_loop();

    for player in &mut s.players {
        // CR 119: life is monotone in a drain/lifegain loop.
        player.life = 0;
        // CR 106.1: floating mana is consumed/produced within the loop.
        player.mana_pool.clear();
        // CR 122.1: player counters that a loop pumps (poison/energy/…).
        player.poison_counters = 0;
        player.energy = 0;
        player.player_counters.clear();
        // Per-turn resource trackers the strict PartialEq compares — these grow
        // with the loop but do not change the board configuration.
        player.life_gained_this_turn = 0;
        player.life_lost_this_turn = 0;
        player.cards_drawn_this_turn = 0;
        player.cards_drawn_this_step = 0;
    }

    for (_, object) in s.objects.iter_mut() {
        // CR 120: marked damage is a monotone resource (lifelink/ping loops).
        object.damage_marked = 0;
        // CR 122.1: project out only *monotone* counters (CR 122.1a/613.4c
        // +1/+1, -1/-1, P/T; CR 306.5b loyalty; CR 310.4c defense) — these are
        // the pumped resource of a +1/+1 or loyalty loop, so two cycles compare
        // as the same board. PRESERVE consumable/duration/state-gating counters
        // (CR 122.1b/c/d stun/shield/keyword; CR 702.62a/63a time; CR 702.32a
        // fade; CR 702.24a age; CR 714.3 lore; generic): consuming one of these
        // is a real board change, not a monotone pump, so it must remain visible
        // to `objects_content_eq` (game_state.rs counter comparison).
        object
            .counters
            .retain(|ct, _| !ct.is_monotone_loop_resource());
        // CR 613.4c: the counter-derived fields are zeroed because they derive
        // ONLY from the monotone counters just projected out — power/toughness
        // fold only `power_toughness_delta()==Some` counters, loyalty derives
        // only from CounterType::Loyalty and defense only from CounterType::Defense.
        // The preserved counters never reach these four fields, so zeroing cannot
        // mask a consumed non-monotone counter.
        object.power = None;
        object.toughness = None;
        object.loyalty = None;
        object.defense = None;
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
        AbilityCondition, CountScope, Effect, QuantityExpr, QuantityRef, ReplacementCondition,
        ReplacementDefinition, ResolvedAbility, StaticCondition, StaticDefinition, TargetFilter,
        TargetRef, TriggerCondition, TriggerDefinition,
    };
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
}
