//! CR 732.2a loop-shortcut proposal gate — decide whether the priority holder should
//! propose the auto-detected loop shortcut, or return to ordinary priority.
//!
//! ## The defect this closes
//!
//! At `WaitingFor::LoopShortcut` the candidate generator (`engine::ai_support::candidates`)
//! emits BOTH `GameAction::DeclareShortcut` (`TacticalClass::Utility`) and
//! `GameAction::DeclineShortcut` (`TacticalClass::Pass`) and explicitly defers the choice
//! "to the policy/search layer" — but no policy scored either action. `should_play_now_with_facts`
//! (`card_hints.rs`, terminal arm `_ => 0.5`) gives both 0.5, and the class-bonus table
//! (`planner/mod.rs`) then subtracts 0.1 (or 0.25) from `Pass` and nothing from `Utility`.
//! Result: the TACTICAL score preferred `Declare` (0.5) over `Decline` (0.4) in every game state.
//! On every path where the tactical score IS the whole score — the heuristic-only branch
//! (VeryEasy/Easy, `SearchConfig::default()`, ≥5p pods at `<= Medium`) and the deadline-expired
//! tactical floor (`search.rs:1956`) — that is the ENTIRE decision, so an AI holding priority on a
//! loop that a DIFFERENT player wins proposed the shortcut and handed that player the game. Under
//! search the final score is `cont + score * tactical_weight` (`search.rs:1990`), and there the
//! beam's continuation value could already prefer `Decline` on its own (measured control at Hard,
//! policy unregistered: Declare = -9999.925) — but nothing GUARANTEED it. The `Reject` makes the
//! refusal total on every path.
//!
//! ## Why a `TacticalPolicy` and not a picker special-case
//!
//! `PlannerServices::tactical_score` is the single scoring authority consulted by BOTH branches
//! of `score_candidates_core` — the search-ON ranked path and the heuristic-only path — plus the
//! beam interior node and the rollout leaf's priors. A `TacticalPolicy` is therefore
//! difficulty-independent BY CONSTRUCTION: it fires for VeryEasy / Easy (which force
//! `search.enabled = false`), for large pods at `<= Medium`, for the `SearchConfig::default()`
//! `enabled: false`, and for a pre-expired deadline — every path a picker special-case would
//! have to be duplicated into.
//!
//! Note the two branches weight this policy DIFFERENTLY. The search-ON branch multiplies the
//! tactical score by `tactical_weight` — `0.1` at a quiesced node, or `0.35` if an opponent's
//! object is on the stack (the offer is raised at a priority window, which does not imply an
//! empty stack); target-selection's `0.7` is unreachable here. The heuristic-only branch adds the
//! score RAW. So the positive win bonus is attenuated up to 10x under search — deliberately,
//! because there the beam's own continuation value already sees the crown. The REJECT is immune
//! to the weight AND the temperature: `-inf * 0.1 == -inf * 0.35 == -inf`, and `exp(-inf / T) == 0`
//! at every temperature, so no difficulty ever throws the game away.
//!
//! ## The three rulings the verdict encodes
//!
//! 1. `predicted_winner == Some(w), w != proposer` + `UntilLethal` ⇒ REJECT.
//!    `loop_check::live_mandatory_loop_winner` partitions the living players into `fallers`
//!    (per-cycle `delta.life < 0 || delta.poison > 0`) and `nonfallers`, and names a winner ONLY
//!    when `nonfallers.len() == 1`. The two sets partition `living`, so a named winner other than
//!    the proposer PROVES the proposer is a faller — a deterministic self-loss (CR 704.5a life /
//!    CR 704.5c poison) with the other player crowned by CR 104.2a. The declare is WEAKLY
//!    DOMINATED, not merely bad: the drive either reaches that crown, or aborts into
//!    `until_lethal_fallback`, which restores exactly the board a decline would have left.
//!    Loss-or-no-op — never a gain.
//! 2. `predicted_winner == None` + `UntilLethal` ⇒ REJECT.
//!    Only the object-growth offer carries `None`. The crown gate requires
//!    `Some(winner) == proposal.predicted_winner`, false for every winner when the latch is `None`
//!    ⇒ `until_lethal_fallback` full-rolls-back the board, clears `loop_detect_ring` +
//!    `last_recast_context`, and hands priority back. Zero progress, the CR 732.2b APNAP window
//!    burned, the re-offer signal destroyed — weakly dominated by declining.
//! 3. `predicted_winner == Some(proposer)` + `UntilLethal` ⇒ positive (critical band).
//!    The crown IS the win (CR 104.2a); the only other outcome is `until_lethal_fallback`, which
//!    lands where declining lands. Dominant in outcome. (It is not costless: declaring opens the
//!    CR 732.2b window, and an opponent's `Shorten` hands THEM a priority window they would not
//!    get if the AI declined and kept priority. The crown is worth that risk; the phrase "no
//!    downside" would not be true.)
//!
//! ## Why the `IterationCount` gate is load-bearing for the CLASS
//!
//! `materialize_fixed_shortcut` NEVER consults `predicted_winner`: it drives `n` whole cycles and
//! COMMITS each atomically (an object-growth `None` offer is routed to
//! `materialize_object_growth_shortcut`). A `Fixed(n)` declare is real, committed board progress
//! needing no crown — so a reject that ignored the count would be wrong for the class. Today's AI
//! candidate generator only ever emits `UntilLethal`, but `Fixed(n)` is reachable through the
//! public `GameAction` surface: `handle_declare_shortcut` moves `count` into the proposal with
//! ZERO validation (the fail-closed firewall validates only `template` pins, and is skipped
//! entirely when `template` is `None`).
//!
//! ## Why the verdict reads `proposer` from the state, never `ctx.ai_player`
//!
//! `WaitingFor::acting_player()` returns `Some(proposer)` for `LoopShortcut`, and the beam/rollout
//! paths bind `PolicyContext::ai_player` to exactly that, so the two coincide wherever both are
//! defined. Reading `proposer` is fail-safe: the candidate is the proposer's action by
//! construction (`metadata.actor == Some(proposer)`), so the veto stays correct even if a caller
//! ever scores this state under a different seat's value lens — whereas gating on
//! `ctx.ai_player == proposer` would silently DROP the veto in that case.

