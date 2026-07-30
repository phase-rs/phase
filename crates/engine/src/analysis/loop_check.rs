//! Engine A — the **dynamic loop-confirmation** entry point.
//!
//! PR-0 gave the [`ResourceVector`] (the monotone axes a loop can pump) and
//! [`loop_states_equal_modulo_resources`] (board/zones/tap-state equal, resources
//! allowed to differ). PR-1 gave [`crate::analysis::LoopProbe`], which drives a
//! `GameRunner` and measures the per-iteration [`ResourceVector`] delta. This
//! module is the classifier that turns those two measurements into a
//! [`LoopCertificate`].
//!
//! # What "detection" means here
//!
//! [`detect_loop`] is the offline classifier: given two driven states plus a
//! per-cycle delta it answers "what resource is unbounded and how does this loop
//! win?" It is called by analysis code and the corpus test harness on a *driven*
//! `GameRunner`.
//!
//! [`live_mandatory_loop_winner`] couples that classifier into the live reducer
//! (`game::engine::reconcile_terminal_result`, CR 732.2a / CR 704.5a): at an
//! all-mandatory cascade whose board has returned identical modulo monotone resources
//! (and the volatile stack id, see `resource::project_out_resources`) while exactly
//! one opponent's life drains without bound, it shortcuts to the forced loss instead
//! of halting on the resource ceiling. PR-3 (Option C) scans a persisted bounded ring
//! of post-resolution snapshots (`GameState::loop_detect_ring`), maintained at the
//! post-pipeline frame of `game::engine::pass_priority_once_with_pipeline` (after
//! `run_post_action_pipeline` places refilling triggers, CR 603.3) and scanned at the
//! single SBA-reconciliation seam — so the win path
//! fires LIVE under the default per-beat `apply(PassPriority)` drive (the production
//! frontend default), which runs `reconcile_terminal_result` after every beat. Note
//! `run_auto_pass_loop` does NOT call `reconcile_terminal_result` inside its internal
//! iterations, so its net-progress grind still runs to the natural CR 704.5a death;
//! the per-beat drive is the accelerated path. So `detect_loop` IS now reached from
//! the reducer via that
//! helper. The strict CR 104.4b / CR 732.4 mandatory-DRAW path (a repeat with no net
//! progress) and the `emit_resolution_halt` runaway backstop are unchanged — the live
//! win path is strictly additive and fires only when life strictly advances toward a
//! single determinate opponent loss.
//!
//! # The detection rule (CR 732.2a — the shortcut, not the draw)
//!
//! A confirmed net-progress loop is exactly the pair of conditions PR-0 built:
//!
//! 1. **Same board** — [`loop_states_equal_modulo_resources`] holds between the
//!    state at the start of a cycle and the state at the end (controller, zone,
//!    tap-state, attachments, object count, stack, phase, priority all identical;
//!    only the monotone resources may differ). This is the *complement* of the
//!    strict CR 104.4b equality the live draw path uses.
//! 2. **Net progress** — the per-cycle [`ResourceVector::delta`] satisfies
//!    [`ResourceVector::is_net_progress`] (≥1 axis strictly increased and no
//!    *consumed* axis — mana, life — went net-negative).
//!
//! When both hold, the loop is repeatable without bound (CR 732.2a: a shortcut
//! that "repeats a specified number of times"), and [`detect_loop`] returns a
//! [`LoopCertificate`] naming the unbounded axes ([`ResourceVector::unbounded_components`])
//! and the derived [`WinKind`]. When either fails, it returns `None` — the
//! soundness guarantee: no certificate for a non-loop or a non-progressing cycle.

use crate::analysis::decision_template::{DecisionTemplate, IterationCount};
use crate::analysis::resource::{
    loop_states_cover_modulo_growth, loop_states_equal_modulo_resources, BoardDelta, CounterClass,
    ObjectClass, ResourceAxis, ResourceVector,
};
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;
use serde::{Deserialize, Serialize};

/// How a confirmed net-progress loop reaches a win (or merely accrues unbounded
/// advantage), derived from its unbounded resource axes.
///
/// This is the engine-side, analysis-owned classification. It deliberately does
/// **not** reuse `phase-ai`'s `combo::WinKind` — that enum lives in a crate that
/// *depends on* `engine`, so it cannot be imported here, and it is a coarser
/// 3-variant author's-claim vocabulary (`ImmediateLoss` / `InfiniteLoop` /
/// `LethalDamage`). The detector classifies the *measured* unbounded axis, so it
/// needs the finer set below; PR-8 maps this onto `combo::WinKind` when it couples
/// the certificate into the AI.
///
/// PR-7 Phase 3: gains `Serialize`/`Deserialize` because it now rides inside
/// `WaitingFor::LoopShortcut`'s `LoopCertificate` and `ShortcutProposal` across the
/// serialization boundary (all fields are derived from public board state — no hidden
/// info). Serde default (externally tagged): each unit variant serializes as its bare
/// name string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WinKind {
    /// CR 704.5a: an opponent's life is driven to 0 or less — unbounded damage to
    /// or unbounded life loss from an opponent (burn pings, drains, lifeloss).
    LethalDamage,
    /// CR 704.5c: an opponent accrues 10+ poison counters — an unbounded poison
    /// (infect/proliferate-poison) loop.
    PoisonLoss,
    /// CR 104.3c / CR 121.4: an opponent's library is emptied (mill) such that the
    /// next draw — or the mill itself reaching 0 — loses them the game. Surfaces as
    /// an unbounded *downward* library axis on an opponent.
    Decking,
    /// CR 104.2: an explicit "you win the game" / "that player loses the game"
    /// effect fires each cycle (e.g. an Aetherflux-style life-payment, a
    /// Thassa's-Oracle-style deckout win). Reserved for loops whose win is a
    /// printed win/loss condition rather than a resource threshold.
    ImmediateWin,
    /// CR 500.7: unbounded extra turns — a turns loop that wins by simply never
    /// passing the game back.
    ExtraTurns,
    /// A loop that accrues an unbounded *advantage* resource (mana, tokens, cards
    /// drawn, casts, combat phases, generic triggers, +1/+1 or loyalty counters,
    /// death/ETB/LTB/sac trigger engines) without, by itself, being a direct loss
    /// condition for an opponent. The canonical CR 732.2a beneficial loop; the
    /// payoff that converts the advantage to a win is a separate card.
    Advantage,
}

/// A sound certificate that a candidate cycle is an infinite net-progress loop.
///
/// Produced only by [`detect_loop`] when the board is identical modulo resources
/// **and** the per-cycle resource delta is net-progress. It is an *analysis*
/// value — never stored on `GameState`; PR-3 acts on an equivalent live signal. PR-7
/// Phase 3 serializes a certificate into `WaitingFor::LoopShortcut` for the interactive
/// shortcut offer, so it gains `Serialize`/`Deserialize` (every field is public board
/// state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopCertificate {
    /// The resource axes that grew (or, for a mill loop, shrank) each cycle — the
    /// unbounded resources, as named by [`ResourceVector::unbounded_components`].
    /// A non-empty vector is an invariant of a returned certificate.
    pub unbounded: Vec<ResourceAxis>,
    /// The classified win condition derived from `unbounded`.
    pub win_kind: WinKind,
    /// CR 104.4b vs CR 732.2a/CR 732.6: whether the cycle is all-mandatory (no
    /// "may"/choice once started). `true` ⇒ a forced loop the live path would draw
    /// (CR 732.4) absent a net resource; `false` ⇒ an optional loop a player chooses
    /// to repeat. The detector cannot infer optionality from two states alone, so
    /// the caller (which drives the actions) supplies it.
    pub mandatory: bool,
    /// CR 110.1: non-recycled per-cycle remainder of battlefield permanents (the "+1
    /// untapped" seed). EMPTY for every certificate this phase produces (both detection
    /// paths require an identical battlefield); wired now so an object-growth path
    /// populates it with no further change. NOT a `ResourceAxis` — concrete permanents.
    pub residual_board_delta: BoardDelta,
}

impl LoopCertificate {
    /// True iff `self.unbounded` is a superset of every axis in `expected`
    /// (order-independent). The corpus harness uses this: a certificate must name
    /// *at least* the combo's documented unbounded axis (it may legitimately name
    /// more — e.g. a lifelink ping loop is unbounded on *both* damage and life).
    pub fn covers(&self, expected: &[ResourceAxis]) -> bool {
        expected.iter().all(|e| self.unbounded.contains(e))
    }
}

