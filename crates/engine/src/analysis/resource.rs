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
    /// `declarable_victims` is the union of the published `Targets` slots' legal targets —
    /// EMPTY for the untargeted class. `slot_magnitude` is the per-period life loss the
    /// certificate attributed to each published slot. A declaration may aim **every** slot
    /// at **one** opponent, so a declarable victim's life magnitude is the SUM over all
    /// slots; that is what makes an all-slots-on-one-seat declaration bounded by
    /// construction rather than by a cross-slot check in `validate_pins`.
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
    // The first production consumer is the bounded offer, which lands in a later phase; the
    // bound ships ahead of it so its conventions are pinned by a unit row before any producer
    // depends on them.
    #[allow(dead_code)]
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

/// CR 732.2a: the facts a CALLER has PROVED about the loop window it is asking a
/// window predicate to certify. Every field is a *proof obligation discharged by the
/// caller*, never a request: a caller that has proved nothing passes
/// [`LoopWindowScope::unproven`] and gets byte-identical pre-change behaviour, so the
/// design is FAIL-CLOSED BY CONSTRUCTION — forgetting to thread a proof can only make
/// a predicate more conservative, never less.
///
/// The `_scoped` predicates below stay identity for [`LoopWindowScope::unproven`]
/// (asserted by `scoped_wrappers_are_identity`) because every guard that reads a field
/// sits inside an `if let Some(..)` / `is_some_and`. `phase_invariant` and `sole_driver`
/// ARE now read — by the growing-class firewall's CR 510.2 / CR 506.1 and CR 117.1b
/// guards — and `cast_card_ids` by the projected firewall's CR 601.2f cost guard, so the
/// scope is no longer write-only; `pinned_slots` is the remaining unread field.
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
    /// CR 732.2a: the per-iteration choice slots the OFFER publishes, which
    /// `decision_template::predictability_gate` then FORCES the declaration to pin.
    /// A slot listed here is a *specified* choice in CR 732.2a's sense, not a free one.
    #[allow(dead_code)] // write-only until the phase that consumes pinned slots.
    pinned_slots: &'a [DecisionSlot],
    /// CR 601.2f (cost determination reads static cost modifiers): `Some(ids)` iff the
    /// caller proved the EXACT set of card ids this window casts — `Some(&[])` for a
    /// window that provably casts nothing. `None` means NO PROOF, i.e. scan everything.
    cast_card_ids: Option<&'a [CardId]>,
}

