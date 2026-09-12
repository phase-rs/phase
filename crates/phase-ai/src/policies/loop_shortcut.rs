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
//! `materialize_fixed_shortcut` drives `n` whole cycles and COMMITS each atomically (an
//! object-growth `None` offer is routed to `materialize_object_growth_shortcut`). It reads
//! `predicted_winner` at ONE point only — its cross-lethal arm refuses to crown a verdict that
//! name contradicts — so a count crossing nothing crowns nobody, and a `Fixed(n)` declare is real,
//! committed board progress needing no crown whoever is latched. A reject that ignored the count
//! would therefore be wrong for the class. The AI candidate generator itself proposes
//! `Fixed(max_iterations)` against a bounded offer, and offers `UntilLethal` only against an offer
//! that narrowed no bound; `Fixed(n)` is additionally reachable through the public `GameAction`
//! surface. `handle_declare_shortcut` checks the declared count against the global cap and against
//! the offer's own `max_iterations`, and refuses `UntilLethal` against a bounded offer; what it
//! never checks is the declared shape against the schema's *suggested* `iteration_count` (the
//! fail-closed pin firewall validates only `template` pins, and it runs against the RESOLVED
//! template rather than the payload's: the handler shadows it with
//! `template.or_else(|| offer.declaration.cloned())` before the `match`, so a payload carrying
//! `None` against an offer that PUBLISHED a declaration reaches the `Some` arm and IS
//! pin-validated by `declaration_conforms`. The firewall is skipped only when the payload carried
//! none AND the offer published none — the arm that still refuses unless the proposer controls
//! the recorded loop period).
//!
//! The count gate is what keeps the three rulings off the AI's own bounded candidate. Every offer
//! the AI answers with `Fixed` is a bounded one, and every production mint of a bounded offer
//! latches `predicted_winner: None` — ruling 2's antecedent shape, which a count-blind reject would
//! fire on.
//!
//! ## Why the verdict reads `proposer` from the state, never `ctx.ai_player`
//!
//! `WaitingFor::acting_player()` returns `Some(proposer)` for `LoopShortcut`, and the beam/rollout
//! paths bind `PolicyContext::ai_player` to exactly that, so the two coincide wherever both are
//! defined. Reading `proposer` is fail-safe: the candidate is the proposer's action by
//! construction (`metadata.actor == Some(proposer)`), so the veto stays correct even if a caller
//! ever scores this state under a different seat's value lens — whereas gating on
//! `ctx.ai_player == proposer` would silently DROP the veto in that case.