/// CR 732.2a: the public, log/display summary a `WaitingFor::RespondToShortcut` carries
/// to each responding opponent — "the player with priority suggests repeating this loop
/// N times". Every field is derived from public board state (the confirmed certificate +
/// the proposer's declared count), so there is no hidden information to redact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutProposal {
    /// CR 732.2a: the player with priority who proposed the shortcut. This is separate from
    /// `predicted_winner`: a player may propose a shortcut whose deterministic outcome wins
    /// the game for another player.
    pub proposer: PlayerId,
    /// The determinate winner measured when the offer was created. `None` represents an
    /// advantage-only object-growth offer, which may be materialized but cannot be crowned.
    pub predicted_winner: Option<PlayerId>,
    /// CR 732.1b: how many times to repeat before stopping. Phase 3 only ever proposes
    /// [`IterationCount::UntilLethal`] (a determinate CR 704.5a drain).
    pub count: IterationCount,
    /// The confirmed unbounded axes (from the certificate) — display only.
    pub unbounded: Vec<ResourceAxis>,
    /// How the loop wins (from the certificate) — display only.
    pub win_kind: WinKind,
    /// PR-7 Phase 4b: an optional CR 732.2a decision template pinning the loop's
    /// free choices for `IterationCount::Fixed(N)` finite materialization. `None`
    /// for a choice-free drain (every existing offer/test) or an `UntilLethal`
    /// proposal, which never reads this field. Absent from `On`/`Off` serialized
    /// streams (skip-if-none), so this is a byte-preserving addition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<DecisionTemplate>,
}

/// CR 732.2b/c: an opponent's answer to a proposed loop shortcut. `Accept` lets the
/// shortcut proceed; `Shorten` names an earlier stopping point (Phase 3 realizes this
/// conservatively as decline-to-manual — the opponent receives a real priority window
/// instead of the loop being auto-taken; finite-K materialization is Phase 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ShortcutResponse {
    /// CR 732.2c: this player agrees to take the shortcut.
    Accept,
    /// CR 732.2b: this player names an earlier ending point (`at_iteration` cycles in).
    Shorten { at_iteration: u32 },
}

/// Engine A's primary offline classification entry point.
///
/// Given the game state at the **start** and **end** of one candidate loop cycle
/// plus the per-cycle [`ResourceVector`] `delta` (typically from
/// [`crate::analysis::LoopProbe::iteration_delta`]), confirm whether the cycle is
/// an infinite net-progress loop and, if so, classify it.
///
/// Returns `Some(LoopCertificate)` iff **both**:
/// 1. [`loop_states_equal_modulo_resources`] holds between `cycle_start` and
///    `cycle_end` (same board, resources may differ), and
/// 2. `delta.is_net_progress()` holds (≥1 axis up, no consumed axis net-negative).
///
/// Otherwise returns `None`. The `controller` and `mandatory` flags are both
/// caller-supplied facts the detector cannot infer from two states alone:
/// `controller` is the loop's controlling player (so the consumed-axis constraint
/// is scoped to *their* life/mana and opponent depletion reads as progress, and
/// the win classifier can tell an opponent loss from self-mill/lifegain), and
/// `mandatory` records whether the driven cycle contained an optional choice. The
/// caller, which drove the actions, knows both.
pub fn detect_loop(
    cycle_start: &GameState,
    cycle_end: &GameState,
    delta: &ResourceVector,
    controller: PlayerId,
    mandatory: bool,
) -> Option<LoopCertificate> {
    // CR 732.2a: the board must have returned to an identical configuration
    // modulo the monotone resources — OR covered it by pure inert object growth
    // (PR-7 Phase 4a offline object-growth cover; the residual `board_delta` below
    // lights up with the grown permanents when this arm fires). Constant-depth
    // short-circuits, so behavior for every existing (non-growth) cycle is
    // byte-unchanged. This is the OFFLINE classifier only — no live/reducer path.
    if !(loop_states_equal_modulo_resources(cycle_start, cycle_end)
        || crate::analysis::resource::loop_states_cover_modulo_object_growth(
            cycle_start,
            cycle_end,
        )
        // CR 122.1 + CR 732.2a: OR covered by pure preserved-`Generic` counter
        // growth (the proliferate/charge Pentad Prism, burden The One Ring shape).
        // Offline Advantage certification only — this seam never crowns a GameOver.
        || crate::analysis::resource::loop_states_cover_modulo_counter_growth(
            cycle_start,
            cycle_end,
        ))
    {
        return None;
    }
    // CR 732.2a: and a resource must have strictly advanced without an
    // unsustainable consumed-axis deficit for the loop's controller — otherwise
    // nothing goes unbounded. This is controller-aware (see `net_progress_for`):
    // PR-0's `ResourceVector::is_net_progress` treats *any* player's life/mana
    // going negative as disqualifying, which is correct for a self-sustainability
    // question but wrongly rejects a damage/drain/mill loop whose entire point is
    // to drive an OPPONENT's life or library down. The caller supplies the loop's
    // `controller`, so the consumed-axis constraint is scoped to that player and
    // opponent depletion is treated as progress.
    if !delta.net_progress_for(controller) {
        return None;
    }

    let unbounded = delta.unbounded_axes_for(controller);
    // `is_progress` guarantees ≥1 unbounded axis, but guard the empty case
    // defensively so a returned certificate always names ≥1 axis.
    if unbounded.is_empty() {
        return None;
    }

    let win_kind = classify_win_kind(controller, delta);
    Some(LoopCertificate {
        unbounded,
        win_kind,
        mandatory,
        // Empty by construction of the `:162` equal-board gate
        // (`loop_states_equal_modulo_resources` guarantees `cycle_start`/`cycle_end`
        // battlefields are equal, so `added`/`removed` are []). This is the SINGLE
        // population seam: it calls `board_delta` rather than hard-coding
        // `BoardDelta::default()` so that when a future object-growth path relaxes the
        // `:162` gate, the residual lights up with no further edit (fail-loud), instead
        // of a silent default that never fires until someone remembers to swap it. A
        // computed-empty `BoardDelta` compares equal to `BoardDelta::default()`, so the
        // derived-`PartialEq` certificate equalities in `corpus_tests.rs` stay intact.
        // Invariant pinned by `residual_empty_for_constant_depth` (T12). (No CR
        // annotation: this is an invariant/plumbing comment, not rule-implementing code.)
        residual_board_delta: crate::analysis::resource::board_delta(cycle_start, cycle_end),
    })
}

