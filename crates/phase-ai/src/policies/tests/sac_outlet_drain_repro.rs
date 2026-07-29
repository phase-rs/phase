//! End-to-end pin for the self-cost cost-vs-benefit authority: the AI must not
//! drain its own board into a repeatable "sacrifice another creature or
//! artifact: draw a card" outlet, **and must still crack cheap fodder that
//! genuinely pays for itself**.
//!
//! Models Baron Bertram Graywater's second ability (verified against
//! `data/card-data.json`: `{1}{B}, Sacrifice another creature or artifact:
//! Draw a card.`) as a hand-built `AbilityDefinition`, then runs the real AI
//! action loop (`auto_play::run_ai_actions`) at a main-phase priority window and
//! counts how many of the AI's own permanents end up in the graveyard.
//!
//! Three fodder classes — what the crack COSTS — each measured with search ON
//! and OFF:
//!
//! | fodder | cheapest `Another` match | vs `draw(1)` | expected |
//! |---|---|---|---|
//! | 1/1 creature tokens | `max(2.5, 0.5)` = **2.5** | 1.0 | net −1.5 → **veto** |
//! | 3/3 non-token bodies | **7.5** | 1.0 | net −6.5 → **veto** |
//! | Clues (non-creature artifact tokens) | `sacrifice_token_cost` = **0.5** | 1.0 | net +0.5 → **covers** |
//!
//! and a second, orthogonal axis — whether the crack BUYS anything
//! ([`DrawSuppression`]). A Clue crack pays for itself while the draw lands and
//! buys literally nothing once an opponent's Notion Thief takes the card
//! (CR 614.6), so the same 0.5 cost sits on both sides of the decision
//! depending on a permanent the AI does not control:
//!
//! | fodder | suppression | cost | payoff | expected |
//! |---|---|---|---|---|
//! | Clues | none | 0.5 | 1.0 | net +0.5 → **covers** |
//! | Clues | opposing Notion Thief | 0.5 | **0.0** | net −0.5 → **veto** |
//!
//! The unsuppressed Clue class is the **positive control**: a veto that leaked
//! into the covering class reads 0 activations there. It also tests the
//! fodder-exhaustion → commander boundary for the first time — after all five
//! Clues are cracked, the cheapest remaining `Another` match is the 4/4
//! commander at intrinsic 10.0 → net −9.0 → vetoed, so the drain stops at the
//! **economics boundary** rather than at fodder exhaustion.
//!
//! # OBSERVED, post-fix — all eight arms, one test-runner cycle
//!
//! | fodder | search | offered | activations | creatures | fodder left | commander |
//! |---|---|---|---|---|---|---|
//! | 1/1 tokens | on | 1 | **0** | 7 → 7 | 5 / 5 | safe |
//! | 1/1 tokens | off | 1 | **0** | 7 → 7 | 5 / 5 | safe |
//! | 3/3 non-token | on | 1 | **0** | 7 → 7 | 5 / 5 | safe |
//! | 3/3 non-token | off | 1 | **0** | 7 → 7 | 5 / 5 | safe |
//! | Clues | on | 1 | **≥ 5** | 2 → 2 | **0 / 5** | safe |
//! | Clues | off | 1 | **≥ 5** | 2 → 2 | **0 / 5** | safe |
//! | Clues + opposing thief | on | 1 | **0 suppressed** (raw count not claimed) | 2 → 2 | not claimed | safe |
//! | Clues + opposing thief | off | 1 | **0 suppressed** (raw count not claimed) | 2 → 2 | not claimed | safe |
//!
//! The Clue rows' `≥ 5` is *derived, not asserted*: nothing else on that board
//! sacrifices a permanent, so `fodder_left` falling 5 → 0 means at least five
//! outlet activations. It is recorded rather than asserted because the arm is
//! saturated (see the saturation table below) — `fodder_left == 0` IS asserted,
//! as the reach guard that makes the commander-boundary claim non-vacuous.
//!
//! The thief rows may crack Clues, and that is CORRECT: the suppressor does
//! leave the battlefield partway through the run (measured), and every crack
//! after that buys a real card. The asserted quantity there is therefore
//! `suppressed_activations` — cracks taken while the engine says the payoff will
//! not arrive AND the opposing suppressor is still on the battlefield — never
//! the raw count, which is left unclaimed precisely because it mixes the two
//! worlds. Pre-fix that number was **5 of 5**; post-fix it is **0**, with
//! `suppressed_windows >= 1` proving the opportunity was there.
//!
//! Both halves of that conjunct are load-bearing. A dead payoff on its own is
//! also what an empty library (CR 704.5b) or a `CantDraw` static produces, so
//! measuring it alone would let this arm keep its name while quietly certifying
//! "the AI declines to draw off an empty library" — MEASURED, on a probe build
//! with the AI library shrunk 30 -> 1 so the dead payoff has a NON-thief cause:
//! with no thief at all the payoff-only predicate counted 8 suppressed windows
//! while the conjunct counted 0, and on the thief board both counted 23.
//!
//! Read those four numbers precisely, because they are not all the same kind of
//! claim. The **8** is the measured content: a dead payoff really does arise on
//! this fixture family from a non-thief cause. The thiefless **0** is
//! STRUCTURAL, not evidence — `suppression=None` leaves `suppressor` as `None`,
//! so the conjunct is 0 by construction whatever the run does. But the
//! thief-board **23 == 23** IS a measurement, and it is the one that keeps this
//! probe honest: adding a conjunct can only remove windows, so the conjunct
//! reading 23 rather than 0 is what shows `suppressor_live` is not identically
//! false on a thief board — a mis-wired id or a wrong field would have read 0.
//! Divergence was possible (the suppressor does leave mid-run, measured above),
//! so equality is an observation, not a forced identity. What the probe does
//! NOT establish is the stronger claim "drops every false positive and no true
//! positive" in general; it establishes non-vacuity on this board.
//!
//! They are also NOT this arm's window counts. Both were taken under the
//! shrunk library. Under the shipped 30-card configuration the same thief board
//! measures `suppressed_windows` 57 (search on) / 61 (search off) — so the reach
//! guard is now non-vacuous on the tree that SHIPS, not merely on a probe build.
//! What moved the number is INFERRED, not re-measured: most likely the shrunk
//! library, since `build_board` seeds the AI library at one site shared by both
//! arms (so a 30 -> 1 probe edit necessarily applied to both), the run ended on
//! game-over rather than the 200-action cap at 126/132 actions, and Notion
//! Thief's `ExceptFirstDrawInDrawStep` exemption means the AI still consumes one
//! library card per turn while the thief is live — decking out early and
//! truncating the count. Nobody re-ran the shrunk library on the shipped tree to
//! reproduce 23. What IS measured is the NON-cause: a probe asserted the old
//! `battlefield`-vector predicate against an `obj.zone == Battlefield` one on
//! EVERY decision state of both runs and never fired, so the authority swap is
//! excluded. Note the shipped predicate is now the wider
//! `Battlefield | Command`, which that probe never ran; the two coincide here
//! only because nothing on this fixture ever occupies `Zone::Command`, argued
//! at the predicate itself rather than assumed here.
//!
//! Read the two blocks together: the veto is exact-zero on both underwater
//! classes in both search regimes, the covering class runs to completion, and
//! the commander survives even the run that exhausts its fodder.
//!
//! # HISTORY — the pre-fix baseline at `3047643e82` (two independent runs, identical)
//!
//! | fodder | search | offered | outlet activations | creatures |
//! |---|---|---|---|---|
//! | 1/1 tokens | on | 1 | **5** | 7 → 2 |
//! | 1/1 tokens | off | 1 | **5** | 7 → 2 |
//! | 3/3 non-token | on | 1 | **0** | 7 → 7 |
//! | 3/3 non-token | off | 1 | **0** | 7 → 7 |
//!
//! # HISTORY — the suppressed-payoff baseline, measured on THIS fixture
//!
//! Measured with the deliverability gate in `effect_benefit_value` temporarily
//! reverted (the arm priced every draw at a full card, thief or no thief):
//!
//! | fodder | suppression | search | offered | activations | **suppressed** | AI hand |
//! |---|---|---|---|---|---|---|
//! | Clues | opposing thief | on | 1 | 5 | **5** | 0 → 2 |
//! | Clues | opposing thief | off | 1 | 5 | **5** | 0 → 1 |
//!
//! i.e. pre-fix the AI cracked every Clue it had while an opponent's Notion
//! Thief was live and the engine had already certified the draw would not
//! arrive — in both search regimes, buying nothing with any of them. Post-fix
//! that count is 0: the Clues are only spent once the suppressor is gone and the
//! card actually lands. **The instrument is `suppressed_activations`, not the
//! raw count**: an earlier draft of this arm asserted `activations == 0`, and
//! that assertion is simply false about correct play, because the suppressor
//! leaves the battlefield partway through the run.
//!
//! The pre-fix root-verdict diff (kept because it names the defect that was
//! removed): `FreeOutletActivationPolicy` scored the token board **+0.5**
//! (`sac_outlet_effect_justified`) and hard-Rejected the 3/3 board
//! (`sac_outlet_too_expensive`) on a flat "cheapest creature sacrifice cost >
//! 4.0" cliff that never read the payoff's magnitude, while
//! `SelfCostValuePolicy` returned delta-0 `self_cost_benefit_present` on both.
//! So the reported drain was *actively rewarded*, not merely unpenalized. That
//! policy is now narrowed to its aristocrats name and opts out here entirely;
//! `SelfCostValuePolicy` is the single authority.
//!
//! Two arms of this file used to pin that defective baseline
//! (`ai_drains_its_own_board_into_a_repeatable_draw_outlet`,
//! `the_drain_is_a_policy_layer_decision_not_a_search_one`); they were rewritten
//! in place into the Tokens and Clues arms below rather than deleted, and the
//! baseline they pinned is the table above. Per house rule, no transient honest
//! red is ever pinned as expected.
//!
//! # Root-score decomposition — do not re-derive this wrong
//!
//! `PlannerServices::tactical_score` = `should_play_now_with_facts(..)` + the
//! policy-registry sum + a tactical-class adjustment. `ActivateAbility` matches
//! no explicit arm of `should_play_now_with_facts` and takes its `_ => 0.5`
//! non-spell catch-all (`card_hints.rs`), and `TacticalClass::Ability` matches no
//! class adjustment. So **root score = 0.5 + Σ scaled verdicts**, which is why
//! verdict sums of −1.15 / −5.25 were observed as root scores −0.65 / −4.75. Two
//! prior rounds of arithmetic were off by exactly this constant. It is
//! common-mode and moot under a veto (`-inf + 0.5 = -inf`).
//!
//! # The softmax law this fixture exists to enforce
//!
//! The final decision is a **sample**, not an argmax: `softmax_select_pairs` at
//! Medium `temperature = 1.0`, repeated over ~100–120 priority windows per run.
//! Measured on this fixture, repricing the token candidate from +0.85 to −0.65
//! cut P(activate) per window 63.9% → 28.3% — a real 2.3× improvement — and the
//! board drained **identically**, because P(≥1 selection over that many windows)
//! ≈ 1.0 either way. **A graduated penalty is a rate; a `Reject` is a bound; a
//! rate cannot enforce a bound over unbounded trials.** Only `-inf` is
//! categorical (softmax weight `exp(-inf) = 0`).
//!
//! # Saturation status — WHAT EACH ARM MAY AND MAY NOT CLAIM (binding)
//!
//! | arm | claim | why it is sound |
//! |---|---|---|
//! | Tokens ×2, RealBodies ×2 | **exact categorical zero** | a vetoed candidate has softmax weight exactly 0, so 0 is a *prediction*, not a ceiling. Any single activation falsifies it. |
//! | Clues ×2 | **direction only** (`>= 1`) | the fixture is SATURATED — it holds 5 fodder against ~100 windows, so its ceiling is the fodder count, not a behavioural equilibrium. No per-window rate or magnitude may be asserted from it. |
//! | Clues + opposing thief ×2 | **exact categorical zero, on `suppressed_activations` only** | same board, same saturation — but the CLAIM is a zero, and a zero cannot saturate. A vetoed candidate has softmax weight exactly 0, so one suppressed crack falsifies the arm. It may claim nothing about the raw activation count: the suppressor leaves mid-run and the cracks after that are correct. Paired non-vacuity probe: `suppressed_windows >= 1`. |
//!
//! Microscopic caveat on the "weight 0 ⇒ never sampled" claim, stated rather
//! than left implicit: `softmax_select_pairs` draws `threshold = rng * total`,
//! which can be exactly `0.0` with probability 2⁻⁵³, and the cumulative `>=`
//! scan would then select a *leading* weight-0 entry. The fixture runs a fixed
//! seed, so the exact-zero assertions are deterministic and stand.
//!
//! **If a future round ever returns this seam to a graduated restraint, a
//! non-saturated instrument (fodder ≫ windows, or a direct per-window rate
//! measure) must be built BEFORE any assertion is written.** A saturated
//! instrument cannot see a 2.3× improvement, which is exactly how a real
//! improvement was once mistaken for an inert change.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rand::rngs::SmallRng;
use rand::SeedableRng;