impl LoopWindowScope<'static> {
    /// The zero-proof scope. Every 2-arg wrapper passes this, which is what makes the
    /// wrappers structurally identity rather than conditionally so.
    pub(crate) const fn unproven() -> Self {
        Self {
            phase_invariant: None,
            sole_driver: None,
            pinned_slots: &[],
            cast_card_ids: None,
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
    pinned_slots: &'a [DecisionSlot],
) -> LoopWindowScope<'a> {
    // (p1) same turn, (p2) same step-granular phase, (p3) no pending extra phase in
    // either frame (CR 500.8).
    let phase_invariant = (pa.turn_number == pb.turn_number
        && pa.phase == pb.phase
        && pa.extra_phases.is_empty()
        && pb.extra_phases.is_empty())
    .then_some(pa.phase);

    // (s1) BOTH sequences non-empty — the `(Some, Some)` arm; (s2) one controller
    // across BOTH sequences.
    let sole_driver = match (
        pa.last_loop_action_sequence.first(),
        pb.last_loop_action_sequence.first(),
    ) {
        (Some(first), Some(_)) => {
            let driver = first.controller;
            pa.last_loop_action_sequence
                .iter()
                .chain(pb.last_loop_action_sequence.iter())
                .all(|ctx| ctx.controller == driver)
                .then_some(driver)
        }
        _ => None,
    };

    LoopWindowScope {
        phase_invariant,
        sole_driver,
        pinned_slots,
        // 2b's axis (the PROJECTED covers), derived at its own call site.
        cast_card_ids: None,
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
    loop_states_cover_modulo_growth_scoped(prior, current, LoopWindowScope::unproven())
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
fn window_cast_card_ids(state: &GameState) -> Option<Vec<CardId>> {
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
/// The `_scope` PARAMETER is still unread: this body's own axis is the PROJECTED
/// firewall, and the scope parameter is the seam for the SIBLING covers. The projected
/// scope conjunct (5) passes downstream is therefore derived LOCALLY, from `current`'s
/// own driving sequence ([`window_cast_card_ids`]) — so behaviour does move through
/// this body even though the parameter does not carry it.
pub(crate) fn loop_states_cover_modulo_growth_scoped(
    prior: &GameState,
    current: &GameState,
    _scope: LoopWindowScope<'_>,
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
    // CR 601.2f: `cast_ids` is bound BEFORE `projected_scope` so NLL keeps the borrow
    // live across the call (`LoopWindowScope::cast_card_ids` is `Option<&'a [CardId]>`).
    let cast_ids = window_cast_card_ids(current);
    // All four fields written explicitly — no functional-update base, so there is no
    // `LoopWindowScope<'static>` -> `LoopWindowScope<'_>` variance question to reason
    // about, and a future FIFTH field is a compile error that forces a decision rather
    // than a silent default. The other three stay at their `unproven()` values: 2b's
    // axis is `projected`, and the sibling proofs belong to the sibling covers.
    let projected_scope = LoopWindowScope {
        phase_invariant: None,
        sole_driver: None,
        pinned_slots: &[],
        cast_card_ids: cast_ids.as_deref(),
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
        window_scope_from_cover_frames(&pa, &pb, &[]),
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
        window_scope_from_cover_frames(&pa, &pb, &[]),
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
/// explicitly classified here. Shared by BOTH `classify_generic_counter_growth` (the
/// ω-cover direction gate) and `grown_generic_counter_targets` (the display
/// re-derivation) so the two can never drift out of lockstep. Kept in lockstep with
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

/// CR 122.1 + CR 701.34a + CR 732.2a: the per-object `(ObjectId, CounterType)` pairs
/// whose PRESERVED `Generic` counters STRICTLY GREW across one cycle (`current` vs
/// `prior`) — the concrete DISPLAY targets of an accepted counter-growth loop
/// (proliferate charge on Pentad Prism, burden on The One Ring). The offer
/// certificate's unbounded axis is object-AGNOSTIC (`Counter(Other, Other)`), so the
/// specific object id / counter type is NOT recoverable from the axis; this
/// re-derives them by diffing each SHARED object's growable counters — the display
/// analog of `classify_generic_counter_growth`, sharing its SAME wildcard-free
/// `generic_counter_is_growable` partition (single-source, so they can't drift).
///
/// Iterates the CURRENT side only: strict growth requires `a > b >= 0`, so a grown
/// counter is necessarily present in `current`'s map — this both captures every
/// grown pair (no false negatives) and is duplicate-free (unlike a two-sided key
/// chain). An object absent from `prior` is caught by the object-set cover, not this
/// axis, so only SHARED objects contribute. DISPLAY-ONLY: the caller renders `∞`
/// from these pairs without mutating the real counter count (CR 701.34a still adds a
/// real counter each cycle; the `∞` is a render of the certified-unbounded loop).
pub(crate) fn grown_generic_counter_targets(
    prior: &GameState,
    current: &GameState,
) -> Vec<(ObjectId, CounterType)> {
    let mut targets = Vec::new();
    for (id, co) in current.objects.iter() {
        let Some(po) = prior.objects.get(id) else {
            continue;
        };
        for (ct, &a) in co.counters.iter() {
            if !generic_counter_is_growable(ct) {
                continue;
            }
            let b = po.counters.get(ct).copied().unwrap_or(0);
            if a > b {
                targets.push((*id, ct.clone()));
            }
        }
    }
    targets
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

/// CR 122.1 + CR 732.2a: the per-object `(ObjectId, CounterType, delta)` triples whose
/// BENEFICIAL-materializable counters strictly grew across one accepted period (`current`
/// vs `prior`) — the batched-collapse δ source. The beneficial analog of
/// `grown_generic_counter_targets` (Generic-only for the DISPLAY channel); this widens to
/// +1/+1 / loyalty / defense via `counter_is_beneficial_materializable`. A CLONE, not a
/// refactor: the display/cover Generic partition must stay narrow. Iterates the CURRENT
/// side (strict growth ⇒ the grown counter is present in `current`); only SHARED objects
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
                    // widening variants. `PlayerFilter` (`types/ability.rs`) has 25
                    // variants; enumerating would make THIS site assert that the other 23
                    // leave a foreign ability unreachable — a claim nothing forces anyone
                    // to re-verify when variant 26 lands. `is_none()` asserts nothing about
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
    // CR 614.1 / CR 113.6: `active_replacements` is all-zones (its callers restrict).
    // `find_applicable_replacements` — the real pipeline this over-approximates —
    // scans [Battlefield, Command] (plus the entering/discarded card, irrelevant to a
    // life event). A life-class replacement on a card in the library / hand /
    // graveyard cannot apply during the loop; scanning it is the same all-zones
    // false-reject class as the observer firewalls, so match the pipeline's scope.
    let object_defs = crate::game::functioning_abilities::active_replacements(state)
        .filter(|(_, obj, _)| matches!(obj.zone, Zone::Battlefield | Zone::Command))
        .map(|(_, _, def)| def);
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
            pinned_slots: &[],
            cast_card_ids: None,
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
            pinned_slots: &[],
            cast_card_ids: None,
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
                loop_states_cover_modulo_growth_scoped(prior, current, LoopWindowScope::unproven()),
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
                "BattleProtectorChoice (CR 310.10 + CR 704.5w / CR 704.5x)",
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
                pinned_slots: &[],
                cast_card_ids: Some(&never_cast),
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
            pinned_slots: &[],
            cast_card_ids: Some(&never_cast),
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
            pinned_slots: &[],
            cast_card_ids: Some(&recast),
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
            pinned_slots: &[],
            cast_card_ids: Some(&other),
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
            window_cast_card_ids(&state),
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
            window_cast_card_ids(&state),
            Some(vec![CardId(64)]),
            "(2) a one-entry sequence yields exactly that card id"
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
        let state = serde_json::from_value::<crate::types::game_state::PersistedGameState>(
            envelope["gameState"].clone(),
        )
        .expect("the real 4p gameState restores through the persisted ingress")
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
            pinned_slots: &[],
            cast_card_ids: Some(&cast),
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
            pinned_slots: &[],
            cast_card_ids: Some(&recast),
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
            window_scope_from_cover_frames(&pa, &pb, &[]).sole_driver,
            Some(PlayerId(0)),
            "PAIRED POSITIVE: a homogeneous single-driver window proves CR 117.1's premise"
        );

        // (s2) heterogeneous ACROSS the two frames — the case a `pa`-only read would
        // mint `Some(P0)` for, which is the relieving direction #4603 forbids.
        let mut pb_other = base();
        pb_other.last_loop_action_sequence = vec![ctx(1)];
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb_other, &[]).sole_driver,
            None,
            "(s2) a two-controller window proves nothing about who holds priority"
        );

        // (s2) heterogeneous WITHIN one frame.
        let mut pa_mixed = base();
        pa_mixed.last_loop_action_sequence = vec![ctx(0), ctx(1)];
        assert_eq!(
            window_scope_from_cover_frames(&pa_mixed, &pb, &[]).sole_driver,
            None,
            "(s2) an interleaved sequence is fail-closed"
        );

        // (s1) an EMPTY sequence proves nothing — not "nobody drove this".
        let mut pb_empty = base();
        pb_empty.last_loop_action_sequence.clear();
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb_empty, &[]).sole_driver,
            None,
            "(s1) an empty driving sequence is NO PROOF, so it cannot relieve anything"
        );

        // ── `phase_invariant` — CR 500.1 / CR 506.1 / CR 500.8 ──
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb, &[]).phase_invariant,
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
            window_scope_from_cover_frames(&pa, &pb_extra, &[]).phase_invariant,
            None,
            "(p3) CR 500.8: a pending extra phase breaks `equal phase ⇒ never left it`"
        );

        // (p1) different turns.
        let mut pb_turn = base();
        pb_turn.turn_number = 14;
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb_turn, &[]).phase_invariant,
            None,
            "(p1) frames from different turns bound nothing about one window's phase"
        );

        // (p2) different step-granular phases.
        let mut pb_phase = base();
        pb_phase.phase = Phase::PostCombatMain;
        assert_eq!(
            window_scope_from_cover_frames(&pa, &pb_phase, &[]).phase_invariant,
            None,
            "(p2) a window that crosses a phase boundary is not phase-invariant"
        );
    }
}