use engine::analysis::decision_template::{DecisionPoint, DecisionTemplate, IterationCount};
use engine::analysis::loop_check::LoopCertificate;
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
        // (see `cycles_to_proposer_elimination` below for the CR 704 self-cost predicate)

        // Cheapest possible gate FIRST (one enum-discriminant compare): every `ActivateAbility`
        // candidate in the game runs this. It is also this policy's contribution to NaN safety —
        // `DeclineShortcut` exits here, so the policy can never reject BOTH candidates.
        // (`softmax_select_pairs` also self-heals on an all-`-inf` vector via its
        // `!total.is_finite()` argmax fallback — belt and braces.)
        let GameAction::DeclareShortcut { count, template } = &ctx.candidate.action else {
            return na();
        };
        let WaitingFor::LoopShortcut {
            proposer,
            predicted_winner,
            schema,
            certificate,
            declaration,
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

            // CR 732.2a: a ZERO-repetition declaration is representable, and the engine treats
            // it as legal — `a_zero_count_declaration_validates_over_an_empty_range_but_still_
            // checks_cardinality` in `crates/engine/tests/integration/loop_shortcut.rs` pins
            // exactly that — but it commits NO cycles while still spending the CR 732.2b
            // response window. That is the same weak-domination shape the over-bound arm below
            // and the `(None, UntilLethal)` arm above reject: the outcome set is {no-op, minus a
            // response window}, so declining weakly dominates it.
            //
            // ⚠ ORDER IS LOAD-BEARING: this arm MUST precede the `!schema.is_bounded() => na()`
            // arm below. `Fixed(0)` matches that guard too, so with the arms the other way round
            // a zero-count declare on an UNBOUNDED offer scored NEUTRAL rather than rejected —
            // and the domination argument above does not depend on boundedness at all. A
            // declaration that commits nothing spends the CR 732.2b window whether or not the
            // offer carries a bound, so the two schema shapes must reach the same verdict.
            // `loop_shortcut_unbounded_declare_rejects_zero_count` is the regression for the
            // unbounded half specifically, and it goes RED via `na()` if this arm is moved back
            // below.
            //
            // Not reachable from today's generator either way (it emits only
            // `Fixed(max_iterations)`, and the load seam refuses `max_iterations: 0`), so no
            // scoring that can occur today is reordered. The arm states the scoring arm's OWN
            // precondition rather than leaving it to an invariant maintained a crate away.
            (_, IterationCount::Fixed(0)) => PolicyVerdict::reject(PolicyReason::new(
                "loop_shortcut_bounded_declare_zero_count",
            )),

            // CR 732.2a "a loop that repeats a specified number of times". The verdict splits
            // on whether the OFFER states a real CR 704 bound, because the domination argument
            // below is valid only when it does.
            //
            // UNBOUNDED offer ⇒ neither rejected nor boosted, deliberately. An offer whose
            // producer could not compute a bound publishes `MAX_SHORTCUT_CYCLES`, so it states
            // no CR 704 threshold for a domination argument to stand on, and a count-blind
            // reject would be wrong for the CLASS: `materialize_fixed_shortcut` drives and
            // COMMITS `n` whole cycles, and a count that crosses nothing crowns nobody, so a
            // small-`n` `Fixed` is genuine committed board progress whoever is latched.
            //
            // NOTE (tripwire, still live for the unbounded branch): a `Fixed(n)` large enough
            // to cross lethal WOULD commit a `GameOver` — and against an offer whose name is
            // RIGHT it still does. `materialize_fixed_shortcut`'s `CrossLethal` arm now crowns
            // only the seat `proposal.predicted_winner` names (or any seat when it names
            // nobody), so what lets such a declare through is no longer the absence of a filter
            // but the offer's own prediction agreeing with the drive. Such a declare by a faller
            // proposer is a committed self-loss, exactly what the `UntilLethal` arm above
            // rejects, and this arm is still neutral on it. On a BOUNDED offer that hazard is
            // discharged by the `loop_shortcut_declare_eliminates_proposer` arm BELOW, which asks
            // `cycles_to_proposer_elimination` of the proposer alone. It is NOT discharged by
            // `elimination_bounds`' contract: that bound admits a crossing as the sequence's
            // FINAL iteration, so `max_iterations` can equal the proposer's own fatal count and
            // `Fixed(max_iterations)` — the AI's only bounded candidate — can name it. That
            // arm's remaining limit is stated where it sits, not assumed away here.
            //
            // ⚠ AN EARLIER FORM OF THIS ARM ALSO RESTED ON A DRIVE THAT DID NOT HONOUR THE
            // PUBLISHED BOUND, which is why the delimiter's history is kept here. At `c6d834040`
            // `materialize_fixed_shortcut` had no cycle delimiter for the basis-B class, so the
            // drive ran to the beat cap and `Fixed(1)` on a bounded 3p/4p drain eliminated the
            // whole table — the arithmetic above described a bound the drive never honoured. Fix
            // round 1 delimits a cycle by the published `PeriodicDelta::frames_per_period` and
            // drops any cycle whose measured delta differs from the published one.
            //
            // RE-MEASURED after that fix, through the production accept path (declare + APNAP
            // accepts) at `n` = 1, 3 and AT the offered bound:
            //   bloodloop 3p (bound 16): n=1 [20,16,16]  n=3 [20,14,14]  n=16 [20,1,1]  elim 0
            //   bloodloop 4p (bound 16): n=1/3/16 likewise, elim 0
            //   dina 4p      (bound 30): n=1 [50,34,30,35] … n=30 [79,5,1,6]            elim 0
            // The engine-side regression row that keeps this honest is
            // `bounded_fixed_count_commits_exactly_n_periods`, which asserts zero eliminations
            // and `committed == n × published δ` on all three fixtures.
            (_, IterationCount::Fixed(_)) if !schema.is_bounded() => na(),

            // CR 732.2a: over the offered bound. The AI-side mirror of the engine's own
            // declare-time guard — such a declare names a count at which some living player
            // crosses a CR 704.5a / CR 704.5c / CR 104.3c threshold inside the proposal, which
            // is a conditional action, so the engine hands it back fail-closed with ZERO
            // committed cycles while the CR 732.2b response window is spent. Weakly dominated
            // by declining: the outcome set is {no-op minus a response window}. Same
            // domination shape the `(None, UntilLethal)` arm above encodes.
            (_, IterationCount::Fixed(n)) if *n > schema.max_iterations => PolicyVerdict::reject(
                PolicyReason::new("loop_shortcut_bounded_declare_over_bound")
                    .with_fact("declared", i64::from(*n))
                    .with_fact("max_iterations", i64::from(schema.max_iterations)),
            ),

            // CR 732.2a: within the offered bound on a bounded offer ⇒ committed board
            // progress that eliminates at most the binding seat, and only on the sequence's
            // FINAL iteration. Never the proposer — the `Some(fatal)` branch below is what
            // refuses that. Game-deciding ⇒ critical band, via the
            // auto-banding `PolicyVerdict::score` (NEVER `preference`, whose `debug_assert!`
            // band domain panics on this field's default). Both declare kinds route through
            // the one reused config field on purpose: the winning arm above and this one are
            // never both scoreable at a single node — a winning offer carries
            // `predicted_winner: Some(..)` and an unnarrowed bound (so no `Fixed` candidate is
            // generated), and a bounded offer carries `predicted_winner: None` (so the winning
            // arm cannot be reached). With no ordering to distort, a second tuned field would
            // buy nothing and cost the full `UNTUNED_POLICY_PENALTY_FIELDS` protocol.
            // CR 704.5a / CR 704.5c / CR 104.3c: the offered bound is derived from EVERY living
            // seat, and `ResourceVector::elimination_bounds` deliberately lets the PROPOSER be
            // the binding one — CR 732.2a's shortcut proposer "need not be the player proposing
            // the shortcut" who benefits, so the producer is right not to gate on proposer
            // benefit (engine `game/engine.rs`, `bounded_cycle_offer` doc). That makes it the
            // DECIDING side's job, and nothing was doing it: a bounded offer always carries
            // `predicted_winner: None`, so the "hands somebody else the win" arm above is
            // structurally unreachable here, and the AI's only bounded candidate is
            // `Fixed(max_iterations)` — the maximum, never a smaller n, and now a count the
            // producer carries all the way to the binding seat's own crossing. A self-mill
            // period whose binding seat is the proposer therefore scored CRITICAL for running
            // the proposer's own library to exactly 0.
            //
            // This arm asks the question the producer declines to ask, on the proposer's behalf
            // only, and REJECTS rather than dropping to `na()`: neutral would still leave the
            // declare competing on other policies' scores, and the domination argument here is
            // the same shape as the zero-count arm's — a declare that eliminates the declarer is
            // weakly dominated by declining, which rolls back to exactly where a decline lands.
            //
            // IT IS THE WHOLE DISCHARGE ON A BOUNDED OFFER. It charges the proposer's life
            // through the DECLARATION this candidate would propose: a `template: None` declare
            // is "no override of what the offer published", which is how
            // `handle_declare_shortcut` resolves it, so the pins charged here are the pins the
            // drive replays. The offer's own declaration is also the OBSERVATION the charge is
            // measured against: the engine derives it from what the proposer answered in the
            // window the period was certified over. A slot the declaration moves onto the
            // proposer is charged to the proposer even when the observed period drained
            // someone else (CR 119.3). One limit is named rather than hidden: the predicate is
            // proposer-only by construction.
            (_, IterationCount::Fixed(n)) => {
                let declared = template.as_ref().or(declaration.as_ref());
                match cycles_to_proposer_elimination(
                    ctx.state,
                    certificate,
                    &schema.points,
                    declared,
                    declaration.as_ref(),
                    *proposer,
                    *n,
                ) {
                    Some(fatal) => PolicyVerdict::reject(
                        PolicyReason::new("loop_shortcut_declare_eliminates_proposer")
                            .with_fact("declared", i64::from(*n))
                            .with_fact("eliminates_at", fatal),
                    ),
                    None => PolicyVerdict::score(
                        ctx.penalties().loop_shortcut_winning_declare_bonus,
                        PolicyReason::new("loop_shortcut_bounded_declare_progress")
                            .with_fact("declared", i64::from(*n)),
                    ),
                }
            }
        }
    }
}