use engine::game::effects::draw::can_draw_at_least_one;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, DrawReplacementScope, Effect, FilterProp,
    QuantityExpr, ReplacementCondition, ReplacementDefinition, ReplacementPlayerScope,
    SacrificeCost, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use crate::auto_play::run_ai_actions;
use crate::config::{create_config, AiDifficulty, Platform};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);
const OPP: PlayerId = PlayerId(1);

/// Per-run card-id source. Deliberately NOT a process-global atomic: two arms
/// of the same fixture must build byte-identical boards regardless of how many
/// other arms ran first, or the measured activation counts are not comparable
/// across arms (and not reproducible across test-runner orderings).
struct Ids(u64);

impl Ids {
    fn new() -> Self {
        Self(7000)
    }

    fn next(&mut self) -> CardId {
        self.0 += 1;
        CardId(self.0)
    }
}

fn creature(
    state: &mut GameState,
    ids: &mut Ids,
    owner: PlayerId,
    name: &str,
    p: i32,
    t: i32,
) -> ObjectId {
    let id = create_object(
        state,
        ids.next(),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(p);
    obj.toughness = Some(t);
    obj.summoning_sick = false;
    id
}

/// A Clue-class permanent: a NON-creature artifact token. Legal fodder through
/// the outlet's artifact leg, and priced by the flat `sacrifice_token_cost`
/// (0.5) because `sacrifice_cost`'s token branch only consults creature stats
/// for creature tokens. Deliberately carries no P/T — a Clue with a body would
/// be priced as a creature and stop being the covering-class control.
fn clue(state: &mut GameState, ids: &mut Ids, name: &str) -> ObjectId {
    let id = create_object(state, ids.next(), AI, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.is_token = true;
    id
}

fn swamp(state: &mut GameState, ids: &mut Ids, owner: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        ids.next(),
        owner,
        "Swamp".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    Arc::make_mut(&mut obj.abilities).push({
        let mut a = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: engine::types::ability::ManaProduction::Fixed {
                    colors: vec![engine::types::mana::ManaColor::Black],
                    contribution: engine::types::ability::ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        );
        a.cost = Some(AbilityCost::Tap);
        a
    });
    id
}

/// `{1}{B}, Sacrifice another creature or artifact: Draw a card.`
fn baron_outlet_ability() -> AbilityDefinition {
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    // Cost AST copied verbatim from `data/card-data.json`
    // (`.["baron bertram graywater"].abilities[] | select(.kind=="Activated") | .cost`):
    // Composite[ Mana{1}{B}, Sacrifice{ Or[ Typed(Creature, [Another]),
    //                                       Typed(Artifact, [Another]) ], count 1 } ].
    ability.cost = Some(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Black],
                    generic: 1,
                },
            },
            AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(
                            TypedFilter::creature().properties(vec![FilterProp::Another]),
                        ),
                        TargetFilter::Typed(
                            TypedFilter::new(TypeFilter::Artifact)
                                .properties(vec![FilterProp::Another]),
                        ),
                    ],
                },
                1,
            )),
        ],
    });
    ability
}