use engine::analysis::decision_template::IterationCount;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct LoopShortcutPolicy;

impl TacticalPolicy for LoopShortcutPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::LoopShortcut
    }

    /// `classify_decision` maps `WaitingFor::LoopShortcut` to `ActivateAbility` BEFORE inspecting
    /// the action, so this single kind reaches both the `DeclareShortcut` and the
    /// `DeclineShortcut` candidate.
    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // A hard-veto backstop for the CR 732.2a shortcut protocol: a pure state-machine policy
        // with no deck signal (mirrors `XCastGatePolicy` / `SelfCostValuePolicy`). `verdict`
        // short-circuits on one enum-discriminant compare for every non-shortcut candidate, so the
        // unconditional activation costs nothing in the search inner loop.
        // activation-constant: unconditional Reject backstop; all gating lives in `verdict`.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || PolicyVerdict::neutral(PolicyReason::new("loop_shortcut_na"));

        // Cheapest possible gate FIRST (one enum-discriminant compare): every `ActivateAbility`
        // candidate in the game runs this. It is also this policy's contribution to NaN safety —
        // `DeclineShortcut` exits here, so the policy can never reject BOTH candidates.
        // (`softmax_select_pairs` also self-heals on an all-`-inf` vector via its
        // `!total.is_finite()` argmax fallback — belt and braces.)
        let GameAction::DeclareShortcut { count, .. } = &ctx.candidate.action else {
            return na();
        };
        let WaitingFor::LoopShortcut {
            proposer,
            predicted_winner,
            ..
        } = &ctx.state.waiting_for
        else {
            return na();
        };

        match (predicted_winner, count) {
            // CR 704.5a / CR 704.5c + CR 104.2a: the offer's winner is somebody else. The
            // faller/non-faller partition in `loop_check::live_mandatory_loop_winner` is total over
            // the living players, so a named winner != proposer PROVES the proposer's per-cycle
            // life/poison delta is a loss. Declaring `UntilLethal` runs that loop to the SBA and
            // crowns the other player. WEAKLY DOMINATED: the alternative branch is
            // `until_lethal_fallback` (a rollback to exactly where a decline lands), so the
            // outcome set is {self-loss, no-op} — never a gain.
            (Some(winner), IterationCount::UntilLethal) if winner != proposer => {
                PolicyVerdict::reject(
                    PolicyReason::new("loop_shortcut_declare_hands_opponent_the_win")
                        .with_fact("proposer", i64::from(proposer.0))
                        .with_fact("predicted_winner", i64::from(winner.0)),
                )
            }

            // CR 732.2a + CR 104.2a: the crown gate requires `Some(winner) == predicted_winner`, so
            // an `UntilLethal` declare on an offer whose latched winner IS the proposer is the win.
            // Worst case the gate refuses and `until_lethal_fallback` restores the pre-drive board.
            // Game-deciding ⇒ critical band (`PolicyVerdict::score` auto-bands the config-routed
            // value).
            (Some(winner), IterationCount::UntilLethal) => PolicyVerdict::score(
                ctx.penalties().loop_shortcut_winning_declare_bonus,
                PolicyReason::new("loop_shortcut_declare_wins")
                    .with_fact("winner", i64::from(winner.0)),
            ),

            // CR 732.2a: only the object-growth offer latches `None`. The crown gate can never
            // match it, so `UntilLethal` always ends in `until_lethal_fallback`: full board
            // rollback, `loop_detect_ring` + `last_recast_context` cleared, priority handed back.
            // Zero progress AND the CR 732.2b response window spent — weakly dominated by
            // declining.
            (None, IterationCount::UntilLethal) => {
                PolicyVerdict::reject(PolicyReason::new("loop_shortcut_untillethal_cannot_crown"))
            }

            // CR 732.2a "a loop that repeats a specified number of times": neither rejected nor
            // boosted. Two independent reasons. (1) The AI never emits `Fixed` — its ONLY
            // `DeclareShortcut` construction site (`candidates.rs:3012`) hardcodes `UntilLethal`.
            // (2) A count-blind reject would be wrong for the CLASS: `materialize_fixed_shortcut`
            // drives and COMMITS `n` whole cycles without ever reading `predicted_winner`, so a
            // small-`n` `Fixed` is genuine committed board progress whoever is latched.
            //
            // NOTE (tripwire): a `Fixed(n)` large enough to cross lethal WOULD commit a `GameOver`
            // crowning whoever the DRIVE's state-based actions crown (`drive_one_shortcut_cycle`,
            // `engine.rs:1180-1186`, forwards the SBA's own `Option<PlayerId>` — which is the
            // latched winner when the prediction was right, and can even be `None`, a CR 104.4b
            // draw). `materialize_fixed_shortcut`'s `CrossLethal` arm (`engine.rs:1393-1402`)
            // forwards it WITHOUT filtering on `proposal.predicted_winner`, unlike BOTH
            // `UntilLethal` crown gates (`engine.rs:971`, `engine.rs:1000`). Such a declare by a
            // faller proposer is therefore a committed self-loss — exactly what the `UntilLethal`
            // arm above rejects. REVISIT THIS ARM if the candidate generator ever emits `Fixed`.
            (_, IterationCount::Fixed(_)) => na(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{create_config, AiConfig, AiDifficulty, Platform};
    use crate::policies::context::SearchDepth;
    use crate::policies::registry::STRONG_MAX;
    use crate::search::{choose_action, score_candidates_with_session};
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::analysis::decision_template::ShortcutDecisionSchema;
    use engine::analysis::loop_check::{LoopCertificate, WinKind};
    use engine::analysis::resource::BoardDelta;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    /// A synthetic optional-lethal certificate — the policy never reads it; only `proposer` and
    /// `predicted_winner` drive the verdict.
    fn cert() -> LoopCertificate {
        LoopCertificate {
            unbounded: vec![],
            win_kind: WinKind::LethalDamage,
            mandatory: false,
            residual_board_delta: BoardDelta::default(),
        }
    }

    fn offer_state(predicted_winner: Option<PlayerId>) -> GameState {
        let mut state = GameState::new_two_player(0);
        state.waiting_for = WaitingFor::LoopShortcut {
            proposer: P0,
            predicted_winner,
            certificate: cert(),
            schema: ShortcutDecisionSchema::default(),
        };
        state
    }

    fn declare(count: IterationCount) -> CandidateAction {
        CandidateAction {
            action: GameAction::DeclareShortcut {
                count,
                template: None,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Utility),
        }
    }

    fn decline() -> CandidateAction {
        CandidateAction {
            action: GameAction::DeclineShortcut,
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Pass),
        }
    }

    fn verdict_for(state: &GameState, candidate: &CandidateAction) -> PolicyVerdict {
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![declare(IterationCount::UntilLethal), decline()],
        };
        let context = crate::context::AiContext::empty(&config.weights);
        LoopShortcutPolicy.verdict(&PolicyContext {
            state,
            decision: &decision,
            candidate,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: SearchDepth::Root,
        })
    }

    fn delta_of(v: &PolicyVerdict) -> f64 {
        match v {
            PolicyVerdict::Score { delta, .. } => *delta,
            PolicyVerdict::Reject { reason } => {
                panic!("expected Score, got Reject {}", reason.kind)
            }
        }
    }

    fn kind_of(v: &PolicyVerdict) -> &'static str {
        match v {
            PolicyVerdict::Score { reason, .. } | PolicyVerdict::Reject { reason } => reason.kind,
        }
    }

    /// Reach-guard: prove the synthetic offer state actually reaches the SCORER with BOTH
    /// candidates alive. `validate_candidates` simulates both shortcut actions, and a 1-element
    /// survivor set makes `deterministic_choice` (`search.rs:2127-2129`, `if actions.len() == 1`)
    /// short-circuit the scorer entirely — which would make the e2e tests below pass with the
    /// policy deleted. Exactly 2 scored entries is the only proof that neither happened.
    fn assert_both_candidates_reach_the_scorer(state: &GameState, config: &AiConfig) {
        let session = AiSession::arc_from_game(state);
        let scored = score_candidates_with_session(state, P0, config, &session);
        assert_eq!(
            scored.len(),
            2,
            "both shortcut candidates must survive validate_candidates + gate_candidates and reach \
             the scorer (1 entry ⇒ deterministic_choice short-circuited ⇒ the test would be \
             VACUOUS); got {scored:?}"
        );
        assert!(
            scored
                .iter()
                .any(|(a, _)| matches!(a, GameAction::DeclareShortcut { .. })),
            "DeclareShortcut must reach the scorer; got {scored:?}"
        );
        assert!(
            scored
                .iter()
                .any(|(a, _)| matches!(a, GameAction::DeclineShortcut)),
            "DeclineShortcut must reach the scorer; got {scored:?}"
        );
    }

    /// The policy has NO `DeckFeatures` axis (it is a pure state-machine veto), so
    /// `/add-ai-feature-policy`'s `activation_opts_out_below_floor` convention does not apply.
    /// Non-vacuous: returning `None` silently disables the whole policy, and this fails on `None`.
    #[test]
    fn activation_is_an_unconditional_backstop() {
        assert_eq!(
            LoopShortcutPolicy.activation(&DeckFeatures::default(), &offer_state(Some(P1)), P0),
            Some(1.0)
        );
    }

    /// Rows 0/1 — the NaN-safety lock: `DeclineShortcut` is NEVER rejected, so the softmax always
    /// sees at least one finite weight.
    #[test]
    fn decline_shortcut_is_never_rejected() {
        let state = offer_state(Some(P1));
        let v = verdict_for(&state, &decline());
        assert_eq!(delta_of(&v), 0.0);
        assert_eq!(kind_of(&v), "loop_shortcut_na");
    }

    /// Row 0 — a `DeclareShortcut` candidate scored at a NON-`LoopShortcut` state is neutral.
    #[test]
    fn declare_outside_loop_shortcut_returns_zero() {
        let mut state = GameState::new_two_player(0);
        state.waiting_for = WaitingFor::Priority { player: P0 };
        let v = verdict_for(&state, &declare(IterationCount::UntilLethal));
        assert_eq!(delta_of(&v), 0.0);
        assert_eq!(kind_of(&v), "loop_shortcut_na");
    }

    /// Row 2 — THE BUG. The latched winner is an opponent ⇒ declaring `UntilLethal` runs a loop
    /// whose SBA crowns them (CR 104.2a) and kills the proposer (CR 704.5a).
    #[test]
    fn declare_until_lethal_when_opponent_wins_is_rejected() {
        let state = offer_state(Some(P1));
        let v = verdict_for(&state, &declare(IterationCount::UntilLethal));
        assert!(
            matches!(v, PolicyVerdict::Reject { .. }),
            "declaring a shortcut an OPPONENT wins must be vetoed, got {v:?}"
        );
        assert_eq!(kind_of(&v), "loop_shortcut_declare_hands_opponent_the_win");
    }

    /// Row 6 — the over-suppression guard: the proposer's OWN win must still be boosted.
    #[test]
    fn declare_until_lethal_when_self_wins_is_critical() {
        let state = offer_state(Some(P0));
        let v = verdict_for(&state, &declare(IterationCount::UntilLethal));
        assert!(
            delta_of(&v) > STRONG_MAX,
            "a guaranteed CR 104.2a crown is critical-band, got {v:?}"
        );
        assert_eq!(kind_of(&v), "loop_shortcut_declare_wins");
    }

    /// Row 4 — an `UntilLethal` declare on a `None`-winner (object-growth) offer can never satisfy
    /// the crown gate, so it always full-rolls-back: zero progress, CR 732.2b window burned.
    #[test]
    fn declare_until_lethal_with_no_predicted_winner_is_rejected() {
        let state = offer_state(None);
        let v = verdict_for(&state, &declare(IterationCount::UntilLethal));
        assert!(
            matches!(v, PolicyVerdict::Reject { .. }),
            "an UntilLethal declare that cannot crown must be vetoed, got {v:?}"
        );
        assert_eq!(kind_of(&v), "loop_shortcut_untillethal_cannot_crown");
    }

    /// Rows 3 + 5 + 7 — THE CLASS GUARD: `materialize_fixed_shortcut` never reads
    /// `predicted_winner` and COMMITS every cycle it drives, so a `Fixed(n)` declare is real board
    /// progress for ANY latched winner. Proves the reject set is not one state too wide.
    #[test]
    fn declare_fixed_is_never_rejected() {
        for predicted_winner in [None, Some(P0), Some(P1)] {
            let state = offer_state(predicted_winner);
            let v = verdict_for(&state, &declare(IterationCount::Fixed(3)));
            assert_eq!(
                delta_of(&v),
                0.0,
                "Fixed(n) is committed progress, never vetoed nor boosted (winner \
                 {predicted_winner:?})"
            );
            assert_eq!(kind_of(&v), "loop_shortcut_na");
        }
    }

    /// E2E, HEURISTIC branch (VeryEasy: `search.enabled == false` ⇒ the tactical score is added
    /// RAW). Without the policy the class-bonus table makes Declare (0.5) beat Decline (0.4) in
    /// every state; with it the `Reject` drives Declare's softmax weight to `exp(-inf/T) == 0`, so
    /// the pick is deterministic across every seed.
    #[test]
    fn heuristic_picker_declines_a_losing_shortcut() {
        let config = create_config(AiDifficulty::VeryEasy, Platform::Native);
        assert!(
            !config.search.enabled,
            "reach-guard: VeryEasy must exercise the HEURISTIC branch"
        );
        let state = offer_state(Some(P1));
        assert_both_candidates_reach_the_scorer(&state, &config);

        for seed in 0..32u64 {
            let mut rng = SmallRng::seed_from_u64(seed);
            assert_eq!(
                choose_action(&state, P0, &config, &mut rng),
                Some(GameAction::DeclineShortcut),
                "seed {seed}: the AI must NOT propose a shortcut that crowns its opponent"
            );
        }
    }

    /// E2E, SEARCH branch (Hard). The TEETH are the two RNG-free SCORE assertions: the `-inf` must
    /// survive the `tactical_weight` multiply (`-inf * 0.1 == -inf * 0.35 == -inf`).
    ///
    /// # ⚠️ DO NOT "SIMPLIFY" THIS DOWN TO THE SEED LOOP.
    ///
    /// The seed loop at the end is a POST-CONDITION INVARIANT (necessary, NOT discriminating). With
    /// the policy unregistered the control Declare score here is a FINITE `-9999.925` (measured) —
    /// which still loses the softmax to Decline — so a pick-only assertion PASSES WITH THE POLICY
    /// DELETED. The only assertion with teeth is
    /// `declare_score.is_infinite() && declare_score.is_sign_negative()`: that is what proves the
    /// `Reject` (and not the beam's own continuation value) is doing the work.
    #[test]
    fn search_picker_declines_a_losing_shortcut() {
        let mut config = create_config(AiDifficulty::Hard, Platform::Native);
        assert!(
            config.search.enabled,
            "reach-guard: Hard must exercise the SEARCH branch"
        );
        // The verdict is state-only, so K determinization samples add nothing; 0 keeps the test on
        // the deterministic core path.
        config.search.determinization_samples = 0;
        let state = offer_state(Some(P1));
        assert_both_candidates_reach_the_scorer(&state, &config);

        let session = AiSession::arc_from_game(&state);
        let scored = score_candidates_with_session(&state, P0, &config, &session);
        let declare_score = scored
            .iter()
            .find(|(a, _)| matches!(a, GameAction::DeclareShortcut { .. }))
            .map(|(_, s)| *s)
            .expect("DeclareShortcut is scored");
        let decline_score = scored
            .iter()
            .find(|(a, _)| matches!(a, GameAction::DeclineShortcut))
            .map(|(_, s)| *s)
            .expect("DeclineShortcut is scored");
        assert!(
            declare_score.is_infinite() && declare_score.is_sign_negative(),
            "the Reject must survive the tactical_weight multiply, got {declare_score}"
        );
        assert!(
            decline_score.is_finite(),
            "DeclineShortcut must stay finite (NaN safety), got {decline_score}"
        );

        // POST-CONDITION INVARIANT (necessary, NOT discriminating — see the doc comment): the
        // `-inf` must actually reach the picker. This loop passes with the policy deleted (the
        // control Declare score is a finite -9999.925, which also loses the softmax), so it proves
        // the end-to-end wiring, never the policy.
        for seed in 0..8u64 {
            let mut rng = SmallRng::seed_from_u64(seed);
            assert_eq!(
                choose_action(&state, P0, &config, &mut rng),
                Some(GameAction::DeclineShortcut),
                "seed {seed}: the search picker must decline too"
            );
        }
    }

    /// The over-suppression e2e guard. VeryEasy (heuristic ⇒ UNWEIGHTED) is the branch the `8.0`
    /// default is sized for. Do NOT rewrite this against a search-ON difficulty: there the margin
    /// is `8.1 * 0.1 = 0.81` (or `2.835` at `w = 0.35`), both `< STRONG_MAX`, and the assertion
    /// would be false for a CORRECT implementation.
    #[test]
    fn winning_shortcut_is_still_declared() {
        let config = create_config(AiDifficulty::VeryEasy, Platform::Native);
        let state = offer_state(Some(P0));
        assert_both_candidates_reach_the_scorer(&state, &config);

        let session = AiSession::arc_from_game(&state);
        let scored = score_candidates_with_session(&state, P0, &config, &session);
        let declare_score = scored
            .iter()
            .find(|(a, _)| matches!(a, GameAction::DeclareShortcut { .. }))
            .map(|(_, s)| *s)
            .expect("DeclareShortcut is scored");
        let decline_score = scored
            .iter()
            .find(|(a, _)| matches!(a, GameAction::DeclineShortcut))
            .map(|(_, s)| *s)
            .expect("DeclineShortcut is scored");
        assert!(
            declare_score - decline_score > STRONG_MAX,
            "a shortcut the proposer WINS must stay strongly preferred; declare = \
             {declare_score}, decline = {decline_score}"
        );
    }
}
