//! Engine A — the **dynamic loop-confirmation** entry point.
//!
//! PR-0 gave the [`ResourceVector`] (the monotone axes a loop can pump) and
//! [`loop_states_equal_modulo_resources`] (board/zones/tap-state equal, resources
//! allowed to differ). PR-1 gave [`crate::analysis::LoopProbe`], which drives a
//! `GameRunner` and measures the per-iteration [`ResourceVector`] delta. This
//! module is the classifier that turns those two measurements into a
//! [`LoopCertificate`].
//!
//! # What "detection" means here (and what it does NOT)
//!
//! This is **purely offline analysis**. It changes no game behavior: the live
//! resolution loop (`game::engine::run_auto_pass_loop`) still draws a repeating
//! *mandatory* loop (CR 104.4b / CR 732.4) and still halts a runaway cascade
//! (`emit_resolution_halt`) exactly as before. [`detect_loop`] is never called
//! from the reducer; it is called by analysis code and the corpus test harness on
//! a *driven* `GameRunner` to answer the question the engine's live path cannot:
//! "given that the board returned to an identical configuration while a resource
//! strictly increased, what resource is unbounded and how does this loop win?"
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
    // nothing goes unbounded. This is controller-aware (see `is_progress`):
    // PR-0's `ResourceVector::is_net_progress` treats *any* player's life/mana
    // going negative as disqualifying, which is correct for a self-sustainability
    // question but wrongly rejects a damage/drain/mill loop whose entire point is
    // to drive an OPPONENT's life or library down. The caller supplies the loop's
    // `controller`, so the consumed-axis constraint is scoped to that player and
    // opponent depletion is treated as progress.
    if !is_progress(delta, controller) {
        return None;
    }

    let unbounded = unbounded_axes_for(delta, controller);
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

/// CR 732.2a: controller-scoped net-progress. Returns true iff the cycle makes
/// unbounded progress on ≥1 axis without leaving the loop's controller(s) with an
/// unsustainable net deficit on a *consumed* axis (their own life or mana).
///
/// Distinct from [`ResourceVector::is_net_progress`] (PR-0) only in *who* the
/// consumed-axis constraint applies to:
/// - **Controller life/mana net-negative ⇒ not sustainable ⇒ false** (a loop that
///   bleeds its own controller stops on its own).
/// - **Opponent life net-negative ⇒ progress** (the drain/damage win). Opponent
///   library net-negative ⇒ progress (the mill win).
/// - All other axes (damage, tokens, draws, casts, counters, triggers, combats,
///   turns, the controller's gained mana) count as progress when strictly up.
fn is_progress(delta: &ResourceVector, controller: PlayerId) -> bool {
    // CR 106.1: a loop that net-spends mana across the whole pool is not
    // sustainable. Mana is not attributed per player in the summed `mana` array,
    // so any net-negative color is a controller-side deficit.
    if delta.mana.iter().any(|&n| n < 0) {
        return false;
    }
    // CR 119: the controller losing life across the cycle is unsustainable.
    for (pid, &n) in &delta.life {
        if *pid == controller && n < 0 {
            return false;
        }
    }
    !unbounded_axes_for(delta, controller).is_empty()
}

/// The unbounded axes of `delta`, with the opponent-vs-controller sign rules a
/// win classifier needs. Builds on [`ResourceVector::unbounded_components`] (which
/// reports every strictly-positive axis plus any nonzero library) and additionally
/// surfaces an **opponent's life loss** (negative life on a non-controller) as the
/// drain win axis — `unbounded_components` only reports positive life (lifegain),
/// so a pure drain loop would otherwise name no axis.
fn unbounded_axes_for(delta: &ResourceVector, controller: PlayerId) -> Vec<ResourceAxis> {
    let mut out: Vec<ResourceAxis> = delta
        .unbounded_components()
        .into_iter()
        .map(|(axis, _)| axis)
        .collect();
    // CR 704.5a: an opponent's life driven *down* each cycle is the drain win.
    for (pid, &n) in &delta.life {
        if n < 0 && *pid != controller {
            let axis = ResourceAxis::Life(*pid);
            if !out.contains(&axis) {
                out.push(axis);
            }
        }
    }
    out
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
fn classify_win_kind(controller: PlayerId, delta: &ResourceVector) -> WinKind {
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
}