/// Expendable permanents the AI still controls: everything on its battlefield
/// except the outlet itself, the commander, and its lands.
fn ai_fodder_count(state: &GameState, baron: ObjectId, commander: ObjectId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| **id != baron && **id != commander)
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                o.controller == AI && !o.card_types.core_types.contains(&CoreType::Land)
            })
        })
        .count()
}

fn ai_creature_count(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                o.controller == AI && o.card_types.core_types.contains(&CoreType::Creature)
            })
        })
        .count()
}

/// Which kind of expendable permanent fills the AI's board. The classes differ
/// ONLY in `sacrifice_cost`, which is the whole point — same outlet, same
/// payoff, same board size, three prices:
///
/// - `Tokens` — 1/1 creature tokens: `max(evaluate_creature_intrinsic(1,1) =
///   2.5, sacrifice_token_cost = 0.5) = 2.5` → underwater against `draw(1)`.
/// - `RealBodies` — 3/3 non-tokens: `evaluate_creature_intrinsic = 1.5*3 + 3 =
///   7.5` → deeply underwater.
/// - `Clues` — non-creature artifact tokens: the flat `sacrifice_token_cost =
///   0.5` → **covers** `draw(1)`. The positive control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fodder {
    Tokens,
    RealBodies,
    Clues,
}

/// Whether an OPPONENT is taking the outlet's payoff away.
///
/// The second axis of the board, orthogonal to [`Fodder`]: `Fodder` sets what
/// the crack COSTS, this sets whether it BUYS anything. The Clue class is the
/// only one where they interact visibly — a Clue crack pays for itself while the
/// draw lands, and buys literally nothing once an opponent's Notion Thief takes
/// the card, which is the reported bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawSuppression {
    None,
    /// Notion Thief on the OPPONENT's battlefield (CR 614.6): the AI's draw is
    /// replaced by "that player skips that draw and you draw a card", so each
    /// activation costs a permanent and `{1}{B}`, draws the AI nothing, and
    /// hands the opponent a card.
    OpposingNotionThief,
}

#[derive(Debug, Clone, Copy)]
struct Measurement {
    /// Non-vacuity probe: how many outlet activations the ENGINE offered.
    offered: usize,
    activations: usize,
    /// Outlet activations taken in a window where the engine certified the
    /// payoff would NOT arrive (`can_draw_at_least_one == false`) AND the
    /// opponent's suppressor was on the battlefield — both asked of the state
    /// the AI actually decided in.
    ///
    /// THE quantity a draw-suppression arm must measure, and the reason a raw
    /// `activations` count is not it. The suppressor can leave the battlefield
    /// mid-run — MEASURED: this fixture's thief did, in both search regimes —
    /// and every crack after that is CORRECT play, so an arm asserting
    /// `activations == 0` would be asserting that the AI must not do the right
    /// thing. This counter is blind to those and counts only the cracks made
    /// into a live suppressor.
    ///
    /// The suppressor conjunct is load-bearing, not belt-and-braces: a dead
    /// payoff alone is also produced by an empty library and by any `CantDraw`
    /// static, so without it this counter would name one cause and measure
    /// another. See the derivation at its computation site.
    suppressed_activations: usize,
    /// AI priority windows that had a live suppressor AND fodder still to crack
    /// — the opportunities to make the mistake.
    ///
    /// NON-VACUITY PROBE for [`Self::suppressed_activations`]: a zero there is
    /// only a claim about behaviour if the AI was actually offered the chance to
    /// make the mistake. Zero windows means the fixture never reached the
    /// condition under test.
    suppressed_windows: usize,
    before_creatures: usize,
    after_creatures: usize,
    before_hand: usize,
    after_hand: usize,
    /// Expendable fodder still on the battlefield afterwards — every AI
    /// permanent that is neither the outlet, the commander, nor a land.
    ///
    /// This is what makes the Clue arm's commander-boundary claim non-vacuous:
    /// `!commander_gone` proves nothing about fodder exhaustion unless the
    /// fodder was actually exhausted first. Without this field an arm that
    /// cracked one Clue and stopped would pass `activations >= 1` and
    /// `!commander_gone` while never reaching the boundary under test.
    fodder_left: usize,
    commander_gone: bool,
    /// Total actions the AI loop took before stopping.
    ///
    /// Second reach guard, one step beyond [`Self::fodder_left`]. Exhausting the
    /// fodder proves the boundary was *reached*; it does not prove the run
    /// *continued past* it. A batch that halted at `auto_play`'s
    /// `MAX_AI_ACTIONS_PER_SEQUENCE` cap on the very action that cracked the last
    /// Clue would satisfy `fodder_left == 0` while the AI was never offered the
    /// commander at all. Today the per-source cap of 4 pushes the fifth crack
    /// into a later turn and leaves many live windows, so that cannot happen —
    /// but that is circumstance, not a guard, and a future budget change would
    /// silently hollow out the commander assertions.
    actions_taken: usize,
}