/// CR 732.2a + CR 704.5a: the LIVE coupling of [`detect_loop`] into the reducer.
///
/// At an all-mandatory auto-pass cascade whose board has returned identical (modulo
/// monotone resources AND the volatile stack id, see
/// [`crate::analysis::resource::project_out_resources`]), decide whether the loop
/// forces a single determinate opponent life-loss and, if so, name the winner.
/// Returns `None` unless the outcome is unambiguous (the soundness guarantee: the
/// reducer only shortcuts to a WIN it can prove).
///
/// The caller guarantees `mandatory == true` (every iteration in the auto-pass loop
/// is mandatory by construction) and passes the LIVE (raw) reducer state as
/// `cycle_end` so the SBA-layer can't-lose/can't-win firewall sees real
/// `transient_continuous_effects` and is not perturbed by `normalize_for_loop`'s
/// `layers_dirty = full()`. `cycle_start` is a prior NORMALIZED window snapshot; the
/// caller-measured per-cycle `delta` is the `snapshot`/`delta` difference between
/// them.
///
/// Every `BTreeMap` read uses `.get(&k).copied().unwrap_or(0)` — `map_delta` drops
/// zero-delta keys, so an unchanged axis is ABSENT and `[]` would panic in the live
/// reducer.
pub(crate) fn live_mandatory_loop_winner(
    cycle_start: &GameState,
    cycle_end: &GameState,
    delta: &ResourceVector,
) -> Option<PlayerId> {
    // CR 104.1: the living players (not eliminated).
    let living: Vec<PlayerId> = cycle_end
        .players
        .iter()
        .filter(|p| !p.is_eliminated)
        .map(|p| p.id)
        .collect();
    // Need at least one opponent to force a loss on.
    if living.len() < 2 {
        return None;
    }

    // CR 704.5a (life ≤ 0) OR CR 704.5c (poison → 10): partition living into strict
    // fallers vs. non-fallers. A player whose life is draining OR whose poison is
    // accumulating each cycle extrapolates to a loss. Delta-based (not the absolute SBA
    // threshold): the loop works on per-cycle deltas + extrapolation. Exactly one
    // non-faller is the sole survivor candidate; since fallers/non-fallers partition
    // `living`, that single non-faller condition IS "every other living player falls"
    // (CR 104.2a).
    let fallers: Vec<PlayerId> = living
        .iter()
        .copied()
        .filter(|p| {
            delta.life.get(p).copied().unwrap_or(0) < 0
                || delta.poison.get(p).copied().unwrap_or(0) > 0
        })
        .collect();
    let nonfallers: Vec<PlayerId> = living
        .iter()
        .copied()
        .filter(|p| !fallers.contains(p))
        .collect();
    if nonfallers.len() != 1 {
        return None;
    }
    let winner = nonfallers[0];

    // Second-loss-path firewall (life axis only), over ALL living players — keep the
    // 2p behavior generalized to the pod. CR 704.5b / CR 121.4: any library loss is a
    // second determinate-loss path.
    if living
        .iter()
        .any(|p| delta.library_delta.get(p).copied().unwrap_or(0) < 0)
    {
        return None;
    }
    // CR 704.5c: poison is now per-victim (delta.poison) and handled by the faller
    // partition (G-7) — a player whose poison rises IS a faller, and a rising poison on
    // the winner would make the winner a faller (so it can't be the sole non-faller).
    // The former aggregate-poison firewall (unattributable → conservative None) is
    // superseded and removed.

    // CR 101.2 firewalls, generalized. CR 104.3b + CR 101.2: NO faller may be a player
    // who can't lose the game (Platinum Angel). CR 104.2b + CR 101.2: the winner can't
    // be named if they can't win (Abyssal Persecutor). Evaluated on the LIVE
    // `cycle_end` so static effects see the real board.
    if fallers
        .iter()
        .any(|&p| crate::game::sba::player_has_cant_lose(cycle_end, p))
    {
        return None;
    }
    if crate::game::static_abilities::player_has_cant_win(cycle_end, winner) {
        return None;
    }

    // CR 732.2a board-recurrence gate: constant-depth exact recurrence OR a
    // growing-cascade covering pair (the ≥3p fan-out grows the stack without bound,
    // so the exact-depth equality never matches — the coverability path is required).
    //
    // DELIBERATE NON-WIRING (Residual B): `loop_states_cover_modulo_counter_growth`
    // is intentionally NOT a disjunct here. This is a GameOver-capable winner path,
    // and it is unreachable for a pure charge/burden growth loop anyway — such a
    // loop has no life/poison faller, so the `nonfallers.len() != 1` gate above
    // early-returns `None` before this recurrence gate is ever consulted. Adding the
    // disjunct here would be dead code today AND a soundness hazard tomorrow (it
    // would be the seam a future counter-growth loop that ALSO carries a life-faller
    // could ride into a GameOver). Left fail-closed by design; the counter-growth
    // cover only ever routes through the offline `detect_loop` cert and the live
    // Path-C revocable capability mark, neither of which can end the game.
    if !(loop_states_equal_modulo_resources(cycle_start, cycle_end)
        || loop_states_cover_modulo_growth(cycle_start, cycle_end))
    {
        return None;
    }

    // CR 732.2a: net progress for the winner (≥1 unbounded axis, no consumed-axis
    // deficit on the winner's own life/mana). Replaces the former `detect_loop(...)`
    // delegation, which re-ran only the exact-depth equality and would reject the
    // growing-cascade board the gate above just accepted.
    //
    // The measured per-cycle drain is a LOWER BOUND on the actual drain rate: it is
    // at least the delta observed between the two compared frames. A super-critical
    // (μ>1) cascade only ACCELERATES from here — each cycle spawns more drain than
    // the last — so proving progress on the measured floor is sufficient; the real
    // trajectory reaches lethal no later than the linear extrapolation implies.
    if !delta.net_progress_for(winner) {
        return None;
    }
    // Scope the live shortcut to a determinate loss axis — CR 704.5a life drain
    // (`LethalDamage`) or CR 704.5c poison (`PoisonLoss`) — never a pure advantage
    // engine or mill. classify's branch order (damage → opp-life<0 → poison>0 → …)
    // reaches the poison branch before the Advantage fallthrough, so the winner's own
    // lifegain does not mask a rising-poison loss.
    if !matches!(
        classify_win_kind(winner, delta),
        WinKind::LethalDamage | WinKind::PoisonLoss
    ) {
        return None;
    }

    // R5-B2 simultaneity floor (CR 704.3 / CR 800.4a / CR 104.2a): with ≥2 fallers,
    // require EQUAL per-cycle life deltas so all fallers cross lethal in the SAME
    // resolution's CR 704.3 SBA batch — then the sole CR 104.2a elimination is
    // terminal and no post-CR-800.4a continuation is ever modeled. The seam adds the
    // complementary per-frame pairwise-equality check ([`fallers_lives_pairwise_equal`]).
    // `fallers.len() == 1` needs no gate: the sole opponent's elimination IS the
    // terminal event (2p behavior byte-preserved).
    if fallers.len() >= 2 {
        let first = delta.life.get(&fallers[0]).copied().unwrap_or(0);
        if fallers
            .iter()
            .any(|p| delta.life.get(p).copied().unwrap_or(0) != first)
        {
            return None;
        }
        // CR 704.3 poison simultaneity: all poison-fallers must cross 10 in the SAME SBA
        // batch. Require BOTH equal per-cycle poison delta AND equal ABSOLUTE poison at
        // cycle_end — equal-absolute alone is a fail-open (differing deltas with equal
        // current totals cross at different future cycles); equal-delta alone is a
        // fail-open (equal +1/cycle from staggered starts). Staggered on either axis ⇒
        // fail-safe None. This also rejects mixed life+poison pods (a life-faller has
        // poison 0, a poison-faller has poison>0 → unequal → None). Unlike the life
        // arm, poison is monotone (proliferate only adds), so no intra-cycle dip check
        // is needed (contrast `winner_life_never_dips`, which stays life-only).
        let first_pd = delta.poison.get(&fallers[0]).copied().unwrap_or(0);
        if fallers
            .iter()
            .any(|p| delta.poison.get(p).copied().unwrap_or(0) != first_pd)
        {
            return None;
        }
        let poison_at = |p: &PlayerId| {
            cycle_end
                .players
                .iter()
                .find(|q| q.id == *p)
                .map_or(0, |q| q.poison_counters as i64)
        };
        let first_pois = poison_at(&fallers[0]);
        if fallers.iter().any(|p| poison_at(p) != first_pois) {
            return None;
        }
    }
    Some(winner)
}

/// CR 704.5a + CR 104.4a: the loop's winner (the sole strict non-faller) must never
/// dip below its prior life at ANY per-resolution ring frame. Named for the winner,
/// NOT the loop's controller: a mandatory-loop trigger can be controlled by a faller,
/// so the controller is not necessarily the player this guard protects — the winner
/// (the one non-faller) is. A transient intra-cycle dip that recovers to a
/// non-negative NET delta would still kill the winner via the CR 704.5a SBA at low
/// absolute life before the extrapolated win — a net-delta check cannot see it.
/// Per-resolution granularity IS SBA granularity here (CR 704.3 checks whenever a
/// player would get priority, between resolutions), and consecutive ring frames are
/// consecutive resolutions (a non-sampling beat clears the ring), so requiring
/// `life[winner]` non-decreasing across the matched window (prior frame → every
/// subsequent ring frame → the live state) is exactly right. Winner draw-from-empty
/// is correctly unreachable (a non-faller never crosses a loss SBA).
pub(crate) fn winner_life_never_dips(frames: &[&GameState], winner: PlayerId) -> bool {
    let mut prev: Option<i32> = None;
    for frame in frames {
        let Some(life) = frame
            .players
            .iter()
            .find(|p| p.id == winner)
            .map(|p| p.life)
        else {
            continue;
        };
        if prev.is_some_and(|p| life < p) {
            return false;
        }
        prev = Some(life);
    }
    true
}

/// CR 704.3 (one SBA batch) + CR 800.4a + CR 104.2a: the R5-B2 simultaneity floor,
/// seam half. With ≥2 fallers, every faller's `player.life` must be pairwise-equal at
/// EVERY ring frame (incl. the live state); combined with the predicate's equal
/// per-cycle deltas, all fallers stay pairwise-equal at every extrapolated frame and
/// therefore cross lethal in the SAME resolution's CR 704.3 SBA batch. The single
/// CR 104.2a elimination is then terminal ("happens immediately and overrides all
/// effects"), so CR 800.4a's machinery-removal side effects occur only after the game
/// is already decided — zero post-death continuation to model. Staggered-life games
/// become fail-safe FALSE NEGATIVES. Only meaningful for `fallers.len() >= 2`.
pub(crate) fn fallers_lives_pairwise_equal(frames: &[&GameState], fallers: &[PlayerId]) -> bool {
    frames.iter().all(|frame| {
        let lives: Vec<i32> = fallers
            .iter()
            .filter_map(|&fp| frame.players.iter().find(|p| p.id == fp).map(|p| p.life))
            .collect();
        lives.windows(2).all(|w| w[0] == w[1])
    })
}

