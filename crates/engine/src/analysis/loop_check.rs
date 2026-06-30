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

use crate::analysis::resource::{
    loop_states_equal_modulo_resources, CounterClass, ObjectClass, ResourceAxis, ResourceVector,
};
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
/// value — never stored on `GameState`, never serialized into game flow; PR-3 is
/// what (later) acts on an equivalent live signal.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    // modulo the monotone resources — otherwise this is not a repeatable cycle.
    if !loop_states_equal_modulo_resources(cycle_start, cycle_end) {
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
    // CR 104.2a: a forced single-loser outcome is unambiguous only in a 2-player
    // game; multiplayer player-elimination is deferred (offline-covered by PR-2).
    let living: Vec<PlayerId> = cycle_end
        .players
        .iter()
        .filter(|p| !p.is_eliminated)
        .map(|p| p.id)
        .collect();
    if living.len() != 2 {
        return None;
    }

    // CR 704.5a: the per-player attributable life axis — who is draining out.
    let life_fallers: Vec<PlayerId> = living
        .iter()
        .copied()
        .filter(|p| delta.life.get(p).copied().unwrap_or(0) < 0)
        .collect();
    // CR 704.5b: any decking (library going down) is a SECOND determinate-loss path.
    let any_library_loss = living
        .iter()
        .any(|p| delta.library_delta.get(p).copied().unwrap_or(0) < 0);
    // CR 704.5c: poison is keyed by an aggregate (Poison, Player) pair in `snapshot`
    // (unattributable per player), so treat any poison gain conservatively.
    let any_poison_gain = delta
        .counters
        .get(&(CounterClass::Poison, ObjectClass::Player))
        .copied()
        .unwrap_or(0)
        > 0;
    // Single-faller firewall: reject dual-faller (mutual drain), the Niv shape
    // (opponent life ↓ AND a controller library ↓), and any second determinate-loss
    // path. PR-3 wins ONLY on the CR 704.5a life axis.
    if life_fallers.len() != 1 || any_library_loss || any_poison_gain {
        return None;
    }
    let faller = life_fallers[0];
    let winner = living.iter().copied().find(|&p| p != faller)?;

    // CR 101.2 + CR 104.3b + CR 704.5a: a player who can't lose the game can't be the
    // faller of a forced loss (Platinum Angel / "you can't lose the game"). CR 101.2 +
    // CR 104.2b: a player who can't win can't be named winner (Abyssal Persecutor:
    // "your opponents can't win the game"). Reuse the SBA-layer predicates on the LIVE
    // `cycle_end` so static effects are evaluated against the real board. (This
    // firewall is strict 2-player — the Two-Headed Giant team rule CR 810.8a does not
    // apply here.)
    if crate::game::sba::player_has_cant_lose(cycle_end, faller)
        || crate::game::static_abilities::player_has_cant_win(cycle_end, winner)
    {
        return None;
    }

    // Confirm via the PR-2 classifier: `detect_loop` re-runs
    // `loop_states_equal_modulo_resources` (so the board-equality gate is enforced
    // here, no redundant pre-check) and `is_progress`. Holds by construction —
    // `is_progress` passes (winner life Δ≥0), and `classify_win_kind` sees `faller`
    // life<0 with faller≠winner ⇒ `LethalDamage`. Require exactly that win kind so the
    // live shortcut is scoped to the CR 704.5a life axis.
    let cert = detect_loop(cycle_start, cycle_end, delta, winner, true)?;
    matches!(cert.win_kind, WinKind::LethalDamage).then_some(winner)
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
    // CR 704.5c: unbounded poison counters on a player.
    if delta.counters.iter().any(|(&(class, who), &n)| {
        class == CounterClass::Poison && who == ObjectClass::Player && n > 0
    }) {
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
        delta
            .counters
            .insert((CounterClass::Poison, ObjectClass::Player), 1);

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

    /// SOUNDNESS: a genuine board change (an extra permanent at cycle end) must
    /// yield NO certificate even with a positive resource delta. Reverting the
    /// `loop_states_equal_modulo_resources` gate would wrongly confirm this.
    #[test]
    fn soundness_board_change_yields_no_certificate() {
        let mut start = GameState::new_two_player(7);
        battlefield_creature(&mut start, 500, 0);
        let mut end = start.clone();
        battlefield_creature(&mut end, 501, 0); // board grew — not a repeating cycle

        let mut delta = ResourceVector::default();
        delta.damage_dealt.insert(pid(1), 1);

        assert!(
            detect_loop(&start, &end, &delta, pid(0), true).is_none(),
            "a growing board is not a repeatable loop, even with +damage"
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

    /// U6 SOUNDNESS: a single faller but THREE living players ⇒ None (2-player
    /// scope only; multiplayer deferred). Revert: dropping `living.len()==2` yields
    /// a winner.
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

    /// U7 SOUNDNESS (CR 704.5c): opponent life ↓ AND a poison gain is a SECOND
    /// (unattributable) loss path ⇒ None. Revert: dropping `any_poison_gain`
    /// wrongly yields `Some(P0)`.
    #[test]
    fn live_winner_dual_faller_poison_is_none() {
        let end = GameState::new_two_player(7);
        let start = end.clone();
        let mut delta = ResourceVector::default();
        delta.life.insert(pid(1), -1);
        delta
            .counters
            .insert((CounterClass::Poison, ObjectClass::Player), 1);
        assert_eq!(
            live_mandatory_loop_winner(&start, &end, &delta),
            None,
            "opponent life-loss AND poison gain is two loss paths — refuse to name a winner"
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
}