/// Build the board and run the real AI action loop.
///
/// `search_enabled` is the discriminator that separates the two layers that can
/// decline this outlet: with search OFF the decision is made purely by the root
/// policy priors (`SelfCostValuePolicy` among them); with search ON the beam's
/// eval/board term and the in-search `SacrificeValuePolicy` prior also
/// participate.
fn run_outlet_repro(
    fodder: Fodder,
    suppression: DrawSuppression,
    search_enabled: bool,
) -> Measurement {
    let (mut state, baron, commander, suppressor) = build_board(fodder, suppression);
    state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: AI };

    let before_creatures = ai_creature_count(&state);
    let before_hand = state.players[AI.0 as usize].hand.len();

    // NON-VACUITY PROBE: does the engine even offer the outlet activation here?
    // Without this, "the AI passed" is indistinguishable from "the fixture never
    // presented the choice".
    let legal = engine::ai_support::legal_actions(&state);
    let offered = legal
        .iter()
        .filter(
            |a| matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == baron),
        )
        .count();

    let mut config = create_config(AiDifficulty::Medium, Platform::Native);
    config.search.enabled = search_enabled;
    let mut ai_players = HashSet::new();
    ai_players.insert(AI);
    // Both seats AI, so the activated ability actually resolves and priority
    // comes back — otherwise the batch halts at `NoActor` after one activation
    // and the `pending_activations` guard is mistaken for a value judgement.
    ai_players.insert(OPP);
    let mut configs = HashMap::new();
    configs.insert(AI, config.clone());
    configs.insert(OPP, config);
    let session = AiSession::arc_from_game(&state);
    let mut rng = SmallRng::seed_from_u64(9);

    // The pre-run state, kept so every action's PRE-state is available below:
    // `run_ai_actions` mutates `state` in place, so after the call `state` is
    // the final state and `results[i - 1].state` is action `i`'s pre-state — for
    // `i == 0` there would otherwise be nothing to look at.
    let initial = state.clone();
    let run = run_ai_actions(&mut state, &ai_players, &configs, &mut rng, &session);

    let pre_state = |i: usize| -> &GameState {
        if i == 0 {
            &initial
        } else {
            &run.results[i - 1].state
        }
    };
    let is_outlet = |action: &GameAction| matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == baron);

    let mut activations = 0;
    let mut suppressed_activations = 0;
    let mut suppressed_windows = 0;
    for (i, r) in run.results.iter().enumerate() {
        let pre = pre_state(i);
        // CR 614.6 / CR 704.5b: the engine's own verdict on whether the outlet's
        // draw would put a card in the AI's hand, asked of the state the AI
        // actually decided in.
        //
        // `payoff_dead` alone is a WEAKER property than this arm's name claims:
        // it is equally true when the AI's library has run out (CR 704.5b) or
        // under any future `CantDraw` on the board. Shrink the AI library,
        // lengthen the run, or make the thief enter later and every window's
        // `payoff_dead` would start coming from the empty library instead —
        // `suppressed_windows >= 1` and `suppressed_activations == 0` would both
        // still pass while the arm silently certified only "the AI declines to
        // draw off an empty library". The conjunct below pins the cause to the
        // OPPONENT'S SUPPRESSOR, which is what this arm is named for.
        //
        // This is a PER-WINDOW conjunct, and is deliberately NOT the deleted
        // `suppressors_left` final-state guard: the thief legitimately leaves
        // mid-run and cracks after it leaves are CORRECT play, so as an
        // end-of-run measure it was wrong and was removed. Asked of each
        // decision's own pre-state it is exactly right. Do not "simplify" it
        // back into a final-state count.
        // Zone set mirrors the production candidacy gate:
        // `object_replacement_candidate_applies` (replacement.rs:6168) tests
        // `zones_to_scan.contains(&obj.zone)` with
        // `zones_to_scan = [Zone::Battlefield, Zone::Command]` (:6210-6212), so
        // reading Battlefield alone would be NARROWER than the authority this
        // conjunct claims to mirror — and narrower under-counts
        // `suppressed_activations`, which is asserted == 0.
        //
        // Direction of the residual gap ON THE ZONE/LIMINAL AXIS: this
        // predicate is equal-or-BROADER than production, never narrower.
        // Production's zone term is
        // `!is_liminal_source && zones_to_scan.contains(&obj.zone)`, and the
        // `!is_liminal_source` conjunct narrows PRODUCTION, not this. The other
        // liminal path — resolving the subject through `state.liminal_entries`
        // before `state.objects` — is structurally unreachable for the event
        // this mirrors: `liminal_entry_ref` returns `Some` only for `TokenEntry`
        // and `ZoneChange { to: Zone::Battlefield }`, and `_ => None` covers
        // `ProposedEvent::Draw`.
        //
        // The universal is scoped to that axis deliberately. Candidacy is a
        // DISJUNCTION, and one other disjunct is live for a Draw:
        // `is_applicable_dredge` (replacement.rs, `object_replacement_candidate_applies`)
        // matches `ProposedEvent::Draw` explicitly and requires
        // `obj.zone == Zone::Graveyard`, so for a graveyard object carrying a
        // dredge value PRODUCTION is broader and this predicate narrower — the
        // direction the paragraph above rules out. Inert here: no object in this
        // fixture has a dredge value.
        //
        // Broader is the safe direction for the `suppressed_activations == 0`
        // assertion below, and the WEAKENING direction for its paired
        // `suppressed_windows >= 1` reach guard, which the same predicate gates.
        // (Both are cited by name rather than line: this block's line citations
        // have gone stale three rounds running, each time because an edit to the
        // block moved the targets it cites.) Measured delta on this fixture:
        // ZERO. Nothing here ever occupies `Zone::Command`, and the reason is
        // the FORMAT, not the board — `build_board` does set `is_commander` on
        // the AI's creature, but it builds through `FormatConfig::standard()`,
        // which sets `command_zone: false`, so `check_commander_zone_return`
        // (sba.rs) returns before selecting anything. The window guard measures
        // 57/61 against a threshold of 1. Re-check this if a fixture ever
        // enables `command_zone`, or otherwise puts a replacement source in the
        // command zone.
        let suppressor_live = suppressor.is_some_and(|id| {
            pre.objects
                .get(&id)
                .is_some_and(|object| matches!(object.zone, Zone::Battlefield | Zone::Command))
        });
        let payoff_dead = !can_draw_at_least_one(pre, AI);
        let suppressed = payoff_dead && suppressor_live;
        if suppressed
            && matches!(pre.waiting_for, WaitingFor::Priority { player } if player == AI)
            && ai_fodder_count(pre, baron, commander) > 0
        {
            suppressed_windows += 1;
        }
        if is_outlet(&r.action) {
            activations += 1;
            if suppressed {
                suppressed_activations += 1;
            }
        }
    }

    let measurement = Measurement {
        offered,
        activations,
        suppressed_activations,
        suppressed_windows,
        before_creatures,
        after_creatures: ai_creature_count(&state),
        before_hand,
        after_hand: state.players[AI.0 as usize].hand.len(),
        fodder_left: ai_fodder_count(&state, baron, commander),
        commander_gone: !state.battlefield.contains(&commander),
        actions_taken: run.results.len(),
    };

    eprintln!(
        "REPRO: fodder={fodder:?} suppression={suppression:?} search={search_enabled} \
         {measurement:?}"
    );

    measurement
}