/// Derive the [`WinKind`] from the measured per-cycle delta.
///
/// Classification is by the **most decisive** unbounded axis, in CR loss-priority
/// order: an opponent-lethal axis (damage/life-loss → CR 704.5a, poison → CR
/// 704.5c, decking → CR 104.3c/121.4, extra turns → CR 500.7) outranks a pure
/// advantage engine (mana/tokens/draw/…). A loop that pumps several axes is named
/// by the first loss condition it satisfies; if none, it is [`WinKind::Advantage`].
///
/// `controller` distinguishes "an opponent" from the loop's controller: damage to
/// / life loss from / mill on a player who is *not* the loop's controller is an
/// opponent loss condition; the corpus rows are two-player, so any non-controller
/// player is the opponent.
pub(crate) fn classify_win_kind(controller: PlayerId, delta: &ResourceVector) -> WinKind {
    // CR 704.5a: a player at 0 life loses — so unbounded damage is a WIN only when
    // the damaged player is an OPPONENT (a non-controller). Damage to the loop's
    // own controller (self-ping offset by lifegain) is an advantage engine, not a
    // win; mirror the life/decking branches' opponent-victim discrimination.
    if delta
        .damage_dealt
        .iter()
        .any(|(pid, &n)| n > 0 && *pid != controller)
    {
        return WinKind::LethalDamage;
    }
    // CR 704.5a: unbounded life *loss* from an opponent (drain loops report a
    // negative life delta on the victim) is lethal. A life *gain* on the
    // controller is advantage, not a win, so require a strictly-negative life
    // axis on a non-controller player.
    if delta
        .life
        .iter()
        .any(|(pid, &n)| n < 0 && *pid != controller)
    {
        return WinKind::LethalDamage;
    }
    // CR 704.5c: unbounded poison counters on any player. The live path feeds poison
    // via `delta.poison`; the static `candidate_cycles_from_nodes` path feeds it via
    // `delta.counters[(Poison, Player)]` (ability_graph.rs `add_counter`). This is a
    // shared single-authority classifier, so honor BOTH sources — reading only one
    // would silently degrade the other path's poison SCC to `Advantage`.
    if delta.poison.values().any(|&n| n > 0)
        || delta
            .counters
            .get(&(CounterClass::Poison, ObjectClass::Player))
            .is_some_and(|&n| n > 0)
    {
        return WinKind::PoisonLoss;
    }
    // CR 104.3c / CR 121.4: an unbounded *downward* library delta on a player
    // other than the loop's controller is a mill/deck-out win. The controller
    // milling *themselves* is not a win, so require an opponent victim.
    if delta
        .library_delta
        .iter()
        .any(|(pid, &n)| n < 0 && *pid != controller)
    {
        return WinKind::Decking;
    }
    // CR 500.7: an unbounded extra-turns loop wins by never yielding.
    if delta.extra_turns > 0 {
        return WinKind::ExtraTurns;
    }
    // Otherwise: a beneficial advantage engine (mana, tokens, draw, casts,
    // combats, generic triggers, +1/+1 or loyalty counters, death/ETB/LTB/sac
    // engines, or self-mill). The payoff that converts it to a win is a separate
    // card (CR 732.2a beneficial loop).
    WinKind::Advantage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::resource::ResourceVector;
    use crate::game::game_object::GameObject;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaType, ManaUnit};
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

    /// HELIOD + WALKING BALLISTA shape: same board, +1 damage to the opponent and
    /// +1 life to the controller each cycle. The certificate must confirm, name
    /// BOTH the damage and life axes (covers ⊇ {damage(opp)}), and classify
    /// `LethalDamage`. This is the canonical driving combo's expected certificate.
    #[test]
    fn detects_heliod_ballista_lethal_damage() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        // Board returns identical each cycle (the +1/+1 counter is removed then
        // replaced); only damage/life moved.
        let end = start.clone();

        let mut delta = ResourceVector::default();
        delta.damage_dealt.insert(pid(1), 1); // 1 damage to opponent
        delta.life.insert(pid(0), 1); // 1 life gained (lifelink)

        let cert =
            detect_loop(&start, &end, &delta, pid(0), true).expect("net-progress loop confirmed");
        assert_eq!(cert.win_kind, WinKind::LethalDamage);
        assert!(
            cert.covers(&[ResourceAxis::DamageDealt(pid(1))]),
            "certificate must name unbounded damage to the opponent"
        );
        assert!(cert.mandatory, "mandatory flag threaded through");
    }

    /// KILO + FREED + RELIC shape: mana net-zero, board identical, the only
    /// per-cycle progress is +1 proliferate trigger. The certificate must confirm
    /// from a *trigger* axis alone (a mana-only model would miss it) and classify
    /// `Advantage` (the proliferated counters are the eventual payoff, not a direct
    /// loss this cycle).
    #[test]
    fn detects_proliferate_loop_via_trigger_axis() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let end = start.clone();

        let mut delta = ResourceVector::default();
        // Mana net-zero (tapped for U, spent to untap) — no mana axis moves.
        *delta
            .generic_triggers
            .entry(crate::analysis::resource::TriggerKind::Proliferate)
            .or_insert(0) += 1;

        let cert =
            detect_loop(&start, &end, &delta, pid(0), false).expect("trigger-only loop confirmed");
        assert_eq!(cert.win_kind, WinKind::Advantage);
        assert!(
            cert.covers(&[ResourceAxis::Trigger(
                crate::analysis::resource::TriggerKind::Proliferate
            )]),
            "certificate must name the proliferate trigger axis (mana is net-zero)"
        );
        assert!(
            !cert.mandatory,
            "proliferate is an optional {{U}} activation"
        );
    }

    /// A mill loop against the opponent must classify `Decking`, surfacing the
    /// negative library axis on the victim.
    #[test]
    fn detects_opponent_mill_as_decking() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0); // controller has the engine
        let end = start.clone();

        let mut delta = ResourceVector::default();
        delta.library_delta.insert(pid(1), -2); // opponent milled 2 each cycle

        let cert = detect_loop(&start, &end, &delta, pid(0), false).expect("mill loop confirmed");
        assert_eq!(cert.win_kind, WinKind::Decking);
        assert!(cert.covers(&[ResourceAxis::LibraryDelta(pid(1))]));
    }

    /// A pure mana engine (the most common corpus family) classifies `Advantage`,
    /// not a win condition — the payoff is a separate card.
    #[test]
    fn detects_mana_engine_as_advantage() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let end = start.clone();

        let mut delta = ResourceVector::default();
        delta.mana[5] = 1; // +1 colorless each cycle

        let cert = detect_loop(&start, &end, &delta, pid(0), false).expect("mana loop confirmed");
        assert_eq!(cert.win_kind, WinKind::Advantage);
        assert!(cert.covers(&[ResourceAxis::Mana(ManaType::Colorless)]));
    }

    /// An infinite-tokens loop classifies `Advantage`, naming the tokens axis.
    #[test]
    fn detects_token_engine_as_advantage() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let end = start.clone();

        let delta = ResourceVector {
            tokens_created: 1,
            ..Default::default()
        };

        let cert = detect_loop(&start, &end, &delta, pid(0), false).expect("token loop confirmed");
        assert_eq!(cert.win_kind, WinKind::Advantage);
        assert!(cert.covers(&[ResourceAxis::TokensCreated]));
    }

    /// An infinite-poison loop classifies `PoisonLoss`.
    #[test]
    fn detects_poison_loop_as_poison_loss() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let end = start.clone();

        let mut delta = ResourceVector::default();
        // CR 704.5c: poison is now per-victim — discriminates G-9's live `delta.poison` read.
        delta.poison.insert(pid(1), 1);

        let cert = detect_loop(&start, &end, &delta, pid(0), false).expect("poison loop confirmed");
        assert_eq!(cert.win_kind, WinKind::PoisonLoss);
    }

    /// An extra-turns loop classifies `ExtraTurns`.
    #[test]
    fn detects_extra_turns_loop() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let end = start.clone();

        let delta = ResourceVector {
            extra_turns: 1,
            ..Default::default()
        };

        let cert =
            detect_loop(&start, &end, &delta, pid(0), false).expect("extra-turns loop confirmed");
        assert_eq!(cert.win_kind, WinKind::ExtraTurns);
        assert!(cert.covers(&[ResourceAxis::ExtraTurns]));
    }

    // ------------------------------------------------------------------
    // SOUNDNESS — no false positives. These are the revert-probe negatives:
    // each pins one of the two gates (board-equality, net-progress) so that
    // weakening either gate would wrongly emit a certificate.
    // ------------------------------------------------------------------

    /// SOUNDNESS: a genuine board change that is NOT a valid inert object-growth
    /// cover must yield NO certificate even with a positive resource delta.
    /// Reverting BOTH the `loop_states_equal_modulo_resources` gate AND the PR-7
    /// Phase 4a `loop_states_cover_modulo_object_growth` gate would wrongly confirm
    /// this. The extra permanent carries a `+1/+1` counter, so it is SBA-relevant
    /// (CR 704.5f) and therefore not churn-inert — the object-growth cover's
    /// inertness gate (CR 732.2a MAJOR-1) refuses to certify it, exactly as the
    /// constant-depth equality gate always did.
    #[test]
    fn soundness_board_change_yields_no_certificate() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let mut end = start.clone();
        let grown = battlefield_creature(&mut end, 501, 0); // board grew...
        end.objects
            .get_mut(&grown)
            .unwrap()
            .counters
            .insert(crate::types::counter::CounterType::Plus1Plus1, 1); // ...non-inert

        let mut delta = ResourceVector::default();
        delta.damage_dealt.insert(pid(1), 1);

        assert!(
            detect_loop(&start, &end, &delta, pid(0), true).is_none(),
            "a non-inert growing board is not a repeatable loop, even with +damage"
        );
    }

    /// PR-7 Phase 4a (detect_loop wiring, scope item 5): a valid INERT object-growth
    /// loop — the battlefield grows by one unobserved vanilla permanent per cycle
    /// while damage accrues monotonically — is now certified by the OFFLINE
    /// classifier, and the certificate's `residual_board_delta` lights up with the
    /// grown permanent (the fail-loud residual seam). Revert-failing: removing the
    /// `loop_states_cover_modulo_object_growth` arm from `detect_loop` flips this to
    /// `None` (the pre-4a behavior the soundness test above still pins for non-inert
    /// growth).
    #[test]
    fn object_growth_inert_loop_yields_certificate_with_residual() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let mut end = start.clone();
        battlefield_creature(&mut end, 501, 0); // +1 inert vanilla permanent, same class

        let mut delta = ResourceVector::default();
        delta.damage_dealt.insert(pid(1), 1); // monotone opponent-damage progress

        let cert = detect_loop(&start, &end, &delta, pid(0), true)
            .expect("inert object-growth + monotone damage must certify (4a offline cover)");
        assert_eq!(
            cert.residual_board_delta.added.len(),
            1,
            "the grown inert permanent must populate the residual board delta"
        );
        assert!(
            cert.residual_board_delta.removed.is_empty(),
            "pure growth removes nothing from the battlefield"
        );
    }

    /// SOUNDNESS: identical board but a *no-op* resource delta (nothing advanced)
    /// must yield NO certificate. Reverting the `is_net_progress` gate would
    /// wrongly confirm this (an idle pass-priority cycle is not a combo).
    #[test]
    fn soundness_no_progress_yields_no_certificate() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let end = start.clone();

        let delta = ResourceVector::default(); // nothing changed

        assert!(
            detect_loop(&start, &end, &delta, pid(0), true).is_none(),
            "an idle cycle with no resource progress is not a loop"
        );
    }

    /// SOUNDNESS: a cycle that NET-CONSUMES a consumed axis (spends more mana than
    /// it makes) is not sustainable and must yield NO certificate, even though
    /// some gained axis moved. Pins the `is_net_progress` consumed-axis rule.
    #[test]
    fn soundness_net_negative_mana_yields_no_certificate() {
        let mut start = GameState::new_two_player(7);
        let oid = battlefield_creature(&mut start, 500, 0);
        // Float some mana in `start` so `end` can show a net spend.
        start.players[0]
            .mana_pool
            .add(ManaUnit::new(ManaType::Blue, oid, false, Vec::new()));
        let end = start.clone();

        let mut delta = ResourceVector::default();
        delta.mana[1] = -1; // net spent 1 blue
        delta.tokens_created = 1; // ...to make a token

        assert!(
            detect_loop(&start, &end, &delta, pid(0), false).is_none(),
            "a loop that net-loses mana is not infinite, despite making a token"
        );
    }

    /// SOUNDNESS: the controller milling ITSELF is `Advantage` (self-mill engine),
    /// not `Decking` — only an opponent's deckout is a win. Pins the
    /// opponent-victim discrimination in `classify_win_kind`.
    #[test]
    fn self_mill_is_advantage_not_decking() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0); // player 0 controls the engine
        let end = start.clone();

        let mut delta = ResourceVector::default();
        delta.library_delta.insert(pid(0), -2); // player 0 mills THEMSELF

        let cert =
            detect_loop(&start, &end, &delta, pid(0), false).expect("self-mill is still a loop");
        assert_eq!(
            cert.win_kind,
            WinKind::Advantage,
            "milling your own library is advantage, not a deck-out win"
        );
    }

    /// `covers` is a superset test: a certificate naming more axes than expected
    /// still covers, but one missing the expected axis does not.
    #[test]
    fn covers_is_superset_semantics() {
        let cert = LoopCertificate {
            unbounded: vec![
                ResourceAxis::DamageDealt(pid(1)),
                ResourceAxis::Life(pid(0)),
            ],
            win_kind: WinKind::LethalDamage,
            mandatory: true,
            residual_board_delta: BoardDelta::default(),
        };
        assert!(cert.covers(&[ResourceAxis::DamageDealt(pid(1))]));
        assert!(cert.covers(&[
            ResourceAxis::DamageDealt(pid(1)),
            ResourceAxis::Life(pid(0))
        ]));
        assert!(!cert.covers(&[ResourceAxis::Counter(
            CounterClass::Loyalty,
            ObjectClass::Planeswalker
        )]));
    }

    /// T12 (B4 invariant, Drift Correction 2): a real `detect_loop` certificate over the
    /// equal-board frames its `:162` gate guarantees carries an EMPTY residual. Proves
    /// the `board_delta` population seam returns empty on identical battlefields (a diff
    /// that wrongly reported the recycled objects would fail this), keeping the wired
    /// field honest until an object-growth detection path exists.
    #[test]
    fn residual_empty_for_constant_depth() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let end = start.clone(); // identical board (the detect_loop equal-board gate)

        let mut delta = ResourceVector::default();
        delta.mana[5] = 1; // +1 colorless each cycle — a confirmed mana loop

        let cert = detect_loop(&start, &end, &delta, pid(0), false).expect("mana loop confirmed");
        assert_eq!(
            cert.residual_board_delta,
            BoardDelta::default(),
            "an equal-board certificate carries an empty residual (Drift Correction 2)"
        );
    }

    /// FINDING 2 (CR 704.5a): the loop's `controller` is caller-supplied, NOT
    /// inferred from "who has a permanent on the battlefield". Here BOTH players
    /// control a permanent (the old `surviving_controllers` would include P1), but
    /// the drain victim is P1 and the caller passes `controller = P0`, so the
    /// negative life on P1 is an OPPONENT loss => `LethalDamage`.
    ///
    /// LOAD-BEARING PROOF: `classify_win_kind` is reachable here (same module), so
    /// we assert it directly. With the real controller (P0) the P1 life-loss is
    /// lethal; with the VICTIM as controller (P1) the same delta is self-life-loss
    /// and classifies `Advantage`. Reverting the explicit-controller param (back to
    /// battlefield-presence inference, which would include P1) would downgrade the
    /// `LethalDamage` assertion — the differing classification on the same delta is
    /// the discrimination.
    #[test]
    fn detect_loop_finding2_drain_uses_caller_controller_not_board_presence() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0); // P0 controls the engine
        battlefield_creature(&mut start, 600, 1); // P1 ALSO controls a permanent
        let end = start.clone();

        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1); // drain the opponent (P1)

        let cert = detect_loop(&start, &end, &delta, pid(0), true)
            .expect("opponent drain with controller=P0 is a confirmed lethal loop");
        assert_eq!(
            cert.win_kind,
            WinKind::LethalDamage,
            "P1 life-loss with controller=P0 is an opponent loss, not self-advantage"
        );

        // LOAD-BEARING: same delta, victim-as-controller flips the classification.
        assert_eq!(
            classify_win_kind(pid(0), &delta),
            WinKind::LethalDamage,
            "real controller P0: P1 life-loss is lethal"
        );
        assert_eq!(
            classify_win_kind(pid(1), &delta),
            WinKind::Advantage,
            "victim-as-controller P1: own life-loss is not a win => Advantage (param is load-bearing)"
        );
    }

    /// FINDING 2 (CR 104.3c / CR 121.4): mill sibling of the drain test. BOTH
    /// players control a permanent; the milled victim is P1; caller passes
    /// `controller = P0`, so the negative library on P1 is an opponent deck-out =>
    /// `Decking`. Load-bearing: with P1 as controller it is self-mill => `Advantage`.
    #[test]
    fn detect_loop_finding2_mill_uses_caller_controller_not_board_presence() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0); // P0 controls the engine
        battlefield_creature(&mut start, 600, 1); // P1 ALSO controls a permanent
        let end = start.clone();

        let mut delta = ResourceVector::default();
        delta.library_delta.insert(pid(1), -2); // mill the opponent (P1)

        let cert = detect_loop(&start, &end, &delta, pid(0), false)
            .expect("opponent mill with controller=P0 is a confirmed decking loop");
        assert_eq!(cert.win_kind, WinKind::Decking);
        assert!(cert.covers(&[ResourceAxis::LibraryDelta(pid(1))]));

        // LOAD-BEARING: same delta, victim-as-controller is self-mill => Advantage.
        assert_eq!(classify_win_kind(pid(0), &delta), WinKind::Decking);
        assert_eq!(
            classify_win_kind(pid(1), &delta),
            WinKind::Advantage,
            "self-mill (controller == victim) is advantage, not a deck-out win"
        );
    }

    /// FINDING (CR 704.5a): damage dealt to the loop's OWN controller is NOT a
    /// win — a player loses only when *they* reach 0 life, so lethal damage is a
    /// win only against an OPPONENT. A self-ping loop whose controller's life is
    /// offset (lifegain) pumps `damage_dealt[controller]` unbounded but kills no
    /// opponent; it is an advantage engine, mirroring self-mill (`Advantage`, not
    /// `Decking`) and self-life-loss (`Advantage`, not `LethalDamage`).
    ///
    /// DISCRIMINATING: the pre-fix damage branch was
    /// `delta.damage_dealt.values().any(|&n| n > 0)` — controller-blind — so it
    /// classified controller-only damage as `LethalDamage`. The first assertion
    /// (`controller == victim => Advantage`) therefore FAILS against pre-fix code
    /// and PASSES against the fixed `*pid != controller` predicate. The second
    /// assertion (`opponent victim => LethalDamage`) is unchanged by the fix,
    /// proving the change is surgical: it flips only the controller-victim case.
    ///
    /// WELL-FORMEDNESS: `unbounded_components` still surfaces
    /// `DamageDealt(controller)`, so `detect_loop` returns a `Some` certificate
    /// naming >=1 axis with `win_kind == Advantage` (a beneficial CR 732.2a loop),
    /// not `None` and not a panic.
    #[test]
    fn classify_win_kind_controller_only_damage_is_not_lethal() {
        // Controller-only damage (P0 pings ITSELF) => Advantage, NOT LethalDamage.
        let mut self_dmg = ResourceVector::default();
        self_dmg.damage_dealt.insert(pid(0), 1);
        assert_eq!(
            classify_win_kind(pid(0), &self_dmg),
            WinKind::Advantage,
            "damage to the loop's own controller is not a win (CR 704.5a): \
             a player loses only when THEY reach 0 life"
        );

        // Parallel opponent case (P0 controls, P1 is damaged) => still LethalDamage.
        let mut opp_dmg = ResourceVector::default();
        opp_dmg.damage_dealt.insert(pid(1), 1);
        assert_eq!(
            classify_win_kind(pid(0), &opp_dmg),
            WinKind::LethalDamage,
            "unbounded damage to an OPPONENT is still lethal (CR 704.5a)"
        );

        // WELL-FORMEDNESS: the controller-only-damage loop still produces a
        // well-formed certificate (DamageDealt(controller) axis named) classified
        // as the advantage engine it is — not None, not a false direct win.
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let end = start.clone();
        let cert = detect_loop(&start, &end, &self_dmg, pid(0), false)
            .expect("controller-only damage is still a confirmed (advantage) loop");
        assert_eq!(cert.win_kind, WinKind::Advantage);
        assert!(
            cert.covers(&[ResourceAxis::DamageDealt(pid(0))]),
            "certificate names the controller's damage axis (the unbounded resource), \
             but classifies it as Advantage, not a win"
        );
    }

    // ------------------------------------------------------------------
    // live_mandatory_loop_winner (§8): the LIVE reducer coupling. Each test
    // injects a per-cycle delta into a modulo-equal (start == end.clone())
    // state, exactly as the existing detect_loop tests do.
    // ------------------------------------------------------------------

    /// Add a battlefield permanent controlled by `owner` carrying a `mode` static
    /// (CR 101.2 can't-lose / can't-win shape) affecting its controller ("You").
    fn add_cant_static(
        state: &mut GameState,
        owner: u8,
        id: u64,
        mode: crate::types::statics::StaticMode,
    ) {
        use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
        let oid = ObjectId(id);
        let mut object = GameObject::new(
            oid,
            CardId(2),
            PlayerId(owner),
            "Platinum Angel".to_string(),
            Zone::Battlefield,
        );
        object
            .static_definitions
            .push(StaticDefinition::new(mode).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )));
        state.objects.insert(oid, object);
        state.battlefield.push_back(oid);
    }

    /// U1 POSITIVE: a clean single-opponent life-drain names the winner.
    #[test]
    fn live_winner_positive_life_drain() {
        let end = GameState::new_two_player(7);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1); // opponent drains
        delta.life.insert(pid(0), 1); // controller gains
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            Some(pid(0)),
            "a single-opponent forced life-drain shortcuts to the winner"
        );
    }

    /// U2 SOUNDNESS (CR 704.5b): a dual-faller (opponent life ↓ AND a controller
    /// library ↓ — the Niv shape) is a SECOND determinate-loss path ⇒ None.
    /// Revert: dropping `any_library_loss` wrongly yields `Some(P0)`.
    #[test]
    fn live_winner_dual_faller_library_is_none() {
        let end = GameState::new_two_player(7);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.library_delta.insert(pid(0), -1); // controller mills itself too
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "opponent life-loss AND a library-loss is two loss paths — refuse to name a winner"
        );
    }

    /// U3 SOUNDNESS: a mutual drain (both players' life falls) has no single
    /// determinate loser ⇒ None (the single-faller guard rejects, and is_progress
    /// would reject the negative-life winner as a backstop).
    #[test]
    fn live_winner_mutual_drain_is_none() {
        let end = GameState::new_two_player(7);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(0), -1);
        delta.life.insert(pid(1), -1);
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "a mutual drain has no single determinate loser"
        );
    }

    /// U4: pure advantage (mana up, no life faller) is not a forced loss ⇒ None.
    #[test]
    fn live_winner_advantage_no_faller_is_none() {
        let end = GameState::new_two_player(7);
        let start = end.clone();
        let delta = ResourceVector {
            mana: [0, 0, 0, 0, 0, 1],
            ..Default::default()
        };
        assert_eq!(live_mandatory_loop_winner(&start, &end, &delta), None);
    }

    /// U5 SOUNDNESS: a board change at cycle end (extra permanent) is not a
    /// repeating cycle ⇒ None even with a clean life-drain delta. `detect_loop`'s
    /// board-equality gate is load-bearing here.
    #[test]
    fn live_winner_board_change_is_none() {
        let mut end = GameState::new_two_player(7);
        let start = end.clone();
        battlefield_creature(&mut end, 900, 0); // board grew only at end
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.life.insert(pid(0), 1);
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "a growing board is not a repeating cycle (detect_loop rejects)"
        );
    }

    /// U6 SOUNDNESS: a single faller with THREE living players is NOT an all-opponent
    /// drain — a bystander (P2, life delta 0) survives, so `nonfallers = {P0, P2}`
    /// (len 2 ≠ 1) ⇒ None. The MP-general predicate correctly refuses to name a winner
    /// while a non-draining opponent is alive. Revert: an "any-faller-wins" rewrite
    /// that ignores bystanders flips this to `Some(P0)`.
    #[test]
    fn live_winner_three_player_is_none() {
        let mut end = GameState::new_two_player(7);
        let mut p2 = end.players[1].clone();
        p2.id = pid(2);
        end.players.push(p2);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.life.insert(pid(0), 1);
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "a determinate single-loser outcome is unambiguous only in 2-player"
        );
    }

    /// MP COMMANDER SAFETY (the load-bearing firewall): a 4-player table with a single
    /// faller (P1 drains, P0 gains) while P2 and P3 sit STATIC must NOT name a winner.
    /// This is the partial-net-progress drain — only one opponent is draining, the other
    /// two are untouched and alive — so a forced single-loser outcome is NOT determinate
    /// (CR 104.2a is unambiguous only at two living players). The `living.len() != 2`
    /// early-return is what holds the line; commander infinites that drain just one pod
    /// member must not hand the game to P0 while the rest of the table is alive.
    ///
    /// REVERT-FAIL: delete the `living.len() != 2` gate in `live_mandatory_loop_winner`
    /// WITHOUT adding an all-opponents-fall predicate ⇒ the single-faller path names
    /// `Some(P0)` while P2/P3 live ⇒ this assertion flips. (Strengthens the 3-player
    /// `live_winner_three_player_is_none` to the 4-player commander count.)
    #[test]
    fn mp_partial_net_progress_drain_no_premature_gameover() {
        let mut end = GameState::new_two_player(7);
        for seat in 2..=3u8 {
            let mut p = end.players[1].clone();
            p.id = pid(seat);
            end.players.push(p);
        }
        assert_eq!(
            end.players.iter().filter(|p| !p.is_eliminated).count(),
            4,
            "fixture sanity: four living players"
        );
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1); // ONLY P1 drains
        delta.life.insert(pid(0), 1); // P0 gains (the would-be winner)
                                      // P2 and P3 carry no delta entry ⇒ static (map_delta drops zero-delta keys).
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "a 4-player single-faller must not shortcut to a winner while P2/P3 are alive"
        );
    }

    /// T2 (CR 704.5c, G-7 faller generalization): a board-equal cycle whose ONLY loss
    /// axis is rising poison on opponent P1 names P0 the winner — proving the faller
    /// partition now recognizes poison, not just life. Revert-probe: drop the
    /// `|| delta.poison > 0` faller term (G-7) ⇒ fallers = {} ⇒ nonfallers = {P0, P1} ⇒
    /// `Some(pid(0))` flips to `None`.
    #[test]
    fn live_winner_names_poison_faller() {
        let end = GameState::new_two_player(7);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.poison.insert(pid(1), 1);
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            Some(pid(0)),
            "a pure-poison faller (opponent P1) makes P0 the sole non-faller winner"
        );
    }

    /// U7 (CR 704.5a + CR 704.5c): opponent P1 both loses life AND accrues poison — the
    /// SAME player is doomed by either clock, so P1 is a SINGLE faller and P0 is the sole
    /// non-faller winner. This REPLACES the former over-conservative aggregate-poison
    /// firewall (which refused any poison gain because poison was unattributable per
    /// victim); per-victim attribution now correctly names the winner.
    /// INTENDED BEHAVIOR CHANGE (not a regression): the prior assertion was `None`.
    /// Revert-probe: drop the `|| delta.poison > 0` faller term (G-7) ⇒ P1 is no longer a
    /// faller ⇒ nonfallers = {P0, P1} ⇒ None.
    #[test]
    fn live_winner_same_player_life_and_poison_is_determinate() {
        let end = GameState::new_two_player(7);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.poison.insert(pid(1), 1);
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            Some(pid(0)),
            "same-player life-loss AND poison is ONE faller (either clock dooms P1) — P0 wins"
        );
    }

    /// U7b SOUNDNESS (CR 104.2a): when EVERY living player accrues poison, there is no
    /// sole non-faller ⇒ no determinate winner ⇒ None. (Poisoning only ONE player in 2p
    /// hands the win to the other — the interesting refusal needs all seats poisoned.)
    /// Revert-probe: dropping the G-7 poison faller term ⇒ nonfallers = both ⇒ still None,
    /// so this pairs with the positive `same_player` test above to bracket the G-7 term.
    #[test]
    fn live_winner_all_players_poisoned_is_none() {
        let end = GameState::new_two_player(7);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.poison.insert(pid(0), 1);
        delta.poison.insert(pid(1), 1);
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "every living player poisoned ⇒ zero non-fallers ⇒ no determinate winner"
        );
    }

    /// U7c SOUNDNESS (CR 704.3, G-11): a ≥3p pod with two poison-fallers that cross 10 at
    /// DIFFERENT future cycles (equal per-cycle delta but UNEQUAL absolute poison at
    /// cycle_end) must NOT be named a determinate win — the first crossing eliminates one
    /// opponent while the other is still alive (a premature GameOver). Fail-safe None.
    /// Revert-probe: delete the G-11 absolute-poison equality check ⇒ this yields
    /// `Some(pid(0))` (a fail-open premature win).
    #[test]
    fn live_winner_staggered_absolute_poison_is_none() {
        let mut end = n_player(3);
        // Equal per-cycle poison delta (+1 each) but staggered absolute totals: P1 at 3,
        // P2 at 5 ⇒ they cross the 10-poison SBA in different batches.
        end.players[1].poison_counters = 3;
        end.players[2].poison_counters = 5;
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.poison.insert(pid(1), 1);
        delta.poison.insert(pid(2), 1);
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "staggered absolute poison among ≥2 fallers ⇒ non-simultaneous 10-crossing ⇒ None"
        );
    }

    /// U8: PR-3 wins ONLY on the CR 704.5a life axis — a pure opponent mill (no
    /// life faller) is not shortcut here ⇒ None (decking live-shortcut deferred).
    #[test]
    fn live_winner_pure_mill_is_none() {
        let end = GameState::new_two_player(7);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.library_delta.insert(pid(1), -1);
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "PR-3 shortcuts only the life axis; a pure mill has no life faller"
        );
    }

    /// U9 SOUNDNESS (CR 101.2 + CR 104.3b): the faller CAN'T LOSE ⇒ None. Reverting
    /// the `player_has_cant_lose` firewall would end a game P1 cannot lose.
    #[test]
    fn live_winner_faller_cant_lose_is_none() {
        let mut end = GameState::new_two_player(7);
        add_cant_static(
            &mut end,
            1, // permanent controlled by the faller P1, affecting itself
            901,
            crate::types::statics::StaticMode::CantLoseTheGame,
        );
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.life.insert(pid(0), 1);
        assert!(
            crate::game::sba::player_has_cant_lose(&end, pid(1)),
            "fixture sanity: P1 must actually have can't-lose"
        );
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "a forced loss can't be applied to a player who can't lose"
        );
    }

    /// U10 SOUNDNESS (CR 101.2 + CR 104.2b): the winner CAN'T WIN ⇒ None. Reverting
    /// the `player_has_cant_win` firewall would name a winner who cannot win.
    #[test]
    fn live_winner_winner_cant_win_is_none() {
        let mut end = GameState::new_two_player(7);
        add_cant_static(
            &mut end,
            0, // permanent controlled by the winner P0, affecting itself
            902,
            crate::types::statics::StaticMode::CantWinTheGame,
        );
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.life.insert(pid(0), 1);
        assert!(
            crate::game::static_abilities::player_has_cant_win(&end, pid(0)),
            "fixture sanity: P0 must actually have can't-win"
        );
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "a player who can't win must not be named the loop winner"
        );
    }

    /// U-draw: a net-zero cycle (every axis zero) has no life faller ⇒ None. The
    /// modulo path can never hijack a true mandatory-draw (structural complement of
    /// the strict CR 104.4b block, which runs first and returns).
    #[test]
    fn live_winner_net_zero_is_none() {
        let end = GameState::new_two_player(7);
        let start = end.clone();
        let delta = ResourceVector::default();
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "a net-zero repeat is a draw, not a win — no life faller"
        );
    }

    // ===================================================================
    // N2 — MP winner predicate over a growing-cascade covering pair.
    // ===================================================================

    /// A mandatory, no-ordering-input `TriggeredAbility` stack entry (fixed GainLife,
    /// no target/condition) — the churn-kind whose growth `cover_modulo_growth`
    /// certifies. Same source/controller ⇒ one normalized kind.
    fn mtrig(entry_id: u64) -> crate::types::game_state::StackEntry {
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
        use crate::types::game_state::{StackEntry, StackEntryKind};
        let src = ObjectId(500);
        StackEntry {
            id: ObjectId(entry_id),
            source_id: src,
            controller: pid(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: src,
                ability: Box::new(ResolvedAbility::new(
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                    vec![],
                    src,
                    pid(0),
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

    fn n_player(n: u8) -> GameState {
        let mut s = GameState::new_two_player(7);
        while (s.players.len() as u8) < n {
            let mut p = s.players[1].clone();
            p.id = pid(s.players.len() as u8);
            s.players.push(p);
        }
        s
    }

    /// N2 POSITIVE: a 3-player all-opponent drain over a GROWING (covering) stack
    /// names the controller. The growing depth means the exact-depth equality never
    /// matches — only `cover_modulo_growth` can confirm, so the cover path IS
    /// exercised. REVERT-FAIL: restoring `living.len() != 2` ⇒ None (blocker B-WINNER).
    #[test]
    fn n2_three_player_all_opponent_drain_growing_stack() {
        let mut start = n_player(3);
        start.stack.push_back(mtrig(10));
        start.stack.push_back(mtrig(11));
        let mut end = start.clone();
        end.stack.push_back(mtrig(12)); // [G,G] -> [G,G,G]: covering growth
        assert!(
            !loop_states_equal_modulo_resources(&start, &end),
            "fixture: growing depth ⇒ exact-depth equality must NOT match (cover path required)"
        );
        assert!(
            loop_states_cover_modulo_growth(&start, &end),
            "fixture: the covering pair holds"
        );
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.life.insert(pid(2), -1);
        delta.life.insert(pid(0), 2);
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            Some(pid(0)),
            "3p all-opponent drain over a covering pair names the controller"
        );

        // Non-vacuity of the GROWTH arm: with a NON-covering grown stack (the extra
        // entry is a Spell, not a mandatory trigger) the board gate fails ⇒ None.
        let mut end_bad = start.clone();
        end_bad.stack.push_back(spell_entry(99));
        assert_eq!(
            live_mandatory_loop_winner(&start, &end_bad, &delta),
            None,
            "a grown stack that is NOT a covering pair (spell) must not name a winner"
        );

        // Control: the SAME delta at CONSTANT depth confirms via equality — proving
        // the positive above depends on the cover path, not the equality path.
        let flat = start.clone();
        assert_eq!(
            live_mandatory_loop_winner(&start, &flat, &delta),
            Some(pid(0)),
            "constant-depth confirms via the equality path"
        );
    }

    fn spell_entry(entry_id: u64) -> crate::types::game_state::StackEntry {
        use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
        StackEntry {
            id: ObjectId(entry_id),
            source_id: ObjectId(500),
            controller: pid(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        }
    }

    /// N2 HOSTILE: a 4-player table with a STATIC bystander (P3, no life delta) ⇒
    /// non-fallers {P0, P3} (len 2) ⇒ None.
    #[test]
    fn n2_four_player_static_bystander_is_none() {
        let end = n_player(4);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.life.insert(pid(2), -1);
        delta.life.insert(pid(0), 2);
        // P3 carries no delta ⇒ static ⇒ a second non-faller.
        assert_eq!(live_mandatory_loop_winner(&start, &end, &delta), None);
    }

    /// N2 HOSTILE: 3p where the controller ALSO loses ⇒ non-fallers {} ⇒ None (the
    /// CR 104.4a draw road; the strict draw path owns it).
    #[test]
    fn n2_controller_also_falls_is_none() {
        let end = n_player(3);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(0), -1);
        delta.life.insert(pid(1), -1);
        delta.life.insert(pid(2), -1);
        assert_eq!(live_mandatory_loop_winner(&start, &end, &delta), None);
    }

    /// N2 HOSTILE (CR 101.2): 3p all-opponent drain but a faller CAN'T LOSE ⇒ None.
    #[test]
    fn n2_faller_cant_lose_is_none() {
        let mut end = n_player(3);
        add_cant_static(
            &mut end,
            1,
            911,
            crate::types::statics::StaticMode::CantLoseTheGame,
        );
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.life.insert(pid(2), -1);
        delta.life.insert(pid(0), 2);
        assert_eq!(live_mandatory_loop_winner(&start, &end, &delta), None);
    }

    /// N2 HOSTILE (R5-B2, predicate half): 3p all-opponent drain with UNEQUAL faller
    /// deltas over a valid covering pair ⇒ None (the fallers cross lethal in DIFFERENT
    /// resolutions, so the first CR 800.4a elimination is not terminal). REVERT-FAIL:
    /// dropping the `fallers.len() >= 2 ⇒ equal per-cycle delta` conjunct ⇒ Some(P0).
    #[test]
    fn n2_unequal_faller_deltas_is_none() {
        let mut start = n_player(3);
        start.stack.push_back(mtrig(10));
        start.stack.push_back(mtrig(11));
        let mut end = start.clone();
        end.stack.push_back(mtrig(12));
        assert!(
            loop_states_cover_modulo_growth(&start, &end),
            "fixture: the covering pair holds (isolates the simultaneity conjunct)"
        );
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.life.insert(pid(2), -2); // unequal ⇒ staggered crossing
        delta.life.insert(pid(0), 3);
        assert_eq!(live_mandatory_loop_winner(&start, &end, &delta), None);
    }

    /// PR-7 54th (>2p forced-WIN negative control): a Walking Ballista in a 3-player
    /// pod pinging only ONE opponent (P1 drains, P2 static) must NOT declare a winner —
    /// CR 104.2a wins only when ALL opponents have left. fallers = {P1}, nonfallers =
    /// {P0, P2} (len 2), so the `nonfallers.len() != 1` gate returns None. This is the
    /// multiplayer analog of the self-ping control: it guards the "wrongly declares a
    /// win" mode when the unbounded pings are not distributed across the whole table.
    ///
    /// REVERT-FAIL: dropping the `nonfallers.len() != 1` all-opponents-fall gate
    /// (loop_check.rs ~:330) makes winner = nonfallers[0] = P0 pass every downstream
    /// check (the opponent drain IS P0's net-progress axis), flipping this None → a
    /// premature Some(P0). The paired positive below (both opponents fall) proves the
    /// machinery CAN name P0, so this None is attributable to the un-distributed ping,
    /// not a dead pipeline.
    #[test]
    fn ballista_mp_single_opponent_ping_no_false_win() {
        let end = n_player(3);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1); // ONLY P1 is pinged
                                       // P2 carries no delta ⇒ static (a second non-faller).
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "pinging one of two opponents in 3p is not a win (CR 104.2a): the other lives"
        );
    }

    /// PR-7 54th (>2p forced-WIN paired positive): the SAME 3-player pod when the pings
    /// ARE distributed — both opponents fall by an EQUAL, simultaneous −1 each cycle
    /// (P0 offsets +2) — names P0 the winner. fallers = {P1, P2}, nonfallers = {P0}
    /// (len 1), equal per-cycle deltas ⇒ they cross lethal in the same CR 704.3 SBA
    /// batch ⇒ Some(P0). Board-equal via `start == end.clone()` (constant depth). This
    /// pairs with the single-faller None above to make it non-vacuous.
    ///
    /// REVERT-FAIL: dropping the `fallers.len() >= 2 ⇒ equal per-cycle delta`
    /// simultaneity conjunct is probed by `n2_unequal_faller_deltas_is_none`; this test
    /// pins the equal-delta Some(P0) that gate admits.
    #[test]
    fn ballista_mp_all_opponents_distributed_ping_wins() {
        let end = n_player(3);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta.life.insert(pid(2), -1); // equal ⇒ simultaneous crossing
        delta.life.insert(pid(0), 2); // controller lifegain offset
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            Some(pid(0)),
            "a distributed ping that drops every opponent equally names the controller (CR 104.2a)"
        );
    }

    // ===================================================================
    // N5 — m9 monotonicity + R5-B2 per-frame simultaneity (pure fn tests).
    // ===================================================================

    /// Build a state whose players 0..lives.len() carry the given life totals.
    fn frame(lives: &[i32]) -> GameState {
        let mut s = n_player(lives.len() as u8);
        for (i, &l) in lives.iter().enumerate() {
            s.players[i].life = l;
        }
        s
    }

    /// N5: `winner_life_never_dips` — monotone non-decreasing ⇒ true; a
    /// dip-and-recover whose NET delta is ≥ 0 (a net-delta check cannot see it) ⇒
    /// false. REVERT-FAIL: gutting the fn (or dropping its seam call) admits the dip.
    #[test]
    fn n5_winner_life_never_dips() {
        let mono = [frame(&[5]), frame(&[5]), frame(&[7])];
        let mono_refs: Vec<&GameState> = mono.iter().collect();
        assert!(winner_life_never_dips(&mono_refs, pid(0)));

        let dip = [frame(&[5]), frame(&[2]), frame(&[5])];
        let dip_refs: Vec<&GameState> = dip.iter().collect();
        assert!(
            !winner_life_never_dips(&dip_refs, pid(0)),
            "a 5→2→5 intra-window dip (net ≥ 0) must be rejected"
        );
    }

    /// N5: `fallers_lives_pairwise_equal` — two fallers equal at every frame ⇒ true;
    /// diverging lives ⇒ false (the staggered-crossing CR 800.4a machinery-removal
    /// shape). REVERT-FAIL: gutting the fn (or dropping its seam call) admits it.
    #[test]
    fn n5_fallers_lives_pairwise_equal() {
        let equal = [frame(&[20, 10, 10]), frame(&[20, 9, 9]), frame(&[20, 8, 8])];
        let equal_refs: Vec<&GameState> = equal.iter().collect();
        assert!(fallers_lives_pairwise_equal(&equal_refs, &[pid(1), pid(2)]));

        let diverge = [
            frame(&[20, 10, 20]),
            frame(&[20, 9, 19]),
            frame(&[20, 8, 18]),
        ];
        let diverge_refs: Vec<&GameState> = diverge.iter().collect();
        assert!(
            !fallers_lives_pairwise_equal(&diverge_refs, &[pid(1), pid(2)]),
            "P1@10 / P2@20 staggered lives must be rejected"
        );
    }

    /// T-B1i (PR-7 Phase 4d-i discriminator): `detect_loop` MUST reject a
    /// convoke-fodder pair — the offline classifier stays on the object-growth cover
    /// and does NOT adopt the fodder predicate. Non-vacuity pair:
    ///   (1) `detect_loop(...) == None`, AND
    ///   (2) `loop_states_cover_modulo_fodder_growth(...) == true`
    /// so the `None` in (1) is because `detect_loop` does not USE the fodder cover —
    /// not because the frames are un-coverable. Revert-failing: wiring the fodder
    /// predicate into `detect_loop`'s gate-1 (`… || loop_states_cover_modulo_fodder_
    /// growth(...)`) flips (1) to `Some` — gate 2 `net_progress_for` passes on
    /// `tokens_created = 1` and gate 3 `unbounded` is non-empty, so gate 1 is the sole
    /// determinant.
    #[test]
    fn t_b1i_detect_loop_rejects_convoke_fodder_pair() {
        use crate::types::keywords::Keyword;

        fn inert_saproling(state: &mut GameState, id: u64, tapped: bool) -> ObjectId {
            let oid = ObjectId(id);
            let mut o = GameObject::new(
                oid,
                CardId(id),
                PlayerId(0),
                "Saproling".into(),
                Zone::Battlefield,
            );
            o.tapped = tapped;
            state.objects.insert(oid, o);
            state.battlefield.push_back(oid);
            oid
        }

        let mut prior = GameState::new_two_player(7);
        // Inert recast engine (distinct name ⇒ not fodder), constant across frames.
        {
            let eid = ObjectId(800);
            let engine = GameObject::new(
                eid,
                CardId(800),
                PlayerId(0),
                "Engine".into(),
                Zone::Battlefield,
            );
            prior.objects.insert(eid, engine);
            prior.battlefield.push_back(eid);
        }
        inert_saproling(&mut prior, 700, false);
        inert_saproling(&mut prior, 701, false);
        inert_saproling(&mut prior, 702, false);
        inert_saproling(&mut prior, 703, false);
        inert_saproling(&mut prior, 704, true);
        // Convoke recast card in hand — trips the object-growth cost firewall; present
        // in BOTH frames.
        {
            let cid = ObjectId(900);
            let mut card = GameObject::new(
                cid,
                CardId(900),
                PlayerId(0),
                "Sprout Swarm".into(),
                Zone::Hand,
            );
            card.keywords = vec![Keyword::Convoke];
            prior.objects.insert(cid, card);
        }

        let mut current = prior.clone();
        current.objects.get_mut(&ObjectId(700)).unwrap().tapped = true; // tap one untapped
        inert_saproling(&mut current, 705, false); // reproduce one untapped
                                                   // untapped 4→4, tapped 1→2, total 5→6.

        // gates 2+3 pass on tokens_created ⇒ gate 1 is the sole determinant.
        let delta = ResourceVector {
            tokens_created: 1,
            ..Default::default()
        };

        // (1) The offline classifier rejects the convoke-fodder pair.
        assert!(
            detect_loop(&prior, &current, &delta, pid(0), false).is_none(),
            "detect_loop must REJECT a convoke-fodder pair (it stays on the object-growth cover)"
        );
        // (2) The frames ARE a valid fodder cover — proves (1) is not vacuous.
        let saproling_class = GameObject::new(
            ObjectId(999),
            CardId(999),
            PlayerId(0),
            "Saproling".into(),
            Zone::Battlefield,
        );
        assert!(
            crate::analysis::resource::loop_states_cover_modulo_fodder_growth(
                &prior,
                &current,
                &saproling_class,
            ),
            "the frames must be a valid fodder cover (so the None in (1) is non-vacuous)"
        );
    }
}