/// The fewest whole cycles of this offer's certified period, at most `declared`, that drive
/// `proposer` to a CR 704 elimination threshold under `declaration`, or `None` if the proposer
/// survives all `declared` of them. `observed` is the declaration the offer published, the
/// allocation the period was measured under.
///
/// This is the inverse of `ResourceVector::elimination_bounds` (engine `analysis/resource.rs`),
/// asked of ONE seat under ONE declaration instead of narrowed over all seats and every
/// declaration:
///
/// - **life**, CR 704.5a — reaching **0 or less** is the threshold, and CR 704.3 checks it at
///   every priority beat, inside a repetition as well as between two. Repetition `k` is fatal
///   when what the earlier repetitions NETTED plus the deepest DIP inside `k` reaches the life
///   total. A period that pays 1 and gains 1 therefore never kills a proposer above 1 life,
///   and one that pays 4 and gains 3 kills a 5-life proposer in its second repetition, after
///   netting only 1. Both numbers come from `PeriodicDelta::declared_seat_life_charges`,
///   never re-derived here, one pair per repetition because a scheduled pin may name a
///   different seat at each index. Measured against the net-only rate this replaced, the
///   check only tightens: each repetition's net and dip are both at least what the period
///   itself nets off the proposer, so every declare that rate refused is still refused.
/// - **poison**, CR 704.5c — reaching **10 or more** is the threshold.
///
/// **LIBRARY IS DELIBERATELY EXCLUDED, and that is the non-obvious part of this function.**
/// CR 121.4: "A player who attempts to draw a card from a library with no cards in it loses the
/// game the next time a player would receive priority." The loss attaches to the DRAW ATTEMPT,
/// not to the library reaching zero — a player with an empty library and no draw ahead of them
/// has not lost and may still win. Milling yourself to exactly zero is a legal, sometimes
/// winning line (self-mill payoffs; `loop_check` classifies such a period as `Advantage`, and
/// `elimination_bounds` intentionally permits the exactly-zero terminal value), so vetoing it
/// would refuse a real strategy class. A certified period records per-cycle resource deltas; it
/// cannot express "and then the proposer is forced to draw", so on today's evidence no
/// library-based veto is sound. **Extension point:** if a future certificate can prove a forced
/// post-zero draw, that is what would license adding the axis back — reconcile it against
/// `loop_shortcut_declare_that_mills_the_proposer_to_exactly_zero_still_scores`.
///
/// **The principle is accumulation ≠ realization, and the engine already tests it.** The One Ring
/// under the Kilo/Freed/Relic proliferate engine certifies an infinite burden-GROWTH loop as
/// `WinKind::Advantage` — not a win — naming the unbounded burden counter axis
/// (`analysis/corpus_tests.rs`, `one_ring_burden_growth_certificate`, with the 0-burden dead-loop
/// control beside it and the driver doc at `analysis/corpus.rs`). The burden's lethality realizes
/// only DOWNSTREAM, at the upkeep trigger, which is where CR 704.5a finally applies:
/// `tests/integration/one_ring_burden_upkeep_lethal.rs` pairs
/// `one_ring_burden_upkeep_kills_owner_p0_wins` with the sub-lethal control
/// `one_ring_sublethal_burden_owner_survives_no_gameover`. The engine therefore refuses to treat
/// in-loop accumulation of a doom resource as realized elimination. An emptying library is the
/// same shape with mill in place of burden and the draw attempt in place of the upkeep trigger,
/// so excluding it makes this policy consistent with the engine's own tested doctrine rather than
/// stricter than it.
///
/// **That is also why life STAYS, and the line is principled rather than an ad-hoc keep/drop.**
/// A certified period's life loss is IN-CYCLE realization: the drain happens inside the certified
/// period and is state-based-checkable at its priority beats, so the certificate does prove the
/// death it implies. The One Ring's life loss would only enter a certificate the same way if the
/// upkeep trigger were inside the loop span. Both surviving axes are immediate state-based losses
/// on state alone (CR 704), requiring no intervening action — which is exactly the property an
/// empty library lacks.
///
/// Only movement TOWARD death counts: a life gain or a poison decrease yields no bound on that
/// axis. The engine's life charge is a non-negative magnitude by construction, and the poison
/// rate is tested `> 0` before it is allowed to divide.
///
/// Returns the MINIMUM across axes, so the caller asks one question: `Some` refuses the declare.
/// `None` means "no axis kills the proposer within `declared` cycles" — including the case where
/// this offer carries no certified period at all. That last branch is unreachable for the bounded
/// class (`certified_bounded_cycle_offer` mints `per_cycle: Some(periodic)`), and it is written
/// as a plain `?` rather than an `expect` because a policy must never panic on a state shape;
/// the reach-guards in `loop_shortcut_declare_that_kills_the_proposer_on_life_is_refused` and
/// `loop_shortcut_declare_that_mills_the_proposer_to_exactly_zero_still_scores` are what prove
/// the `Some` path is the one actually exercised.
fn cycles_to_proposer_elimination(
    state: &GameState,
    certificate: &LoopCertificate,
    points: &[DecisionPoint],
    declaration: Option<&DecisionTemplate>,
    observed: Option<&DecisionTemplate>,
    proposer: PlayerId,
    declared: u32,
) -> Option<i64> {
    let period = certificate.per_cycle.as_ref()?;
    let player = state.players.get(proposer.0 as usize)?;

    // CR 704.3 + CR 704.5a: the first repetition inside which the proposer can reach 0 or less
    // life. Accumulated rather than divided, because a scheduled pin may name the proposer at
    // some repetitions and another seat at the rest.
    let life = i64::from(player.life);
    let mut netted = 0i64;
    let life_fatal = (1..=i64::from(declared))
        .zip(period.declared_seat_life_charges(proposer, declaration, observed, points, state))
        .find_map(|(repetition, charge)| {
            let fatal = netted + charge.dip >= life;
            netted += charge.net;
            fatal.then_some(repetition)
        });

    // CR 704.5c: `headroom / rate` rounded UP, the first whole cycle at which ten poison
    // counters is met. Written long-hand rather than with `i64::div_ceil`, which is still
    // unstable on this toolchain (`int_roundings`). Both operands are non-negative here —
    // `headroom` is clamped and `rate` is guarded `> 0` — so the `+ rate - 1` form is exact,
    // with no negative-operand truncation-toward-zero trap.
    let poison_rate = period.delta.poison.get(&proposer).copied().unwrap_or(0);
    let poison_fatal = (poison_rate > 0)
        .then(|| ((10 - i64::from(player.poison_counters)).max(0) + poison_rate - 1) / poison_rate)
        .filter(|fatal| *fatal <= i64::from(declared));

    // No library term — see the CR 121.4 exclusion in this function's doc comment.
    life_fatal.into_iter().chain(poison_fatal).min()
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
    use engine::analysis::decision_template::{
        AnnouncementSubject, DecisionGroupKey, DecisionPointKind, DecisionSlot, PinnedDecision,
        Ranking, ReplayMode, ShortcutDecisionSchema, TargetPin, TargetSchedule,
    };
    use engine::analysis::loop_check::{LoopCertificate, WinKind};
    use engine::analysis::resource::{BoardDelta, PeriodicDelta, ResourceVector};
    use engine::game::zones::create_object;
    use engine::types::ability::TargetRef;
    use engine::types::game_state::YieldTarget;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::zones::Zone;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    /// A synthetic optional-lethal certificate with NO certified period.
    ///
    /// The policy reads exactly one field of this: `per_cycle`, via
    /// `cycles_to_proposer_elimination`. `None` here means "no self-elimination bound is
    /// derivable", so every row built on this helper exercises the pre-existing arms only —
    /// which is why the self-cost rows below use [`bounded_offer_with_period`] instead.
    fn cert() -> LoopCertificate {
        LoopCertificate {
            unbounded: vec![],
            win_kind: WinKind::LethalDamage,
            mandatory: false,
            residual_board_delta: BoardDelta::default(),
            per_cycle: None,
        }
    }

    fn offer_state(predicted_winner: Option<PlayerId>) -> GameState {
        let mut state = GameState::new_two_player(0);
        state.waiting_for = WaitingFor::LoopShortcut {
            proposer: P0,
            predicted_winner,
            certificate: cert(),
            schema: ShortcutDecisionSchema::default(),
            declaration: None,
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

    /// Rows 3 + 5 + 7 — THE CLASS GUARD: `materialize_fixed_shortcut` COMMITS every cycle it
    /// drives, and a count that crosses nothing crowns nobody, so a `Fixed(n)` declare is real
    /// board progress for ANY latched winner. Proves the reject set is not one state too wide.
    ///
    /// This row stays green LEGITIMATELY, not by luck: `ShortcutDecisionSchema::default()`
    /// carries `max_iterations == MAX_SHORTCUT_CYCLES`, so `is_bounded()` is FALSE and the
    /// verdict takes the deliberately-neutral unbounded branch. That branch is asserted
    /// DIRECTLY by `loop_shortcut_unbounded_offer_keeps_fixed_neutral` below, so a future
    /// re-scoping that deletes the `!schema.is_bounded()` guard fails THERE with a message
    /// naming the guard, rather than here with no explanation.
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

    /// A BOUNDED offer — the only shape `try_offer_bounded_cycle_shortcut` mints. `points`
    /// stays empty because the engine's `Fixed` candidate generator is gated on that too.
    fn bounded_offer_state(max_iterations: u32) -> GameState {
        let mut state = GameState::new_two_player(0);
        state.waiting_for = WaitingFor::LoopShortcut {
            proposer: P0,
            predicted_winner: None,
            certificate: cert(),
            schema: ShortcutDecisionSchema {
                max_iterations,
                ..Default::default()
            },
            declaration: None,
        };
        state
    }

    /// A BOUNDED offer carrying a real certified period — the shape
    /// `certified_bounded_cycle_offer` actually mints (`per_cycle: Some(periodic)`), as opposed
    /// to [`cert`]'s `None`.
    fn bounded_offer_with_period(max_iterations: u32, period: PeriodicDelta) -> GameState {
        bounded_offer_declaring(
            GameState::new_two_player(0),
            max_iterations,
            period,
            Vec::new(),
            None,
        )
    }

    /// The same bounded offer on a caller-built board, publishing `points` and the engine's
    /// own `declaration` for them. The single offer literal the self-cost rows share, so the
    /// CR 732.2a offer-writer census counts one fixture site however many shapes they stage.
    fn bounded_offer_declaring(
        mut state: GameState,
        max_iterations: u32,
        period: PeriodicDelta,
        points: Vec<DecisionPoint>,
        declaration: Option<DecisionTemplate>,
    ) -> GameState {
        state.waiting_for = WaitingFor::LoopShortcut {
            proposer: P0,
            predicted_winner: None,
            certificate: LoopCertificate {
                per_cycle: Some(period),
                ..cert()
            },
            schema: ShortcutDecisionSchema {
                max_iterations,
                points,
                ..Default::default()
            },
            declaration,
        };
        state
    }

    /// One repetition's whole-game delta, given only the axes a test cares about.
    fn periodic(library_delta: &[(PlayerId, i64)], life: &[(PlayerId, i64)]) -> PeriodicDelta {
        PeriodicDelta {
            frames_per_period: 1,
            delta: ResourceVector {
                library_delta: library_delta.iter().copied().collect(),
                life: life.iter().copied().collect(),
                ..Default::default()
            },
            victim_slot: vec![],
            declarable_victims: vec![],
            seat_life_charge: vec![],
        }
    }

    /// One repetition's whole-game delta on the POISON axis (CR 704.5c). Kept separate from
    /// [`periodic`] so the common life/library rows stay two-argument, and so a poison row needs
    /// no post-construction mutation of `waiting_for` — reaching into the offer to patch the
    /// certificate would add an in-test hit to the CR 732.2a offer-writer census for no gain.
    fn periodic_poison(poison: &[(PlayerId, i64)]) -> PeriodicDelta {
        PeriodicDelta {
            frames_per_period: 1,
            delta: ResourceVector {
                poison: poison.iter().copied().collect(),
                ..Default::default()
            },
            victim_slot: vec![],
            declarable_victims: vec![],
            seat_life_charge: vec![],
        }
    }

    fn stock_library(state: &mut GameState, player: PlayerId, cards: u64) {
        state.players[player.0 as usize].library = (0..cards).map(ObjectId).collect();
    }

    fn certificate_of(state: &GameState) -> &LoopCertificate {
        match &state.waiting_for {
            WaitingFor::LoopShortcut { certificate, .. } => certificate,
            other => panic!("expected a LoopShortcut offer, got {other:?}"),
        }
    }

    fn schema_of(state: &GameState) -> &ShortcutDecisionSchema {
        match &state.waiting_for {
            WaitingFor::LoopShortcut { schema, .. } => schema,
            other => panic!("expected a LoopShortcut offer, got {other:?}"),
        }
    }

    /// CR 121.4 — running your OWN library to exactly zero is NOT a loss, and a bounded loop that
    /// does it must still score. "A player who attempts to draw a card from a library with no
    /// cards in it loses the game the next time a player would receive priority" — the loss
    /// attaches to the DRAW ATTEMPT, not to the library reaching zero. A self-mill line that ends
    /// at exactly zero with no draw in the cycle is legal, is a real strategy class (self-mill
    /// payoffs, `loop_check` classifies it as `Advantage`), and can be the winning line.
    ///
    /// WRITTEN BEFORE THE FIX AND OBSERVED FAILING, which is this row's revert-probe: against the
    /// library-axis veto it read `loop_shortcut_declare_eliminates_proposer` and the `assert_eq!`
    /// below reported that kind. Re-adding a library term to `cycles_to_proposer_elimination`
    /// flips it back, so the row cannot silently lose its grip on the regression.
    ///
    /// The certificate cannot represent a forced post-zero draw, so no library-based veto is
    /// sound on today's evidence; if a future certificate proves a forced draw, that is the
    /// extension point and this row is what it must be reconciled against.
    ///
    /// Same shape as the tested One Ring case (`one_ring_burden_upkeep_lethal.rs`): burden
    /// accumulating in-loop is not the loss — the upkeep trigger that realizes it is. Here the
    /// draw attempt plays the upkeep's role, per CR 121.4. Accumulation ≠ realization.
    #[test]
    fn loop_shortcut_declare_that_mills_the_proposer_to_exactly_zero_still_scores() {
        // 30 cards, 3 of the proposer's own milled per cycle ⇒ EXACTLY zero at the 10th cycle.
        let mut state = bounded_offer_with_period(10, periodic(&[(P0, -3), (P1, -1)], &[]));
        stock_library(&mut state, P0, 30);

        assert!(
            schema_of(&state).is_bounded(),
            "REACH-GUARD: the arm under test is bounded-only; max_iterations = {}",
            schema_of(&state).max_iterations
        );
        assert!(
            certificate_of(&state).per_cycle.is_some(),
            "REACH-GUARD: a certificate with no period makes every assertion below vacuous"
        );
        assert_eq!(state.players[P0.0 as usize].library.len(), 30);
        assert_eq!(
            state.players[P0.0 as usize].life, 20,
            "REACH-GUARD: the proposer must be alive on the axes that DO kill (CR 704.5a life, \
             CR 704.5c poison), or this row would pass for the wrong reason"
        );

        let v = verdict_for(&state, &declare(IterationCount::Fixed(10)));
        assert_eq!(
            kind_of(&v),
            "loop_shortcut_bounded_declare_progress",
            "10 cycles put the proposer's library at exactly 0 with no draw — legal under \
             CR 121.4, so the scoring arm owns it; got {v:?}"
        );
        assert!(delta_of(&v) > STRONG_MAX);
    }

    /// The CONTRAST that proves the guard reads the PROPOSER's seat and not merely "some seat is
    /// being drained": identical schema, identical bound, identical declared count — the only
    /// change is which player the life loss names. Re-based from the library axis onto life when
    /// CR 121.4 removed library as an elimination axis; a library-drain contrast would now pass
    /// for BOTH seats and discriminate nothing.
    #[test]
    fn loop_shortcut_declare_that_kills_only_an_opponent_still_scores() {
        let state = bounded_offer_with_period(10, periodic(&[], &[(P1, -2)]));
        assert_eq!(
            state.players[P1.0 as usize].life, 20,
            "REACH-GUARD: 10 cycles at -2 must actually reach 0 on the OPPONENT, or the contrast \
             is vacuous"
        );

        let v = verdict_for(&state, &declare(IterationCount::Fixed(10)));
        assert_eq!(
            kind_of(&v),
            "loop_shortcut_bounded_declare_progress",
            "draining an OPPONENT to zero life is the loop working as intended; got {v:?}"
        );
        assert!(delta_of(&v) > STRONG_MAX);
    }

    /// CR 704.5a — the AI must not declare a bounded loop that runs its OWN life to 0 or less.
    /// `elimination_bounds` deliberately lets the proposer be the binding seat, and the engine's
    /// only bounded candidate is `Fixed(max_iterations)`, so before this guard the heuristic path
    /// declared a self-killing loop at the CRITICAL band.
    ///
    /// THIS IS THE THRESHOLD DISCRIMINATOR, moved here from the library axis when CR 121.4 struck
    /// library from `cycles_to_proposer_elimination`. Life is a true elimination axis: 0 or less
    /// life is an immediate state-based loss on state alone, with no intervening action required
    /// — unlike an empty library, which kills only at the next DRAW ATTEMPT.
    ///
    /// THE `Fixed(9)` ROW IS WHAT MAKES THIS A THRESHOLD TEST rather than a test that a
    /// proposer-negative axis exists at all. A "fix" that rejected any period charging the
    /// proposer's life would satisfy the `Fixed(10)` assertion and still be wrong — it would
    /// refuse profitable loops the proposer survives. One cycle short leaves 2 life, and that
    /// must still score.
    ///
    /// REVERT-PROBE: delete the `Some(fatal)` reject branch ⇒ `Fixed(10)` falls
    /// through to the scoring branch and reads `loop_shortcut_bounded_declare_progress`, so the
    /// first assertion FAILS while the `Fixed(9)` row stays green — the two rows fail
    /// independently, which is what makes the pair discriminating rather than redundant.
    #[test]
    fn loop_shortcut_declare_that_kills_the_proposer_on_life_is_refused() {
        let state = bounded_offer_with_period(10, periodic(&[], &[(P0, -2)]));
        assert_eq!(state.players[P0.0 as usize].life, 20);
        assert!(
            schema_of(&state).is_bounded(),
            "REACH-GUARD: the arm under test is bounded-only; max_iterations = {}",
            schema_of(&state).max_iterations
        );
        assert!(
            certificate_of(&state).per_cycle.is_some(),
            "REACH-GUARD: a certificate with no period makes every assertion below vacuous"
        );

        assert_eq!(
            kind_of(&verdict_for(&state, &declare(IterationCount::Fixed(10)))),
            "loop_shortcut_declare_eliminates_proposer"
        );
        assert_eq!(
            kind_of(&verdict_for(&state, &declare(IterationCount::Fixed(9)))),
            "loop_shortcut_bounded_declare_progress",
            "9 cycles leave the proposer at 2 life — alive, so the scoring arm still owns it"
        );
    }

    /// CR 704.5a: **the multi-authority pair — the same declared count, on two boards that
    /// differ only in WHICH seat the period drains.**
    ///
    /// The engine publishes a legal bound either way; the AI must refuse only the board where
    /// the seat the bound's arithmetic names is the PROPOSER. That refusal is now load-bearing
    /// rather than redundant: the published bound reaches the binding seat's own crossing, so
    /// `max_iterations` can EQUAL `cycles_to_proposer_elimination`, and the AI's only bounded
    /// candidate is `Fixed(max_iterations)`. Both boards assert that equality (or its absence)
    /// off the predicate itself, so the row states the re-attribution instead of assuming it.
    ///
    /// REVERT-PROBE: delete the `Some(fatal)` reject branch ⇒ the proposer-as-faller board falls
    /// through to `loop_shortcut_bounded_declare_progress` and SCORES ⇒ the first assertion
    /// FAILS while the opponent sibling stays green.
    #[test]
    fn loop_shortcut_declare_at_the_bound_is_refused_only_when_the_proposer_is_the_faller() {
        // 20 life at 2 per cycle: the strict headroom is 9 and the crossing is 10, which is
        // the count the producer now publishes for a board with one binding seat.
        const BOUND: u32 = 10;

        let proposer_falls = bounded_offer_with_period(BOUND, periodic(&[], &[(P0, -2)]));
        let opponent_falls = bounded_offer_with_period(BOUND, periodic(&[], &[(P1, -2)]));

        assert_eq!(
            cycles_to_proposer_elimination(
                &proposer_falls,
                certificate_of(&proposer_falls),
                &[],
                None,
                None,
                P0,
                BOUND
            ),
            Some(i64::from(BOUND)),
            "REACH-GUARD: the declared MAXIMUM must be exactly the proposer's fatal count, or \
             this row is the over-bound case and not the at-the-bound one"
        );
        assert_eq!(
            cycles_to_proposer_elimination(
                &opponent_falls,
                certificate_of(&opponent_falls),
                &[],
                None,
                None,
                P0,
                BOUND
            ),
            None,
            "REACH-GUARD: the sibling board must charge the proposer NOTHING, so the two arms \
             differ in the faller's identity and in nothing else"
        );

        assert_eq!(
            kind_of(&verdict_for(
                &proposer_falls,
                &declare(IterationCount::Fixed(BOUND))
            )),
            "loop_shortcut_declare_eliminates_proposer",
            "the engine may legally publish a count whose final iteration removes the \
             proposer; declining is the AI's own job"
        );
        assert_eq!(
            kind_of(&verdict_for(
                &opponent_falls,
                &declare(IterationCount::Fixed(BOUND))
            )),
            "loop_shortcut_bounded_declare_progress",
            "PAIRED POSITIVE: the same declare at the same count on a board whose faller is an \
             opponent still scores — without it the refusal above could be a blanket one"
        );
    }

    /// The offered bound of [`slot_charged_offer`]: P0 at 18 life charged 2 per repetition
    /// crosses on the 9th, and P1 at 20 does not until the 10th, so P0 is the sole binding seat.
    const SLOT_BOUND: u32 = 9;

    /// A CR 601.2c target schedule pinning each slot onto its seat at every repetition — the
    /// TARGET-class spelling `record_trigger_target_answer` journals.
    fn seat_declaration(pins: &[(&DecisionSlot, PlayerId)]) -> DecisionTemplate {
        let sources: Vec<_> = pins.iter().map(|(slot, _)| slot.source.clone()).collect();
        DecisionTemplate {
            owner: P0,
            decisions: pins
                .iter()
                .map(|(slot, seat)| PinnedDecision::Targets {
                    slot: (*slot).clone(),
                    targets: vec![TargetPin::Scheduled(TargetSchedule::Constant(
                        Ranking::one(AnnouncementSubject::Seat(*seat)),
                    ))],
                })
                .collect(),
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(SLOT_BOUND),
            },
            key: DecisionGroupKey::from_sources(
                &sources,
                engine::analysis::decision_template::DecisionKind::LoopChoice,
            ),
        }
    }

    /// A trigger source P0 controls, announcing one player-target slot.
    fn announced_slot(state: &mut GameState, card: CardId) -> DecisionSlot {
        let source_id = create_object(
            state,
            card,
            P0,
            "Drain Engine".to_string(),
            Zone::Battlefield,
        );
        DecisionSlot::target(YieldTarget::ThisObject {
            source_id,
            incarnation: None,
            trigger_description: None,
        })
    }

    /// The published CR 115.2 legal set of a slot that may name either seat.
    fn either_seat_point(slot: &DecisionSlot) -> DecisionPoint {
        DecisionPoint {
            slot: slot.clone(),
            kind: DecisionPointKind::Targets {
                legal_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
                min_targets: 1,
                max_targets: 1,
                ordered: false,
            },
        }
    }

    /// CR 119.3: a board whose loop source P0 controls announces a player-target slot that may
    /// name either seat, and whose certified period saw that slot drain P1 by 2. The period
    /// carries what the engine mint publishes for that window — the slot charged to BOTH
    /// reached seats, because the bound is reserved against every legal declaration — and the
    /// offer publishes the engine's own declaration, pinned where the window saw the slot aim:
    /// on P1.
    fn slot_charged_offer() -> (GameState, DecisionSlot) {
        let mut state = GameState::new_two_player(0);
        state.players[P0.0 as usize].life = 18;
        let slot = announced_slot(&mut state, CardId(1));
        let period = PeriodicDelta {
            frames_per_period: 1,
            delta: ResourceVector {
                life: [(P1, -2)].into_iter().collect(),
                ..Default::default()
            },
            victim_slot: vec![(slot.clone(), 2)],
            declarable_victims: vec![P0, P1],
            seat_life_charge: vec![(P0, 2), (P1, 2)],
        };
        let point = either_seat_point(&slot);
        let published = seat_declaration(&[(&slot, P1)]);
        let state =
            bounded_offer_declaring(state, SLOT_BOUND, period, vec![point], Some(published));
        (state, slot)
    }

    /// CR 704.5a + CR 119.3 + CR 732.2a — **a declaration that aims a charged target slot at the
    /// proposer is refused, even though the observed period drained somebody else.**
    ///
    /// The certified period's NET delta charges the proposer nothing: the window saw the slot
    /// drain P1. A declaration may still pin that slot onto P0 (it is one of the slot's
    /// published legal targets), and CR 119.3 then takes 2 life off P0 every repetition. At the
    /// offered bound that is 18 life to 0, a CR 704.5a loss the declarer handed itself.
    ///
    /// THREE LEGS, one offer, differing only in the declaration or the count:
    /// * ⓐ pinned onto P0 at the bound ⇒ refused.
    /// * ⓑ PAIRED CONTROL: the offer's own published declaration (a `template: None` declare,
    ///   which the declare handler resolves against it), pinned on P1 ⇒ P0 is charged nothing
    ///   and the declare still scores.
    /// * ⓒ THRESHOLD: pinned onto P0 one repetition short of the bound ⇒ P0 ends at 2 life and
    ///   the declare still scores, so the refusal is a threshold and not a blanket veto on any
    ///   declaration charging the proposer.
    ///
    /// REVERT-PROBES, each failing a different leg:
    /// * charge the proposer only by the period's net `delta.life` (the pre-fix predicate) ⇒ ⓐ
    ///   reads `loop_shortcut_bounded_declare_progress` ⇒ FAILS.
    /// * accumulate the published per-seat charge, blind to the declaration ⇒ ⓑ is refused ⇒
    ///   FAILS.
    /// * pass no `observed` declaration ⇒ the slot may have been on P0, so ⓑ is refused ⇒
    ///   FAILS.
    /// * drop the fallback to the offer's `declaration` ⇒ ⓑ's unpinned slot may land on P0 and
    ///   is refused ⇒ FAILS.
    #[test]
    fn loop_shortcut_declare_that_aims_a_charged_slot_at_the_proposer_is_refused() {
        let (state, slot) = slot_charged_offer();
        assert!(
            schema_of(&state).is_bounded(),
            "REACH-GUARD: the arm under test is bounded-only; max_iterations = {}",
            schema_of(&state).max_iterations
        );
        let period = certificate_of(&state)
            .per_cycle
            .as_ref()
            .expect("REACH-GUARD: a certificate with no period makes every assertion vacuous");
        assert_eq!(
            period.delta.life.get(&P0),
            None,
            "REACH-GUARD: the observed period charges the proposer NOTHING, so a refusal below \
             can only come from the declared slot"
        );
        let declare_onto_proposer = |count: u32| CandidateAction {
            action: GameAction::DeclareShortcut {
                count: IterationCount::Fixed(count),
                template: Some(seat_declaration(&[(&slot, P0)])),
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Utility),
        };

        // ⓐ
        assert_eq!(
            kind_of(&verdict_for(&state, &declare_onto_proposer(SLOT_BOUND))),
            "loop_shortcut_declare_eliminates_proposer",
            "9 repetitions of a slot pinned onto the proposer take 18 life to 0"
        );

        // ⓑ
        let as_published = verdict_for(&state, &declare(IterationCount::Fixed(SLOT_BOUND)));
        assert_eq!(
            kind_of(&as_published),
            "loop_shortcut_bounded_declare_progress",
            "PAIRED CONTROL: the published declaration pins the slot on P1, so the proposer \
             loses nothing and the same count still scores; got {as_published:?}"
        );
        assert!(delta_of(&as_published) > STRONG_MAX);

        // ⓒ
        assert_eq!(
            kind_of(&verdict_for(&state, &declare_onto_proposer(SLOT_BOUND - 1))),
            "loop_shortcut_bounded_declare_progress",
            "8 repetitions leave the proposer at 2 life — alive, so the scoring branch owns it"
        );
    }

    /// CR 119.3 + CR 704.3 + CR 704.5a — **the swap board, declared both ways at the offered
    /// bound.** P0 at 7 life pays 2 a repetition. S1 ("target player loses 1 life") was seen on
    /// P0 and S2 ("target player loses 2 life") on P1, so the period nets P0 −3 and P1 −2, both
    /// slots carry the worst loss, 3, and each seat is reserved 6. P0 binds at 2 repetitions.
    ///
    /// * ⓐ AS PUBLISHED: S2 stays off P0 in both templates, and P0 loses 3 a repetition,
    ///   7 → 4 → 1, so the declare scores.
    /// * ⓑ SWAPPED: P0 loses 2 + 2 = 4 a repetition and is removed inside the second, so the
    ///   same count is refused.
    ///
    /// REVERT-PROBES, each failing a different leg:
    /// * charge every dip the full reserved charge, blind to the declaration ⇒ ⓐ's second
    ///   repetition reads 3 + 6 against 7 and is refused ⇒ FAILS.
    /// * charge the proposer only by the period's net `delta.life` (the pre-fix predicate) ⇒
    ///   ⓑ reads 3 + 3 against 7 and scores ⇒ FAILS.
    #[test]
    fn loop_shortcut_declare_on_the_swap_board_scores_as_published_and_is_refused_swapped() {
        const SWAP_BOUND: u32 = 2;
        let mut state = GameState::new_two_player(0);
        state.players[P0.0 as usize].life = 7;
        let s1 = announced_slot(&mut state, CardId(1));
        let s2 = announced_slot(&mut state, CardId(2));
        let period = PeriodicDelta {
            victim_slot: vec![(s1.clone(), 3), (s2.clone(), 3)],
            declarable_victims: vec![P0, P1],
            seat_life_charge: vec![(P0, 6), (P1, 6)],
            ..periodic(&[], &[(P0, -3), (P1, -2)])
        };
        let points = vec![either_seat_point(&s1), either_seat_point(&s2)];
        let published = seat_declaration(&[(&s1, P0), (&s2, P1)]);
        let state = bounded_offer_declaring(state, SWAP_BOUND, period, points, Some(published));
        assert!(
            schema_of(&state).is_bounded(),
            "REACH-GUARD: the arm under test is bounded-only"
        );

        // ⓐ
        let as_published = verdict_for(&state, &declare(IterationCount::Fixed(SWAP_BOUND)));
        assert_eq!(
            kind_of(&as_published),
            "loop_shortcut_bounded_declare_progress",
            "as published P0 ends the second repetition at 1 life; got {as_published:?}"
        );

        // ⓑ
        let swapped = CandidateAction {
            action: GameAction::DeclareShortcut {
                count: IterationCount::Fixed(SWAP_BOUND),
                template: Some(seat_declaration(&[(&s1, P1), (&s2, P0)])),
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Utility),
        };
        assert_eq!(
            kind_of(&verdict_for(&state, &swapped)),
            "loop_shortcut_declare_eliminates_proposer",
            "swapped, P0 loses 4 a repetition and the second takes it from 3 to 0 or less"
        );
    }

    /// CR 704.3 + CR 704.5a — **the proposer is checked at every priority beat, and charged
    /// only what a repetition nets between two.** Two boards, each offered at the proposer's
    /// own crossing, where the engine's frame-wise charge exceeds what the period nets:
    ///
    /// * PAY 1, GAIN 1 at 10 life: frame-wise charge 1, net 0. The total dips to 9 and comes
    ///   back every repetition, so the offered 10 repetitions kill nobody and the declare
    ///   scores.
    /// * PAY 4, GAIN 3 at 5 life: frame-wise charge 4, net 1. The first repetition dips to 1
    ///   and ends at 4; the second pays 4 from 4, and CR 704.5a removes the proposer before
    ///   the gain resolves. The declare is refused although 2 repetitions net only 2.
    ///
    /// REVERT-PROBES, one per board:
    /// * accumulate the frame-wise charge (the dip) as if every repetition netted it ⇒ the
    ///   first board reads 10 lost at the 10th repetition and is refused ⇒ FAILS.
    /// * accumulate only the net and never read the dip ⇒ the second board reads 2 lost of 5
    ///   and scores ⇒ FAILS.
    #[test]
    fn loop_shortcut_declare_checks_the_proposer_inside_a_repetition_not_only_between_two() {
        let at_life = |life: i32, bound: u32, net_loss: i64, frame_wise: i64| {
            let mut state = GameState::new_two_player(0);
            state.players[P0.0 as usize].life = life;
            let period = PeriodicDelta {
                seat_life_charge: vec![(P0, frame_wise)],
                ..periodic(&[], &[(P0, -net_loss)])
            };
            bounded_offer_declaring(state, bound, period, Vec::new(), None)
        };

        let even = at_life(10, 10, 0, 1);
        assert!(
            schema_of(&even).is_bounded(),
            "REACH-GUARD: the arm under test is bounded-only"
        );
        let even_verdict = verdict_for(&even, &declare(IterationCount::Fixed(10)));
        assert_eq!(
            kind_of(&even_verdict),
            "loop_shortcut_bounded_declare_progress",
            "a repetition that pays 1 and gains 1 never takes a 10-life proposer below 9; got \
             {even_verdict:?}"
        );

        let dipping = at_life(5, 2, 1, 4);
        assert_eq!(
            kind_of(&verdict_for(&dipping, &declare(IterationCount::Fixed(2)))),
            "loop_shortcut_declare_eliminates_proposer",
            "the second repetition pays 4 from 4 life before its gain resolves"
        );
    }

    /// The predicate is a MINIMUM across the surviving axes, and poison is one of them
    /// (CR 704.5c: ten or more poison counters is a loss). Life is left untouched here, so the
    /// ONLY binding axis is poison — a min that silently dropped it would score this offer.
    #[test]
    fn loop_shortcut_declare_that_poisons_the_proposer_out_is_refused() {
        let state = bounded_offer_with_period(10, periodic_poison(&[(P0, 1)]));
        assert_eq!(
            state.players[P0.0 as usize].poison_counters, 0,
            "REACH-GUARD: 10 cycles at +1 poison must actually reach the CR 704.5c threshold"
        );
        assert_eq!(
            state.players[P0.0 as usize].life, 20,
            "REACH-GUARD: life must be untouched so poison is the ONLY binding axis"
        );

        assert_eq!(
            kind_of(&verdict_for(&state, &declare(IterationCount::Fixed(10)))),
            "loop_shortcut_declare_eliminates_proposer",
            "10 cycles put the proposer at 10 poison — a CR 704.5c loss on state alone"
        );
        assert_eq!(
            kind_of(&verdict_for(&state, &declare(IterationCount::Fixed(9)))),
            "loop_shortcut_bounded_declare_progress",
            "9 cycles leave the proposer at 9 poison — one short of lethal, so it still scores"
        );
    }

    /// CR 732.2a — the BOUNDED branch, both halves, on ONE schema differing only in `n`.
    ///
    /// (i) `Fixed(4)` with `max_iterations == 10` ⇒ committed progress well inside every
    /// seat's headroom, so the critical band `PolicyVerdict::score` routes `8.0` to. (ii) `Fixed(11)` ⇒ the engine hands it back fail-closed with ZERO committed
    /// cycles and the CR 732.2b window spent, i.e. weakly dominated by declining.
    ///
    /// REVERT-PROBES, each flipping a DIFFERENT subset so neither dominates the other:
    /// * ⓟ1 restore `(_, IterationCount::Fixed(_)) => na()` ⇒ BOTH arms collapse to
    ///   `delta == 0.0` / `"loop_shortcut_na"` ⇒ FAILS.
    /// * ⓟ2 delete the `n > schema.max_iterations` conjunct ⇒ arm (ii) SCORES instead of
    ///   rejecting ⇒ FAILS while arm (i) still passes, which is what proves the reject half is
    ///   not carried by ⓟ1.
    /// * ⓟ3 invert `ShortcutDecisionSchema::is_bounded()` to `>=` ⇒ the in-test schema reads
    ///   unbounded ⇒ both arms take `na()` ⇒ FAILS, and so does
    ///   `loop_shortcut_unbounded_offer_keeps_fixed_neutral`. ⓟ3 flipping BOTH rows plus the
    ///   engine's `until_lethal_against_a_bounded_offer_is_rejected` is the single-authority
    ///   proof: one edit to one predicate is measurable at every caller.
    #[test]
    fn loop_shortcut_bounded_declare_scores_and_rejects_over_bound() {
        let state = bounded_offer_state(10);
        assert!(
            schema_of(&state).is_bounded(),
            "REACH-GUARD: every assertion below is vacuous unless the in-test schema really is \
             bounded; max_iterations = {}",
            schema_of(&state).max_iterations
        );

        // (i) within the bound.
        let inside = verdict_for(&state, &declare(IterationCount::Fixed(4)));
        assert!(
            matches!(inside, PolicyVerdict::Score { .. }),
            "a declare within the offered bound must SCORE, got {inside:?}"
        );
        assert_eq!(kind_of(&inside), "loop_shortcut_bounded_declare_progress");
        assert!(
            delta_of(&inside) > STRONG_MAX,
            "a bounded declare is board-deciding ⇒ the critical band; got {} (STRONG_MAX = \
             {STRONG_MAX})",
            delta_of(&inside)
        );

        // (ii) above the bound — the OPPOSITE verdict variant on the SAME schema.
        let outside = verdict_for(&state, &declare(IterationCount::Fixed(11)));
        assert!(
            matches!(outside, PolicyVerdict::Reject { .. }),
            "a declare above the offered bound contains a conditional action and is handed \
             back with zero committed cycles ⇒ weakly dominated, got {outside:?}"
        );
        assert_eq!(
            kind_of(&outside),
            "loop_shortcut_bounded_declare_over_bound"
        );
    }

    /// CR 732.2a — the ZERO-count arm asserted directly. A `Fixed(0)` declaration commits no
    /// cycles while still spending the CR 732.2b response window, so declining weakly
    /// dominates it and it must be rejected rather than scored.
    ///
    /// The arm previously carried no row at all: the suite covered `Fixed(4)` and `Fixed(11)`
    /// bounded and `Fixed(4)` unbounded, so deleting the `Fixed(0)` arm left every test green
    /// and it carried zero regression protection.
    ///
    /// REVERT-PROBE: delete the `(_, IterationCount::Fixed(0))` arm ⇒ `Fixed(0)` falls through
    /// to the in-bound progress arm ⇒ this row reads `Score` /
    /// `"loop_shortcut_bounded_declare_progress"` and FAILS on both assertions.
    #[test]
    fn loop_shortcut_bounded_declare_rejects_zero_count() {
        let state = bounded_offer_state(10);
        // REACH-GUARD: the zero arm sits BELOW the `!is_bounded() => na()` branch, so on an
        // unbounded schema this row would measure the neutral branch instead and pass for the
        // wrong reason.
        assert!(
            schema_of(&state).is_bounded(),
            "reach-guard: the zero arm is only reachable on a BOUNDED schema"
        );
        let v = verdict_for(&state, &declare(IterationCount::Fixed(0)));
        assert!(
            matches!(v, PolicyVerdict::Reject { .. }),
            "a zero-cycle declare commits nothing while spending the response window ⇒ weakly \
             dominated by declining, got {v:?}"
        );
        assert_eq!(kind_of(&v), "loop_shortcut_bounded_declare_zero_count");
    }

    /// CR 732.2a — the UNBOUNDED half of the same arm, which the row above structurally cannot
    /// see because it asserts a bounded schema as its reach-guard.
    ///
    /// The defect this pins was real and measured: with `(_, Fixed(0))` sitting BELOW
    /// `(_, Fixed(_)) if !schema.is_bounded() => na()`, a zero-count declare on an unbounded
    /// offer matched the `na()` guard first and scored NEUTRAL. The domination argument does
    /// not depend on boundedness — a declaration committing no cycles spends the CR 732.2b
    /// response window either way — so the two schema shapes must reach the same verdict.
    ///
    /// Driven across every `predicted_winner` because the arm binds `_` on that axis; if the
    /// reorder had accidentally been written as a winner-specific arm, only one of these three
    /// would pass.
    ///
    /// THE `Fixed(4)` CONTRAST IS LOAD-BEARING, not decoration. A "fix" that deleted the
    /// `!is_bounded() => na()` arm outright would satisfy the reject assertions above while
    /// silently re-scoping every unbounded `Fixed(n)` into the bounded scoring path. Asserting
    /// that a NON-zero count on the SAME state is still neutral is what distinguishes "the zero
    /// arm now precedes the neutral arm" from "the neutral arm is gone".
    ///
    /// REVERT-PROBE: move the `(_, IterationCount::Fixed(0))` arm back below the
    /// `!schema.is_bounded()` arm ⇒ `Fixed(0)` reaches `na()` ⇒ this row reads `"loop_shortcut_na"`
    /// with delta `0.0` and FAILS, while every bounded row stays green.
    ///
    /// On the reason string: it still reads `..._bounded_declare_zero_count` although the arm
    /// now covers both schema shapes. Left as-is deliberately — a `PolicyReason` kind is a
    /// stable identifier that AI-gate baselines key on, so renaming it is a separate change
    /// with its own blast radius, not a drive-by.
    #[test]
    fn loop_shortcut_unbounded_declare_rejects_zero_count() {
        for predicted_winner in [None, Some(P0), Some(P1)] {
            let state = offer_state(predicted_winner);
            assert!(
                !schema_of(&state).is_bounded(),
                "REACH-GUARD: this row is about the UNBOUNDED schema — the bounded row above \
                 already covers the other half; measured max_iterations {}",
                schema_of(&state).max_iterations
            );
            let zero = verdict_for(&state, &declare(IterationCount::Fixed(0)));
            assert!(
                matches!(zero, PolicyVerdict::Reject { .. }),
                "a zero-cycle declare commits nothing while spending the CR 732.2b window \
                 whether or not the offer is bounded (winner {predicted_winner:?}), got {zero:?}"
            );
            assert_eq!(kind_of(&zero), "loop_shortcut_bounded_declare_zero_count");

            let nonzero = verdict_for(&state, &declare(IterationCount::Fixed(4)));
            assert_eq!(
                kind_of(&nonzero),
                "loop_shortcut_na",
                "the neutral arm must survive the reorder — only ZERO is pulled ahead of it \
                 (winner {predicted_winner:?})"
            );
            assert_eq!(delta_of(&nonzero), 0.0);
        }
    }

    /// CR 732.2a — the `na()` branch asserted DIRECTLY rather than inferred from which arm
    /// happened to be reached. This row and the one above assert OPPOSITE outcomes for the
    /// SAME `Fixed(n)`, discriminated by ONE field of the schema: a constant-`na()`
    /// implementation fails the row above, a constant-`Score` implementation fails this one.
    ///
    /// REVERT-PROBES: ⓟ3 (invert `is_bounded()` to `>=`) ⇒ the default schema reads bounded ⇒
    /// this row gets `Score` / `"…_progress"` ⇒ FAILS. ⓟ4 delete the
    /// `!schema.is_bounded() => na()` branch ⇒ same failure, and `declare_fixed_is_never_rejected`
    /// fails with it.
    #[test]
    fn loop_shortcut_unbounded_offer_keeps_fixed_neutral() {
        for predicted_winner in [None, Some(P0), Some(P1)] {
            let state = offer_state(predicted_winner);
            assert!(
                !schema_of(&state).is_bounded(),
                "REACH-GUARD: this row asserts the UNBOUNDED branch, so the default schema must \
                 read unbounded — `ShortcutDecisionSchema::default()` carries \
                 `max_iterations == MAX_SHORTCUT_CYCLES`; measured {}",
                schema_of(&state).max_iterations
            );
            let v = verdict_for(&state, &declare(IterationCount::Fixed(4)));
            assert_eq!(
                delta_of(&v),
                0.0,
                "an offer stating NO CR 704 bound gives the domination argument nothing to \
                 stand on, so `Fixed(n)` is deliberately neutral (winner {predicted_winner:?})"
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