/// The board under test: the outlet, a commander, five expendable bodies of the
/// requested kind, twelve Swamps, real libraries for both seats, and — under
/// [`DrawSuppression::OpposingNotionThief`] — an opposing thief taking the
/// payoff. Returns `(state, outlet_id, commander_id, suppressor_id)`.
///
/// The suppressor id is `Option` rather than a sentinel because the suppression
/// axis genuinely has a "no such object" case
/// ([`DrawSuppression::None`]) — the `Option` IS the axis, so a caller cannot
/// ask whether the suppressor is live without first handling its absence.
fn build_board(
    fodder: Fodder,
    suppression: DrawSuppression,
) -> (GameState, ObjectId, ObjectId, Option<ObjectId>) {
    let mut ids = Ids::new();
    let mut state = GameState::new_two_player(4242);
    state.phase = Phase::PreCombatMain;
    state.active_player = AI;

    // The outlet.
    let baron = creature(&mut state, &mut ids, AI, "Baron Bertram Graywater", 3, 4);
    Arc::make_mut(&mut state.objects.get_mut(&baron).unwrap().abilities)
        .push(baron_outlet_ability());

    // The commander — a 4/4 legendary body. It is legal `Another` fodder on
    // every board, so it is the permanent that gets consumed if the drain ever
    // runs past the cheap fodder. Its intrinsic price is 1.5*4 + 4 = 10.0.
    let commander = creature(&mut state, &mut ids, AI, "Legendary Commander", 4, 4);
    state.objects.get_mut(&commander).unwrap().is_commander = true;

    // Five expendable permanents of the requested class.
    for i in 0..5 {
        match fodder {
            Fodder::Tokens => {
                let id = creature(&mut state, &mut ids, AI, &format!("Body {i}"), 1, 1);
                state.objects.get_mut(&id).unwrap().is_token = true;
            }
            Fodder::RealBodies => {
                creature(&mut state, &mut ids, AI, &format!("Body {i}"), 3, 3);
            }
            Fodder::Clues => {
                clue(&mut state, &mut ids, &format!("Clue {i}"));
            }
        }
    }

    // Plenty of mana so mana is not the limiting resource.
    for _ in 0..12 {
        swamp(&mut state, &mut ids, AI);
    }

    // Library so draws are real.
    for i in 0..30 {
        create_object(
            &mut state,
            ids.next(),
            AI,
            format!("Card {i}"),
            Zone::Library,
        );
    }
    for i in 0..30 {
        create_object(
            &mut state,
            ids.next(),
            OPP,
            format!("Opp Card {i}"),
            Zone::Library,
        );
    }

    let suppressor = match suppression {
        DrawSuppression::None => None,
        DrawSuppression::OpposingNotionThief => {
            Some(add_opposing_notion_thief(&mut state, &mut ids))
        }
    };

    (state, baron, commander, suppressor)
}

/// Notion Thief under the OPPONENT: "If an opponent would draw a card except the
/// first one they draw in each of their draw steps, instead that player skips
/// that draw and you draw a card."
///
/// Rebuilt VERBATIM from the parsed shape in `data/card-data.json`
/// (`.["notion thief"].replacements[0]`): `event: Draw`, `mode: Mandatory`,
/// `valid_player: Opponent`, `condition: ExceptFirstDrawInDrawStep`,
/// `draw_scope: IndividualDraw`, and an `execute` whose head is the
/// `Unimplemented("draw")` gap node carrying the `Draw{1, Controller}`
/// sub-ability. That head is LOAD-BEARING — it is what makes the branch a
/// non-Draw substitution — and the engine is runtime-proven correct on this card
/// (`crates/engine/tests/integration/notion_thief_opponent_draw_redirect.rs`),
/// so it is reproduced here, never "fixed". Built through the single authority
/// `Effect::unimplemented`, not a hand-written literal.
///
/// The board runs in `PreCombatMain` (set by `build_board`), so the
/// `ExceptFirstDrawInDrawStep` condition — which exempts only the active
/// player's FIRST draw of their own draw step — never exempts these draws.
///
/// SCAFFOLDING, stated because it is a deliberate deviation from the printed
/// card: the body is granted indestructible. The printed 3/1 does not survive
/// this fixture's combat — MEASURED: it died in both search regimes, and every
/// activation the AI then made was made with `can_draw_at_least_one == true`,
/// i.e. correct play against a board that no longer had a thief on it. An arm
/// whose suppressor dies halfway is not an instrument for a suppression fix. The
/// body is not what this arm measures; the replacement is, and that is
/// reproduced verbatim.
///
/// Indestructible is necessary but NOT sufficient, and the fixture does not
/// pretend otherwise: even indestructible the thief leaves before the run ends.
/// That is why the enforcement is `Measurement::suppressed_activations` — a
/// per-decision measure — rather than any final-state count of suppressors.
fn add_opposing_notion_thief(state: &mut GameState, ids: &mut Ids) -> ObjectId {
    let id = create_object(
        state,
        ids.next(),
        OPP,
        "Notion Thief".to_string(),
        Zone::Battlefield,
    );
    let mut execute =
        AbilityDefinition::new(AbilityKind::Spell, Effect::unimplemented("draw", "draw"));
    execute.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )));
    let mut replacement = ReplacementDefinition::new(ReplacementEvent::Draw)
        .draw_scope(DrawReplacementScope::IndividualDraw)
        .execute(execute)
        .condition(ReplacementCondition::ExceptFirstDrawInDrawStep);
    replacement.valid_player = Some(ReplacementPlayerScope::Opponent);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(3);
    obj.toughness = Some(1);
    obj.summoning_sick = false;
    obj.keywords.push(Keyword::Indestructible);
    obj.replacement_definitions.push(replacement);
    id
}

#[test]
fn cost_vs_benefit_has_exactly_one_authority() {
    // DETERMINISTIC pin for the single-authority contract — the registry-level
    // statement that only ONE policy answers "is this sacrifice worth the
    // payoff?", and that it answers correctly on all three boards. This is the
    // one row in this file whose result does not depend on the softmax, so it
    // discriminates the DESIGN independently of the runtime arms.
    //
    // Failure images, each tripping a numbered assertion below:
    //   * crude gate restored     → (1) an extra rejecter on the 3/3 board, and
    //                               (2) a `FreeOutletActivation` entry;
    //   * veto reverted to a graduated score
    //                             → (1) red: ZERO rejecters on boards that must
    //                               have exactly one;
    //   * pricing reverted wholesale
    //                             → (3) red: kind is `self_cost_benefit_present`;
    //   * veto over-broadened     → (1) red on the Clue board, which must have
    //                               NO rejecter.
    //
    // The tapped-penalty revert is deliberately INVISIBLE here — every board in
    // this test is untapped, so that world is `tapped_fodder_still_prices_at_
    // full_body_value`'s job (unit level) and the Tokens runtime arm's (where
    // the fodder actually attacks and taps). Division of labor, stated so the
    // gap is not mistaken for an oversight.
    // Kind discrimination cannot be faked by delta-0 equality.
    for (fodder, suppression, expectation) in [
        (
            Fodder::Tokens,
            DrawSuppression::None,
            Expectation::Veto {
                cost_milli: 2500,
                benefit_milli: 1000,
            },
        ),
        (
            Fodder::RealBodies,
            DrawSuppression::None,
            Expectation::Veto {
                cost_milli: 7500,
                benefit_milli: 1000,
            },
        ),
        (
            Fodder::Clues,
            DrawSuppression::None,
            Expectation::Covers {
                cost_milli: 500,
                benefit_milli: 1000,
            },
        ),
        // The suppressed row: same Clue board, same 0.5 cost, payoff taken by an
        // opponent (CR 614.6) — so the SAME crack the row above certifies as
        // covering is certified losing here, by the same single authority. This
        // is the deterministic half of
        // `the_covers_class_stops_when_an_opponent_takes_the_payoff`: it pins
        // the VERDICT independently of the softmax, so a runtime arm that moves
        // for some other reason cannot be mistaken for a pricing failure (or
        // vice versa).
        (
            Fodder::Clues,
            DrawSuppression::OpposingNotionThief,
            Expectation::Veto {
                cost_milli: 500,
                benefit_milli: 0,
            },
        ),
    ] {
        let (mut state, baron, _commander, _suppressor) = build_board(fodder, suppression);
        state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: AI };

        // Reach-guard: the ability index is PROVEN, not assumed.
        assert_eq!(
            state.objects[&baron].abilities.len(),
            1,
            "the fixture's Baron must carry exactly one ability"
        );

        let candidate = engine::ai_support::CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: baron,
                ability_index: 0,
            },
            metadata: engine::ai_support::ActionMetadata::for_actor(
                Some(AI),
                engine::ai_support::TacticalClass::Ability,
            ),
        };
        let decision = engine::ai_support::AiDecisionContext {
            waiting_for: engine::types::game_state::WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let config = crate::config::AiConfig::default();
        let mut context = crate::context::AiContext::empty(&config.weights);
        context.session = Arc::new(AiSession::empty());
        context.player = AI;

        // A DEFAULT-features session (aristocrats commitment 0) — the
        // non-aristocrats regime the reported drain came from.
        let ctx = crate::policies::context::PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let verdicts = crate::policies::registry::PolicyRegistry::shared().verdicts(&ctx);

        // (1) The set of REJECTING policies is EXACTLY what the design says it
        //     is — one authority on the underwater boards, none on the covering
        //     board. An extra rejecter is a second authority returning; the
        //     step-0 census of all 31 verdicts (every other policy 0.0 on this
        //     shape) is what makes this a complete claim rather than a spot
        //     check.
        let rejecters: Vec<_> = verdicts
            .iter()
            .filter(|(_, v)| matches!(v, crate::policies::registry::PolicyVerdict::Reject { .. }))
            .map(|(id, _)| *id)
            .collect();
        match expectation {
            Expectation::Veto { .. } => assert_eq!(
                rejecters,
                vec![crate::policies::registry::PolicyId::SelfCostValue],
                "{fodder:?}: exactly ONE policy may veto a certified-losing \
                 trade, and it must be the priced authority"
            ),
            Expectation::Covers { .. } => assert!(
                rejecters.is_empty(),
                "{fodder:?}: a COVERING trade must not be vetoed by anyone; \
                 got {rejecters:?}"
            ),
        }

        // (2) `FreeOutletActivation` must not appear AT ALL: a non-aristocrats
        //     session drives its `activation()` to `None` and the registry skips
        //     it before `verdict` is ever called, pushing no entry.
        assert!(
            !verdicts.iter().any(|(id, _)| matches!(
                id,
                crate::policies::registry::PolicyId::FreeOutletActivation
            )),
            "FreeOutletActivation must opt out for a non-aristocrats deck ({fodder:?})"
        );

        // (3) `SelfCostValue` is the authority, and its verdict carries the
        //     arithmetic. A `Reject` has no delta, so the comparison is pinned
        //     through `PolicyReason::facts` (`(value * 1000.0) as i64`, hence
        //     exact). Its activation is unconditionally 1.0, so the registry
        //     reports the raw verdict unscaled.
        let (_, self_cost) = verdicts
            .iter()
            .find(|(id, _)| matches!(id, crate::policies::registry::PolicyId::SelfCostValue))
            .unwrap_or_else(|| panic!("SelfCostValue must price the outlet ({fodder:?})"));
        match (expectation, self_cost) {
            (
                Expectation::Veto {
                    cost_milli,
                    benefit_milli,
                },
                crate::policies::registry::PolicyVerdict::Reject { reason },
            ) => {
                assert_eq!(
                    reason.kind, "self_cost_benefit_underwater",
                    "the priced comparison must own this decision ({fodder:?})"
                );
                assert_eq!(
                    reason.facts,
                    vec![("cost_milli", cost_milli), ("benefit_milli", benefit_milli)],
                    "{fodder:?}/{suppression:?}: the veto must carry the comparison it certifies"
                );
            }
            (
                Expectation::Covers {
                    cost_milli,
                    benefit_milli,
                },
                crate::policies::registry::PolicyVerdict::Score { delta, reason },
            ) => {
                assert_eq!(
                    reason.kind, "self_cost_benefit_covers_cost",
                    "a Clue crack pays for itself and must be priced as covering ({fodder:?})"
                );
                assert_eq!(*delta, 0.0, "{fodder:?}: covers is neutral, not a bonus");
                assert_eq!(
                    reason.facts,
                    vec![("cost_milli", cost_milli), ("benefit_milli", benefit_milli)],
                );
            }
            (expected, got) => panic!(
                "{fodder:?}: expected {expected:?}, got {got:?}. A graduated \
                 revert reads Score/underwater here; a pricing revert reads \
                 Score/benefit_present."
            ),
        }
    }
}

/// What the single authority must say about a board, per fodder class. Pairs the
/// verdict SHAPE with the cost the comparison is required to have bound, so a
/// shape that is right for the wrong arithmetic still fails.
#[derive(Debug, Clone, Copy)]
enum Expectation {
    /// A certified-losing trade: `Reject`, kind `self_cost_benefit_underwater`.
    Veto { cost_milli: i64, benefit_milli: i64 },
    /// A trade that pays for itself: neutral `self_cost_benefit_covers_cost`.
    Covers { cost_milli: i64, benefit_milli: i64 },
}

/// Measure BOTH search states before asserting either, so one state's failure
/// cannot hide the other's number. The layer attribution needs both: with search
/// OFF the decision is made purely by the root policy priors; with search ON the
/// beam's eval and the in-search priors also participate. A restraint that only
/// holds in one of the two states is not a restraint.
fn measure_both_search_states(
    fodder: Fodder,
    suppression: DrawSuppression,
) -> Vec<(bool, Measurement)> {
    [true, false]
        .into_iter()
        .map(|search_enabled| {
            (
                search_enabled,
                run_outlet_repro(fodder, suppression, search_enabled),
            )
        })
        .collect()
}

/// Every arm must have been OFFERED the outlet, or its activation count is not a
/// decision and every assertion built on it is vacuous.
fn assert_offered(search_enabled: bool, m: &Measurement) {
    assert_eq!(
        m.offered, 1,
        "the engine must OFFER the outlet (search={search_enabled}), or this \
         arm measures a fixture failure rather than a decision"
    );
}

#[test]
fn token_fodder_board_is_never_drained_with_or_without_search() {
    // Was `ai_drains_its_own_board_into_a_repeatable_draw_outlet` — rewritten in
    // place; the defective baseline it pinned (5 activations, 7→2) is preserved
    // in the module-doc history table.
    //
    // 1/1 creature tokens price at max(2.5, 0.5) = 2.5 against draw(1) = 1.0 →
    // net -1.5, certified losing, in EVERY window, TAPPED OR NOT. The tapped
    // case matters and is not incidental: these tokens attack during the batch
    // and end up tapped, and under the old tapped-discounted give-up price that
    // repriced them to exactly 1.0 → net 0 → `covers_cost` — the escape hatch
    // the whole measured drain flowed through. `sacrifice_cost` is now
    // tap-invariant, so no window reaches that arm.
    //
    // EXACT CATEGORICAL ZERO, not a ceiling: a vetoed candidate has softmax
    // weight exactly 0, so any single activation falsifies the veto's mechanics
    // (or reveals a third path to the outlet) — see the saturation table in the
    // module docs.
    for (search_enabled, m) in measure_both_search_states(Fodder::Tokens, DrawSuppression::None) {
        assert_offered(search_enabled, &m);
        assert_eq!(
            m.activations, 0,
            "a certified-losing trade must never be taken (search={search_enabled})"
        );
        assert_eq!(
            m.after_creatures, m.before_creatures,
            "no body may be surrendered (search={search_enabled}): {}->{}",
            m.before_creatures, m.after_creatures
        );
        assert_eq!(
            m.fodder_left, 5,
            "all five tokens must survive (search={search_enabled})"
        );
        assert!(
            !m.commander_gone,
            "the commander must survive (search={search_enabled})"
        );
    }
}

#[test]
fn a_real_bodied_board_declines_the_outlet_with_or_without_search() {
    // The deep-shortfall sibling: 3/3 non-tokens at 7.5 vs draw(1) = 1.0 → net
    // -6.5. Paired with the Tokens arm above, the two pin that the veto is
    // DEPTH-INVARIANT end-to-end — a shallow -1.5 and a deep -6.5 share one
    // categorical fate, which is the runtime counterpart of
    // `underwater_veto_is_categorical_at_any_depth`.
    //
    // Historically this arm is where the graduated restraint failed hardest:
    // with search ON it measured 6 activations and a SACRIFICED COMMANDER, while
    // the search-OFF root argmax declined. That is the measurement that proved a
    // finite negative is not a bound under repeated softmax sampling.
    for (search_enabled, m) in measure_both_search_states(Fodder::RealBodies, DrawSuppression::None)
    {
        assert_offered(search_enabled, &m);
        assert_eq!(
            m.activations, 0,
            "a 3/3-bodied board must decline (search={search_enabled})"
        );
        assert_eq!(
            m.after_creatures, m.before_creatures,
            "no body may be surrendered (search={search_enabled})"
        );
        assert_eq!(
            m.fodder_left, 5,
            "all five bodies must survive (search={search_enabled})"
        );
        assert!(
            !m.commander_gone,
            "the commander must survive (search={search_enabled})"
        );
    }
}

#[test]
fn the_covers_class_still_activates_with_or_without_search() {
    // Was `the_drain_is_a_policy_layer_decision_not_a_search_one` — rewritten in
    // place into the POSITIVE CONTROL the redesign needs. Its old layer-
    // attribution role is not lost: every arm in this file now measures both
    // search states.
    //
    // Clues are non-creature artifact tokens: 0.5 against draw(1) = 1.0 → net
    // +0.5 → `covers_cost`, NOT vetoed. Cracking five Clues for five cards is
    // correct Magic, and a restraint that forbids it is broken in the
    // overreach direction. This is the arm that catches that.
    //
    // DIRECTION ONLY (`>= 1`). The fixture is saturated — 5 fodder against ~100
    // priority windows — so its ceiling is the fodder count, not an
    // equilibrium. No rate or magnitude may be read from it.
    //
    // It is ALSO the fodder-exhaustion → commander boundary, tested here for the
    // first time. After the Clues are gone the cheapest legal `Another` match is
    // the 4/4 commander at intrinsic 10.0 → net -9.0 → vetoed. So the drain
    // stops because the ECONOMICS say stop, not because it ran out of things to
    // eat. In the measured pre-veto world this exact scenario consumed the
    // commander on its 6th activation.
    for (search_enabled, m) in measure_both_search_states(Fodder::Clues, DrawSuppression::None) {
        assert_offered(search_enabled, &m);
        assert!(
            m.activations >= 1,
            "a covering trade must remain reachable end-to-end \
             (search={search_enabled}); got {} — a veto that leaked into the \
             covers class reads 0 here",
            m.activations
        );
        assert!(
            m.after_hand > m.before_hand,
            "the cracked Clues must have drawn cards (search={search_enabled}): \
             hand {}->{}",
            m.before_hand,
            m.after_hand
        );
        // REACH GUARD for the boundary claim below. `!commander_gone` proves
        // nothing about fodder exhaustion unless the fodder was exhausted: an
        // arm that cracked one Clue and stopped would satisfy every other
        // assertion here while never reaching the boundary under test. Requiring
        // ZERO Clues left forces the run past exhaustion, so the next two
        // assertions are about what the AI does when the only remaining legal
        // `Another` match is its own 4/4 commander.
        assert_eq!(
            m.fodder_left, 0,
            "reach guard (search={search_enabled}): every Clue must be cracked, \
             or the fodder-exhaustion boundary is never reached and the \
             commander assertions below are vacuous. Got {} left.",
            m.fodder_left
        );
        // SECOND REACH GUARD. `fodder_left == 0` proves the boundary was reached;
        // it does not prove the run continued past it. If the loop had halted at
        // `auto_play::MAX_AI_ACTIONS_PER_SEQUENCE` (200) on the action that
        // cracked the last Clue, the AI would never have been offered the
        // commander and every assertion below would pass vacuously.
        assert!(
            m.actions_taken < 200,
            "reach guard (search={search_enabled}): the batch must end naturally, \
             not at auto_play's action cap — a run that halted on the last Clue \
             crack never offers the commander, making the assertions below \
             vacuous. Took {} actions.",
            m.actions_taken
        );
        // The boundary. Only the Baron and the commander are creatures on this
        // board, so an unchanged creature count means no creature was ever
        // chosen while Clues were cheapest, AND none was chosen after they ran
        // out.
        assert_eq!(
            m.after_creatures, m.before_creatures,
            "no creature may be rolled into once the Clues are exhausted \
             (search={search_enabled}): {}->{}",
            m.before_creatures, m.after_creatures
        );
        assert!(
            !m.commander_gone,
            "the commander must survive fodder exhaustion (search={search_enabled})"
        );
    }
}

#[test]
fn the_covers_class_stops_when_an_opponent_takes_the_payoff() {
    // THE REPORTED BUG, end to end. Same Clue board as
    // `the_covers_class_still_activates_with_or_without_search` — its live
    // positive control, identical in every respect except the thief — so the
    // only difference between "crack all five" and "crack none" is an opponent
    // permanent that takes the card (CR 614.6: the replaced draw never happens).
    // Cracking here costs a permanent and {1}{B} per activation, draws the AI
    // nothing, and hands an opponent a card.
    //
    // WHAT IS ASSERTED, and why it is NOT the raw activation count. The measured
    // quantity is `suppressed_activations` — cracks made in a window where the
    // engine certified the payoff would not arrive. A raw `activations == 0`
    // would be WRONG here: the thief is a creature on a board that plays real
    // combat and it does leave, and once it is gone cracking Clues for cards is
    // CORRECT play. That is measurement, not speculation — the first draft of
    // this arm asserted `activations == 0`, went red at 5, and per-activation
    // instrumentation showed all 5 taken with `can_draw_at_least_one == true`,
    // i.e. after the suppressor was gone.
    //
    // MEASURED, both directions, on this exact fixture (not predicted):
    //   * pre-fix, gate reverted: **5 of 5** activations were made into a LIVE
    //     suppressor, in both search regimes (AI hand ended at 2 with search on,
    //     1 with it off);
    //   * post-fix: **0**, below. The raw activation count is deliberately NOT
    //     asserted — it mixes suppressed windows with the correct-play windows
    //     after the suppressor leaves.
    //
    // CLAIM CLASS — and why this arm escapes the saturation ceiling that binds
    // its thief-less control: that control may claim DIRECTION ONLY (`>= 1`,
    // 5 fodder against ~100 windows). This arm claims **exact categorical
    // zero**, which per the same binding table is a PREDICTION, not a ceiling —
    // a vetoed candidate has softmax weight exactly 0, so any single suppressed
    // activation falsifies it. Saturation cannot manufacture that pass.
    for (search_enabled, m) in
        measure_both_search_states(Fodder::Clues, DrawSuppression::OpposingNotionThief)
    {
        assert_offered(search_enabled, &m);
        // NON-VACUITY PROBE. The zero below is a statement about behaviour only
        // if the AI was actually offered the chance to make the mistake: its own
        // priority, fodder still on board, and the engine saying the payoff is
        // dead. Zero such windows would mean the fixture never reached the
        // condition under test.
        assert!(
            m.suppressed_windows >= 1,
            "reach guard (search={search_enabled}): the run must contain at \
             least one window with live suppression AND fodder left, or the \
             zero below is vacuous. Got {}.",
            m.suppressed_windows
        );
        assert_eq!(
            m.suppressed_activations, 0,
            "a crack whose payoff the engine says will not arrive must never be \
             taken (search={search_enabled}); {} of {} activations were made \
             into a live suppressor",
            m.suppressed_activations, m.activations
        );
        assert_eq!(
            m.after_creatures, m.before_creatures,
            "no body may be surrendered (search={search_enabled}): {}->{}",
            m.before_creatures, m.after_creatures
        );
        assert!(
            !m.commander_gone,
            "the commander must survive (search={search_enabled})"
        );
    }
}
